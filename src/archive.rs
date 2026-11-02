//! Catalog archives (design "Import" and "Export"): reading a `.tar.gz` or a
//! directory into stored files with the import rules, and writing a
//! `.tar.gz`. Pure: no database.
//!
//! Import rules for a tarball: only regular files become documents; the pax
//! global header `git archive` writes and directory entries are skipped; any
//! other entry type (symlink, hardlink, device, FIFO), a path that is not
//! UTF-8, holds a control character (NUL, which Postgres `text` cannot store,
//! newline, ...), is absolute, has a `..` component or is not in normal form,
//! a duplicate path, and a file that is also another file's directory (`a`
//! beside `a/b.md`) each refuse the whole import, and every problem is
//! listed. The uncompressed total is capped.
//!
//! A directory follows different rules, on purpose (design "Import", step 1):
//! it is read through M1's listing contract ([`DiskWorkspace`]'s
//! `list_files`), so it is never refused for its entries. A symlink that
//! resolves inside the catalog to a regular file is imported as that file's
//! bytes (a link is flattened); a dangling or outside symlink, a special file
//! (FIFO, socket, device) and a non-UTF-8 name (with everything below it) are
//! skipped with a `tracing` warning, not listed as problems. So
//! `import --replace` from a directory holding such entries drops or flattens
//! them where the same tree as a tarball would be refused. Only the size cap
//! refuses a directory import.

use std::collections::btree_map::Entry as MapEntry;
use std::collections::BTreeMap;
use std::fs::File;
use std::io::{self, Read, Write};
use std::path::Path;

use chrono::{DateTime, Utc};
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;
use flate2::Compression;
use tar::{Archive, Builder, EntryType, Header};

use crate::error::OkfError;
use crate::okf::paths::basename;
use crate::okf::workspace::{files_that_are_directories, DiskWorkspace, StoredFile, Workspace};

/// A catalog's files: catalog-relative path to file, sorted by path.
pub type Files = BTreeMap<String, StoredFile>;

/// One file to export.
// `Copy` is cheap here: the struct only borrows (`&'a`) the path and bytes,
// so copying it copies two pointers, never the content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExportFile<'a> {
    /// Catalog-relative path.
    pub path: &'a str,
    /// The exact bytes.
    pub content: &'a [u8],
    /// Written with mode 0755 when set, else 0644.
    pub executable: bool,
    /// The entry's modification time.
    pub modified: DateTime<Utc>,
}

/// Reads a `.tar.gz` for import.
///
/// # Arguments
/// * `input` - the gzip-compressed tar stream (`impl Read`: a file, stdin, a
///   byte slice).
/// * `max_bytes` - the largest uncompressed stream allowed (`max_import_bytes`).
///
/// # Returns
/// Every regular file, by path.
///
/// # Errors
/// [`OkfError::ImportRefused`] listing every rule violation, or saying the
/// input is not a readable `.tar.gz` or expands past `max_bytes`.
pub fn read_tarball(input: impl Read, max_bytes: u64) -> Result<Files, OkfError> {
    let mut archive = Archive::new(Capped {
        inner: GzDecoder::new(input),
        remaining: max_bytes,
        exceeded: false,
    });
    let result = read_entries(&mut archive);
    // Check the cap first: once it trips, every later read fails, and the
    // read error says less than this does.
    if archive.into_inner().exceeded {
        return Err(too_large(max_bytes));
    }
    result
}

/// Reads every entry, collecting rule violations instead of stopping at the first.
fn read_entries<R: Read>(archive: &mut Archive<R>) -> Result<Files, OkfError> {
    let mut files = Files::new();
    let mut problems = Vec::new();
    for entry in archive.entries().map_err(not_an_archive)? {
        let mut entry = entry.map_err(not_an_archive)?;
        match entry.header().entry_type() {
            // `git archive`'s pax global header (it holds the commit id) and
            // directory entries carry no file.
            EntryType::XGlobalHeader | EntryType::Directory => continue,
            EntryType::Regular => {}
            other => {
                problems.push(format!(
                    "{}: {other:?} entries are not allowed",
                    shown(&entry.path_bytes())
                ));
                continue;
            }
        }
        // `path_bytes` borrows the entry; `checked_path` copies the name out
        // into an owned `String`, so the borrow ends before `read_to_end`
        // needs the entry mutably.
        let path = match checked_path(&entry.path_bytes()) {
            Ok(path) => path,
            Err(problem) => {
                problems.push(problem);
                continue;
            }
        };
        let mode = entry.header().mode().map_err(not_an_archive)?;
        // `size` is the header's claim; it only sizes the buffer, and the cap
        // still bounds what is read.
        let capacity = usize::try_from(entry.size()).map_or(0, |size| size.min(1 << 20));
        let mut content = Vec::with_capacity(capacity);
        entry.read_to_end(&mut content).map_err(not_an_archive)?;
        let file = StoredFile {
            content,
            executable: mode & 0o111 != 0,
        };
        // `entry` API, Python's `setdefault`: one map lookup either way.
        match files.entry(path) {
            MapEntry::Vacant(slot) => {
                slot.insert(file);
            }
            MapEntry::Occupied(slot) => {
                problems.push(format!("{}: appears more than once", slot.key()));
            }
        }
    }
    problems.extend(
        files_that_are_directories(&files)
            .map(|path| format!("{path}: is a file and also the directory of another file")),
    );
    if problems.is_empty() {
        Ok(files)
    } else {
        Err(OkfError::ImportRefused { problems })
    }
}

/// A raw entry name for a message: lossy UTF-8 (bad bytes become U+FFFD),
/// with control characters escaped (`\0`, `\n`) so one problem stays one line.
fn shown(raw: &[u8]) -> String {
    // `escape_debug` is lazy (an iterator of chars); `to_string` collects it.
    String::from_utf8_lossy(raw).escape_debug().to_string()
}

/// A tar entry name as a catalog path, or the problem line saying why it
/// can't be one.
fn checked_path(raw: &[u8]) -> Result<String, String> {
    let reason = match std::str::from_utf8(raw) {
        Err(_) => "the name is not UTF-8",
        Ok("") => "the name is empty",
        // `char::is_control` as a predicate: Python's `any(c.iscntrl() ...)`.
        Ok(path) if path.contains(char::is_control) => "the name contains a control character",
        Ok(path) if path.starts_with('/') => "absolute paths are not allowed",
        Ok(path) if path.split('/').any(|part| part == "..") => "'..' components are not allowed",
        Ok(path) if path.split('/').any(|part| part.is_empty() || part == ".") => {
            "the path is not in normal form ('./', '//' or a trailing '/')"
        }
        Ok(path) => return Ok(path.to_string()),
    };
    Err(format!("{}: {reason}", shown(raw)))
}

/// Reads a catalog directory for import: every file M1's listing contract
/// returns (symlinks only inside the catalog), except `.git` (a directory or
/// a worktree's link file), `.worktrees/` and `__pycache__/`. Dotfiles are kept.
/// Unlike [`read_tarball`], entries the listing skips are not refused (see
/// the module documentation).
///
/// # Arguments
/// * `root` - the catalog directory (a checkout, say).
/// * `max_bytes` - the largest total content allowed (`max_import_bytes`).
///
/// # Returns
/// Every file, by path.
///
/// # Errors
/// [`OkfError::Io`] if the directory or a file can't be read;
/// [`OkfError::ImportRefused`] past `max_bytes`.
pub fn read_directory(root: &Path, max_bytes: u64) -> Result<Files, OkfError> {
    let workspace = DiskWorkspace::new(root)?;
    let mut files = Files::new();
    let mut remaining = max_bytes;
    // `list_files` already prunes `.git` and `.worktrees` directories (at any
    // depth); a `.git` *file* and `__pycache__` are dropped here.
    for path in workspace.list_files()? {
        if basename(&path) == ".git" || path.split('/').any(|part| part == "__pycache__") {
            continue;
        }
        let file = read_capped(&workspace.root().join(&path), remaining)
            .map_err(|source| OkfError::Io {
                path: path.clone(),
                source,
            })?
            .ok_or_else(|| too_large(max_bytes))?;
        // `read_capped` read at most `remaining` bytes, so this cannot underflow.
        remaining -= file.content.len() as u64;
        files.insert(path, file);
    }
    Ok(files)
}

/// Reads one file, holding at most `limit` bytes of it in memory.
///
/// # Returns
/// The file; `None` if it is longer than `limit`.
fn read_capped(path: &Path, limit: u64) -> io::Result<Option<StoredFile>> {
    let file = File::open(path)?;
    // The mode comes from the open handle, so it describes the file read.
    let executable = is_executable(&file.metadata()?);
    let mut content = Vec::new();
    // `take(limit + 1)` stops reading one byte past the limit: enough to know
    // the file is too long without reading the rest of it.
    file.take(limit.saturating_add(1))
        .read_to_end(&mut content)?;
    if content.len() as u64 > limit {
        return Ok(None);
    }
    Ok(Some(StoredFile {
        content,
        executable,
    }))
}

/// Does the file have any execute bit set?
#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    // `PermissionsExt` is the Unix-only extension that exposes the mode bits.
    use std::os::unix::fs::PermissionsExt;
    metadata.permissions().mode() & 0o111 != 0
}

/// Elsewhere there are no execute bits.
#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    false
}

/// Writes a `.tar.gz` of `files`, in the order given, with no top-level
/// directory, modes 0644/0755 and each file's modification time.
///
/// # Arguments
/// * `out` - where to write (`impl Write`: a file, stdout, a `Vec<u8>`).
/// * `files` - the files.
///
/// # Returns
/// `out`, after the gzip stream is finished.
///
/// # Errors
/// [`OkfError::Io`] if writing fails.
pub fn write_tarball<W: Write>(out: W, files: &[ExportFile<'_>]) -> Result<W, OkfError> {
    let mut builder = Builder::new(GzEncoder::new(out, Compression::default()));
    for file in files {
        let mut header = Header::new_ustar();
        header.set_entry_type(EntryType::Regular);
        header.set_size(file.content.len() as u64);
        header.set_mode(if file.executable { 0o755 } else { 0o644 });
        // Before 1970 cannot happen for a server-written time; 0 is a safe floor.
        header.set_mtime(u64::try_from(file.modified.timestamp()).unwrap_or(0));
        // `append_data` sets the path (adding a GNU long-name entry for a long
        // one) and the header checksum.
        builder
            .append_data(&mut header, file.path, file.content)
            .map_err(|source| export_error(file.path, source))?;
    }
    let encoder = builder
        .into_inner()
        .map_err(|source| export_error("<archive>", source))?;
    encoder
        .finish()
        .map_err(|source| export_error("<archive>", source))
}

/// A reader that fails once more than `remaining` bytes pass through it, and
/// remembers that it did.
struct Capped<R> {
    inner: R,
    remaining: u64,
    exceeded: bool,
}

impl<R: Read> Read for Capped<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let read = self.inner.read(buf)?;
        match self.remaining.checked_sub(read as u64) {
            Some(left) => {
                self.remaining = left;
                Ok(read)
            }
            None => {
                self.exceeded = true;
                Err(io::Error::other("uncompressed size limit exceeded"))
            }
        }
    }
}

/// The refusal for an import (tarball or directory) past `max_bytes`.
fn too_large(max_bytes: u64) -> OkfError {
    OkfError::ImportRefused {
        problems: vec![format!(
            "the import exceeds the {max_bytes}-byte limit (max_import_bytes)"
        )],
    }
}

/// The refusal for input that is not a `.tar.gz` (or is a corrupt one).
fn not_an_archive(error: io::Error) -> OkfError {
    OkfError::ImportRefused {
        problems: vec![format!("not a readable .tar.gz: {error}")],
    }
}

/// An export write failure, naming the entry being written.
fn export_error(path: &str, source: io::Error) -> OkfError {
    OkfError::Io {
        path: path.to_string(),
        source,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    const NO_LIMIT: u64 = u64::MAX;

    fn stored(content: &[u8], executable: bool) -> StoredFile {
        StoredFile {
            content: content.to_vec(),
            executable,
        }
    }

    fn modified() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap()
    }

    fn export<'a>(path: &'a str, content: &'a [u8], executable: bool) -> ExportFile<'a> {
        ExportFile {
            path,
            content,
            executable,
            modified: modified(),
        }
    }

    /// A tarball built entry by entry with raw header names, as a hostile or
    /// foreign tool might write it.
    fn raw_tarball(entries: &[(EntryType, &[u8], u32, &[u8])]) -> Vec<u8> {
        let mut builder = Builder::new(GzEncoder::new(Vec::new(), Compression::default()));
        for (entry_type, name, mode, content) in entries {
            let mut header = Header::new_ustar();
            header.set_entry_type(*entry_type);
            header.as_old_mut().name[..name.len()].copy_from_slice(name);
            header.set_size(content.len() as u64);
            header.set_mode(*mode);
            if matches!(entry_type, EntryType::Symlink | EntryType::Link) {
                header.set_link_name("index.md").unwrap();
            }
            header.set_cksum();
            builder.append(&header, *content).unwrap();
        }
        builder.into_inner().unwrap().finish().unwrap()
    }

    /// The problems of a refused import (panics on anything else).
    fn problems(result: Result<Files, OkfError>) -> Vec<String> {
        match result {
            Err(OkfError::ImportRefused { problems }) => problems,
            other => panic!("expected ImportRefused, got {other:?}"),
        }
    }

    #[test]
    fn an_export_reads_back_exactly() {
        let long = format!("{}/x.md", "d".repeat(300));
        let files = [
            export("index.md", b"# Index\n", false),
            export("tools/run.sh", b"#!/bin/sh\n\0\xff", true),
            export(&long, b"long", false),
        ];

        let tarball = write_tarball(Vec::new(), &files).unwrap();
        let read = read_tarball(tarball.as_slice(), NO_LIMIT).unwrap();

        assert_eq!(read.len(), 3);
        assert_eq!(read["index.md"], stored(b"# Index\n", false));
        assert_eq!(read["tools/run.sh"], stored(b"#!/bin/sh\n\0\xff", true));
        assert_eq!(read[long.as_str()], stored(b"long", false));
        // Export writes 0644/0755 and the documents' modification times.
        let mut archive = Archive::new(GzDecoder::new(tarball.as_slice()));
        let modes: Vec<(u32, u64)> = archive
            .entries()
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                (
                    entry.header().mode().unwrap(),
                    entry.header().mtime().unwrap(),
                )
            })
            .collect();
        let mtime = u64::try_from(modified().timestamp()).unwrap();
        assert_eq!(modes, [(0o644, mtime), (0o755, mtime), (0o644, mtime)]);
    }

    #[test]
    fn git_archive_extras_are_skipped_and_modes_decide_the_execute_bit() {
        let tarball = raw_tarball(&[
            (
                EntryType::XGlobalHeader,
                b"pax_global_header",
                0o666,
                b"52 comment=67ca473b\n",
            ),
            (EntryType::Directory, b"infra/", 0o775, b""),
            (EntryType::Regular, b"infra/run.sh", 0o775, b"#!/bin/sh\n"),
            (EntryType::Regular, b"infra/a.md", 0o664, b"# A\n"),
        ]);

        let read = read_tarball(tarball.as_slice(), NO_LIMIT).unwrap();

        assert_eq!(
            read.keys().collect::<Vec<_>>(),
            ["infra/a.md", "infra/run.sh"]
        );
        assert!(read["infra/run.sh"].executable);
        assert!(!read["infra/a.md"].executable);
    }

    #[test]
    fn every_bad_entry_is_listed_and_the_import_refused() {
        let tarball = raw_tarball(&[
            (EntryType::Symlink, b"link", 0o644, b""),
            (EntryType::Link, b"hard", 0o644, b""),
            (EntryType::Fifo, b"fifo", 0o644, b""),
            (EntryType::Char, b"tty", 0o644, b""),
            (EntryType::Regular, b"../escape", 0o644, b""),
            (EntryType::Regular, b"/abs", 0o644, b""),
            (EntryType::Regular, b"bad\xffname", 0o644, b""),
            (EntryType::Regular, b"./a", 0o644, b""),
            (EntryType::Regular, b"a//b", 0o644, b""),
            (EntryType::Regular, b"line\nbreak", 0o644, b""),
            (EntryType::Regular, b"ok.md", 0o644, b"x"),
            (EntryType::Regular, b"ok.md", 0o644, b"y"),
        ]);

        let problems = problems(read_tarball(tarball.as_slice(), NO_LIMIT));

        assert_eq!(problems.len(), 11, "{problems:#?}");
        assert_eq!(problems[0], "link: Symlink entries are not allowed");
        assert_eq!(problems[1], "hard: Link entries are not allowed");
        assert_eq!(problems[2], "fifo: Fifo entries are not allowed");
        assert_eq!(problems[3], "tty: Char entries are not allowed");
        assert!(problems[4].starts_with("../escape: '..'"));
        assert!(problems[5].starts_with("/abs: absolute"));
        assert!(problems[6].starts_with("bad\u{fffd}name: the name is not UTF-8"));
        assert!(problems[7].starts_with("./a: the path is not in normal form"));
        assert!(problems[8].starts_with("a//b: the path is not in normal form"));
        assert_eq!(
            problems[9],
            "line\\nbreak: the name contains a control character"
        );
        assert_eq!(problems[10], "ok.md: appears more than once");
    }

    #[test]
    fn a_nul_in_a_pax_path_is_listed_not_stored() {
        // A local pax header (`x`) whose `path` record carries a NUL, which a
        // ustar name field cannot. The record is "<length> path=<value>\n",
        // the length counting the whole record.
        let record = b"15 path=a\0b.md\n";
        assert_eq!(record.len(), 15);
        let tarball = raw_tarball(&[
            (EntryType::XHeader, b"PaxHeader", 0o644, record),
            (EntryType::Regular, b"placeholder", 0o644, b"x"),
        ]);

        let problems = problems(read_tarball(tarball.as_slice(), NO_LIMIT));

        assert_eq!(
            problems,
            ["a\\0b.md: the name contains a control character"]
        );
    }

    #[test]
    fn every_file_that_is_also_a_directory_is_listed() {
        let tarball = raw_tarball(&[
            (EntryType::Regular, b"a", 0o644, b"file"),
            (
                EntryType::Regular,
                b"a-b.md",
                0o644,
                b"sorts between a and a/",
            ),
            (EntryType::Regular, b"a/b.md", 0o644, b"below a"),
            (EntryType::Regular, b"a/b.md/c.md", 0o644, b"below a/b.md"),
            (EntryType::Regular, b"z.md", 0o644, b"fine"),
        ]);

        let problems = problems(read_tarball(tarball.as_slice(), NO_LIMIT));

        assert_eq!(
            problems,
            [
                "a: is a file and also the directory of another file",
                "a/b.md: is a file and also the directory of another file",
            ]
        );
    }

    #[test]
    fn the_cap_counts_uncompressed_bytes() {
        let big = vec![b'a'; 1024 * 1024];
        let tarball = write_tarball(Vec::new(), &[export("big.txt", &big, false)]).unwrap();
        assert!(tarball.len() < 16 * 1024, "compresses well");

        let problems = problems(read_tarball(tarball.as_slice(), 512 * 1024));

        assert_eq!(problems.len(), 1);
        assert!(problems[0].contains("524288-byte limit"), "{problems:?}");
        assert!(read_tarball(tarball.as_slice(), 4 * 1024 * 1024).is_ok());
    }

    #[test]
    fn garbage_is_not_an_archive() {
        let problems = problems(read_tarball(&b"not a tarball"[..], NO_LIMIT));

        assert_eq!(problems.len(), 1);
        assert!(
            problems[0].starts_with("not a readable .tar.gz"),
            "{problems:?}"
        );
    }

    #[cfg(unix)]
    #[test]
    fn a_directory_reads_every_file_but_git_and_caches() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let write = |path: &str, content: &[u8]| {
            let full = root.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, content).unwrap();
        };
        write("index.md", b"# Index\n");
        write("tools/run.sh", b"#!/bin/sh\n");
        write("img/logo.png", b"\x89PNG\0\xff");
        write(".claude/skills/s/SKILL.md", b"skill");
        write(".gitattributes", b"log.md merge=union\n");
        write(".git/config", b"[core]");
        write("sub/.git", b"gitdir: elsewhere");
        write("tools/__pycache__/okf.cpython-314.pyc", b"\0");
        write(".worktrees/s1/index.md", b"a session");
        let executable = std::fs::Permissions::from_mode(0o755);
        std::fs::set_permissions(root.join("tools/run.sh"), executable).unwrap();

        let read = read_directory(root, NO_LIMIT).unwrap();

        assert_eq!(
            read.keys().collect::<Vec<_>>(),
            [
                ".claude/skills/s/SKILL.md",
                ".gitattributes",
                "img/logo.png",
                "index.md",
                "tools/run.sh"
            ]
        );
        assert!(read["tools/run.sh"].executable);
        assert!(!read["index.md"].executable);
        assert_eq!(read["img/logo.png"].content, b"\x89PNG\0\xff");
        assert_eq!(
            problems(read_directory(root, 8)),
            ["the import exceeds the 8-byte limit (max_import_bytes)"]
        );
    }

    #[test]
    fn a_directory_exactly_at_the_cap_is_read() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.md"), b"1234").unwrap();
        std::fs::write(dir.path().join("b.md"), b"5678").unwrap();

        assert_eq!(read_directory(dir.path(), 8).unwrap().len(), 2);
        assert!(matches!(
            read_directory(dir.path(), 7),
            Err(OkfError::ImportRefused { .. })
        ));
    }
}

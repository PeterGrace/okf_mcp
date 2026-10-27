//! The filesystem a catalog lives in, behind a trait so the OKF logic can be
//! tested against an in-memory catalog.
//!
//! Paths are bundle-relative (`infra/time-server.md`). Implementations must
//! refuse paths that escape the catalog root.

use std::collections::BTreeMap;
use std::io::Write;
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use crate::error::OkfError;
use crate::okf::paths::{dirname, escapes_root, normpath, walk_order};
use crate::okf::pystr::universal_newlines;

/// Read and write access to one catalog's files.
///
/// `Send + Sync` so a workspace can be shared across tokio tasks.
pub trait Workspace: Send + Sync {
    /// Reads a file as UTF-8.
    ///
    /// # Errors
    /// [`OkfError::Io`] if the file is missing, unreadable or not UTF-8;
    /// [`OkfError::InvalidPath`] if the path escapes the catalog.
    fn read_to_string(&self, path: &str) -> Result<String, OkfError>;

    /// Reads a file as UTF-8 text for markdown analysis (headings, links,
    /// footnotes, anchors), with `\r\n` and lone `\r` turned into `\n`.
    ///
    /// This is how `okf.py` reads files for analysis (Python text mode,
    /// universal newlines; see [`universal_newlines`]), so `## Closing ##\r\n`
    /// is the heading `Closing` here too. Frontmatter parsing and editing
    /// ([`crate::okf::doc::Doc`]) must use [`Workspace::read_to_string`]
    /// instead, which keeps every byte.
    ///
    /// A file without `\r` is returned as read, with no extra allocation.
    ///
    /// # Errors
    /// As [`Workspace::read_to_string`].
    fn read_text(&self, path: &str) -> Result<String, OkfError> {
        let raw = self.read_to_string(path)?;
        // `Cow::Borrowed` means nothing changed: hand back `raw` itself. The
        // borrow of `raw` ends with this `match`, so it can then be moved.
        let normalized = match universal_newlines(&raw) {
            std::borrow::Cow::Owned(text) => Some(text),
            std::borrow::Cow::Borrowed(_) => None,
        };
        Ok(normalized.unwrap_or(raw))
    }

    /// Writes a file atomically (all or nothing), creating parent directories.
    ///
    /// On disk, as with `okf.py`'s `open(path, "w")`, an existing file keeps
    /// its permissions and a new one gets 0o666 less the process umask.
    ///
    /// # Errors
    /// [`OkfError::Io`] on any filesystem failure; [`OkfError::InvalidPath`]
    /// if the path escapes the catalog or is not a file path (the root).
    fn write_atomic(&self, path: &str, contents: &str) -> Result<(), OkfError>;

    /// Is there a file or directory at `path`?
    fn exists(&self, path: &str) -> bool {
        self.is_file(path) || self.is_dir(path)
    }

    /// Is there a regular file at `path`?
    fn is_file(&self, path: &str) -> bool;

    /// Is there a directory at `path` (`""` and `.` are the root)?
    fn is_dir(&self, path: &str) -> bool;

    /// Every `.md` file, skipping dot-directories, in `os.walk` order
    /// (see [`walk_order`]). Every returned path is readable through this
    /// workspace.
    ///
    /// # Errors
    /// [`OkfError::Io`] if a directory can't be listed.
    fn list_markdown(&self) -> Result<Vec<String>, OkfError>;
}

/// The mode requested for a newly created file: 0o666, which the kernel
/// masks with the process umask, as for Python's `open(path, "w")`.
#[cfg(unix)]
fn new_file_permissions() -> std::fs::Permissions {
    // `PermissionsExt` is a Unix-only extension trait adding `from_mode`.
    use std::os::unix::fs::PermissionsExt;
    std::fs::Permissions::from_mode(0o666)
}

/// Normalizes a bundle-relative path and refuses one that leaves the bundle.
fn checked(path: &str) -> Result<String, OkfError> {
    let normalized = normpath(path.trim_start_matches('/'));
    if escapes_root(&normalized) {
        return Err(OkfError::InvalidPath {
            path: path.to_string(),
            reason: "outside the catalog".to_string(),
        });
    }
    Ok(normalized)
}

/// Like [`checked`], but also refuses the catalog root itself, which can
/// never be a file. Every write goes through this.
fn checked_file(path: &str) -> Result<String, OkfError> {
    let normalized = checked(path)?;
    if normalized == "." {
        return Err(OkfError::InvalidPath {
            path: path.to_string(),
            reason: "not a file path".to_string(),
        });
    }
    Ok(normalized)
}

/// Is any directory component hidden (starts with `.`)?
fn in_dot_directory(path: &str) -> bool {
    dirname(path).split('/').any(|p| p.starts_with('.'))
}

/// Where a path lands once symlinks are resolved.
enum Resolution {
    /// Nothing there (or a dangling link): the caller's usual behavior applies.
    Missing,
    /// Resolves to somewhere under the catalog root.
    Inside,
    /// Resolves outside the catalog root, through a symlink.
    Outside,
}

/// A catalog on disk, rooted at a directory.
///
/// Containment policy: the root is canonicalized once in [`DiskWorkspace::new`].
/// Every read and predicate canonicalizes its target and refuses (or reports
/// `false` for) anything that resolves outside the root, so a committed
/// symlink cannot expose `/etc`. Writes canonicalize the deepest existing
/// ancestor of the target before creating anything, and refuse to replace a
/// symlink. Symlinked directories are never traversed by `list_markdown`.
///
/// Listing contract: `list_markdown` returns only paths readable through this
/// workspace. A symlink is listed only if it resolves inside the root to a
/// regular file; dangling and outside links are skipped with a warning. This
/// deliberately differs from `okf.py`'s `os.walk`, which lists them.
///
/// Accepted gap: the check and the use are separate steps, so a symlink swapped
/// in between could win the race. That is accepted provided the caller
/// serializes writes and git operations on a worktree. That serialization is
/// planned for M3 (the spec's write and publish flow) and is not enforced here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskWorkspace {
    /// The canonical (symlink-free, absolute) root.
    root: PathBuf,
}

impl DiskWorkspace {
    /// Creates a workspace rooted at `root`.
    ///
    /// `impl Into<PathBuf>` accepts anything convertible to a path (`&str`,
    /// `String`, `&Path`, `PathBuf`), like a Python function taking `str | Path`.
    ///
    /// # Errors
    /// [`OkfError::Io`] if `root` does not exist or can't be canonicalized.
    pub fn new(root: impl Into<PathBuf>) -> Result<Self, OkfError> {
        let root = root.into();
        let canonical = root.canonicalize().map_err(|source| OkfError::Io {
            path: root.display().to_string(),
            source,
        })?;
        Ok(Self { root: canonical })
    }

    /// The canonical root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    /// The lexical on-disk path for a bundle-relative path (no symlink checks).
    fn full(&self, path: &str) -> Result<PathBuf, OkfError> {
        let rel = checked(path)?;
        Ok(if rel == "." {
            self.root.clone()
        } else {
            self.root.join(rel)
        })
    }

    /// Resolves symlinks in `full` and classifies the result.
    fn resolve(&self, full: &Path) -> Resolution {
        match full.canonicalize() {
            Err(_) => Resolution::Missing,
            Ok(real) if real.starts_with(&self.root) => Resolution::Inside,
            Ok(_) => Resolution::Outside,
        }
    }

    /// The full path if it exists inside the catalog, else `None`.
    fn existing_inside(&self, path: &str) -> Option<PathBuf> {
        let full = self.full(path).ok()?;
        matches!(self.resolve(&full), Resolution::Inside).then_some(full)
    }

    /// Recursively lists `.md` files under `dir`, whose catalog-relative name is `prefix`.
    fn collect(&self, dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<(), OkfError> {
        let io_err = |source| OkfError::Io {
            path: prefix.to_string(),
            source,
        };
        for entry in std::fs::read_dir(dir).map_err(io_err)? {
            let entry = entry.map_err(io_err)?;
            let file_name = entry.file_name();
            let Some(name) = file_name.to_str() else {
                tracing::warn!(?file_name, "skipping non-UTF-8 file name");
                continue;
            };
            let rel = if prefix.is_empty() {
                name.to_string()
            } else {
                format!("{prefix}/{name}")
            };
            // `file_type` does not follow symlinks, so a link to a directory
            // is not a directory here and is never descended into.
            let file_type = entry.file_type().map_err(io_err)?;
            if file_type.is_dir() {
                if !name.starts_with('.') {
                    self.collect(&entry.path(), &rel, out)?;
                }
            } else if name.ends_with(".md") {
                if file_type.is_symlink() {
                    let path = entry.path();
                    let skip_reason = match self.resolve(&path) {
                        Resolution::Missing => Some("dangling symlink"),
                        Resolution::Outside => Some("symlink resolves outside the catalog"),
                        Resolution::Inside if !path.is_file() => Some("symlink to a non-file"),
                        Resolution::Inside => None,
                    };
                    if let Some(reason) = skip_reason {
                        tracing::warn!(path = %rel, reason, "not listing symlink");
                        continue;
                    }
                }
                out.push(rel);
            }
        }
        Ok(())
    }
}

impl Workspace for DiskWorkspace {
    fn read_to_string(&self, path: &str) -> Result<String, OkfError> {
        let full = self.full(path)?;
        if matches!(self.resolve(&full), Resolution::Outside) {
            return Err(OkfError::InvalidPath {
                path: path.to_string(),
                reason: "resolves outside the catalog (symlink)".to_string(),
            });
        }
        std::fs::read_to_string(full).map_err(|source| OkfError::Io {
            path: path.to_string(),
            source,
        })
    }

    fn write_atomic(&self, path: &str, contents: &str) -> Result<(), OkfError> {
        let rel = checked_file(path)?;
        let target = self.root.join(rel);
        let io_err = |source| OkfError::Io {
            path: path.to_string(),
            source,
        };
        let refuse = |reason: &str| OkfError::InvalidPath {
            path: path.to_string(),
            reason: reason.to_string(),
        };
        // A file path always has a parent (at worst the root).
        let parent = target
            .parent()
            .expect("a path below the catalog root has a parent");
        // Walk up to the deepest directory that exists and make sure it is
        // really inside the catalog, before creating anything.
        let mut ancestor = parent;
        while !ancestor.exists() {
            ancestor = ancestor
                .parent()
                .expect("the catalog root exists, so the walk stops there");
        }
        let real_ancestor = ancestor.canonicalize().map_err(io_err)?;
        if !real_ancestor.starts_with(&self.root) {
            return Err(refuse("resolves outside the catalog (symlink)"));
        }
        // Never replace a symlink silently.
        if std::fs::symlink_metadata(&target).is_ok_and(|m| m.file_type().is_symlink()) {
            return Err(refuse("is a symlink; refusing to replace it"));
        }
        // Components missing under the checked ancestor cannot exist, unless
        // one is a dangling symlink (canonicalize failed on it).
        let mut probe = ancestor.to_path_buf();
        for component in parent.strip_prefix(ancestor).into_iter().flatten() {
            probe.push(component);
            if std::fs::symlink_metadata(&probe).is_ok() {
                return Err(refuse("dangling symlink in path"));
            }
        }
        std::fs::create_dir_all(parent).map_err(io_err)?;
        // Write a uniquely named sibling temp file, then rename over the target:
        // a reader sees the old file or the new one, never half of it, and
        // concurrent writers never share a temp file. The temp file is deleted
        // when `tmp` is dropped, and Rust runs `Drop` on every early return
        // (like a `with` block's `__exit__`), so no failure path leaks it.
        //
        // Permissions match Python's `open(path, "w")`, which rewrites in place:
        // an existing file keeps its mode, and a new one gets 0o666 minus the
        // umask. `tempfile` alone would create the file at 0o600.
        let existing = std::fs::metadata(&target).ok().map(|m| m.permissions());
        let mut builder = tempfile::Builder::new();
        builder.prefix(".okf-").suffix(".tmp");
        // (Other platforms have only a read-only flag, and new files are writable.)
        #[cfg(unix)]
        if existing.is_none() {
            // The mode passed to open(2) is masked by the umask in the kernel,
            // so there is no need to read the umask (std has no safe getter).
            builder.permissions(new_file_permissions());
        }
        let mut tmp = builder.tempfile_in(parent).map_err(io_err)?;
        if let Some(permissions) = existing {
            // fchmod(2) on the open file: not masked by the umask.
            tmp.as_file().set_permissions(permissions).map_err(io_err)?;
        }
        tmp.write_all(contents.as_bytes()).map_err(io_err)?;
        // Durable before the rename: XFS/btrfs can otherwise leave an empty
        // file after a crash.
        tmp.as_file().sync_all().map_err(io_err)?;
        tmp.persist(&target).map_err(|e| io_err(e.error))?;
        Ok(())
    }

    fn exists(&self, path: &str) -> bool {
        self.existing_inside(path).is_some_and(|p| p.exists())
    }

    fn is_file(&self, path: &str) -> bool {
        // `is_ok_and`/`is_some_and` run the closure only on `Ok`/`Some`.
        self.existing_inside(path).is_some_and(|p| p.is_file())
    }

    fn is_dir(&self, path: &str) -> bool {
        self.existing_inside(path).is_some_and(|p| p.is_dir())
    }

    fn list_markdown(&self) -> Result<Vec<String>, OkfError> {
        let mut out = Vec::new();
        self.collect(&self.root, "", &mut out)?;
        out.sort_by(|a, b| walk_order(a, b));
        Ok(out)
    }
}

/// Is there any key starting with `prefix`? One ordered-map probe, O(log n).
fn has_key_with_prefix(files: &BTreeMap<String, String>, prefix: &str) -> bool {
    files
        .range::<str, _>((Bound::Included(prefix), Bound::Unbounded))
        .next()
        .is_some_and(|(key, _)| key.starts_with(prefix))
}

/// An in-memory catalog for tests. Directories exist implicitly as prefixes
/// of file paths.
#[derive(Debug, Default)]
pub struct MemWorkspace {
    // `RwLock` gives interior mutability: `write_atomic` takes `&self` (like the
    // disk version) yet can still change the map, safely across threads.
    files: RwLock<BTreeMap<String, String>>,
}

impl MemWorkspace {
    /// Builds a workspace from `(path, contents)` pairs.
    ///
    /// # Panics
    /// If a path escapes the catalog (a bad test fixture).
    pub fn from_files<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let map = files
            .into_iter()
            .map(|(p, c)| {
                let rel = checked_file(p).expect("fixture path must be inside the catalog");
                (rel, c.to_string())
            })
            .collect();
        Self {
            files: RwLock::new(map),
        }
    }

    fn files(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<String, String>> {
        // A poisoned lock means a test panicked mid-write; propagating the panic is right.
        self.files.read().expect("MemWorkspace lock poisoned")
    }
}

impl Workspace for MemWorkspace {
    fn read_to_string(&self, path: &str) -> Result<String, OkfError> {
        let rel = checked(path)?;
        self.files().get(&rel).cloned().ok_or_else(|| OkfError::Io {
            path: path.to_string(),
            source: std::io::Error::new(std::io::ErrorKind::NotFound, "no such file"),
        })
    }

    fn write_atomic(&self, path: &str, contents: &str) -> Result<(), OkfError> {
        let rel = checked_file(path)?;
        let mut files = self.files.write().expect("MemWorkspace lock poisoned");
        let dir_prefix = format!("{rel}/");
        let conflict = if has_key_with_prefix(&files, &dir_prefix) {
            Some("is a directory")
        } else {
            // Any proper prefix of the path that is a file blocks the write.
            rel.match_indices('/')
                .any(|(i, _)| files.contains_key(&rel[..i]))
                .then_some("a parent component is a file")
        };
        if let Some(reason) = conflict {
            return Err(OkfError::InvalidPath {
                path: path.to_string(),
                reason: reason.to_string(),
            });
        }
        files.insert(rel, contents.to_string());
        Ok(())
    }

    fn is_file(&self, path: &str) -> bool {
        checked(path).is_ok_and(|rel| self.files().contains_key(&rel))
    }

    fn is_dir(&self, path: &str) -> bool {
        let Ok(rel) = checked(path) else { return false };
        if rel == "." {
            return true;
        }
        has_key_with_prefix(&self.files(), &format!("{rel}/"))
    }

    fn list_markdown(&self) -> Result<Vec<String>, OkfError> {
        let mut out: Vec<String> = self
            .files()
            .keys()
            .filter(|k| k.ends_with(".md") && !in_dot_directory(k))
            .cloned()
            .collect();
        out.sort_by(|a, b| walk_order(a, b));
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const FILES: [(&str, &str); 7] = [
        ("index.md", "root"),
        ("b.md", "b"),
        ("infra/index.md", "infra"),
        ("infra/hosts/x.md", "x"),
        (".claude/skill.md", "hidden"),
        ("notes.txt", "not markdown"),
        ("crlf.txt", "a\r\nb\rc\n"),
    ];

    /// A temp dir holding a `catalog` subdirectory filled with `files`, so a
    /// stray write outside the root shows up as an extra entry in the temp dir.
    fn disk_catalog(files: &[(&str, &str)]) -> (tempfile::TempDir, DiskWorkspace) {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("catalog");
        for (path, contents) in files {
            let full = root.join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, contents).unwrap();
        }
        let ws = DiskWorkspace::new(&root).unwrap();
        (dir, ws)
    }

    /// Names of `.okf-*` temp files directly inside `dir` or its subdirectories.
    fn leftover_temp_files(dir: &Path) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for entry in std::fs::read_dir(dir).unwrap().map(|e| e.unwrap()) {
            if entry.file_name().to_string_lossy().starts_with(".okf-") {
                found.push(entry.path());
            } else if entry.file_type().unwrap().is_dir() {
                found.extend(leftover_temp_files(&entry.path()));
            }
        }
        found
    }

    fn assert_contract(ws: &dyn Workspace) {
        assert_eq!(
            ws.list_markdown().unwrap(),
            ["b.md", "index.md", "infra/index.md", "infra/hosts/x.md"]
        );
        assert_eq!(ws.read_to_string("/infra/index.md").unwrap(), "infra");
        assert_eq!(ws.read_to_string("crlf.txt").unwrap(), "a\r\nb\rc\n");
        assert_eq!(ws.read_text("crlf.txt").unwrap(), "a\nb\nc\n");
        assert_eq!(
            ws.read_text("b.md").unwrap(),
            ws.read_to_string("b.md").unwrap()
        );
        assert!(matches!(
            ws.read_text("../x"),
            Err(OkfError::InvalidPath { .. })
        ));
        assert!(ws.is_file("b.md") && !ws.is_dir("b.md"));
        assert!(ws.is_dir("infra") && ws.is_dir("") && !ws.is_file("infra"));
        assert!(!ws.exists("missing.md"));
        assert!(matches!(
            ws.read_to_string("../x"),
            Err(OkfError::InvalidPath { .. })
        ));
        assert!(matches!(
            ws.read_to_string("missing.md"),
            Err(OkfError::Io { .. })
        ));
        ws.write_atomic("new/dir/c.md", "c").unwrap();
        assert_eq!(ws.read_to_string("new/dir/c.md").unwrap(), "c");
        ws.write_atomic("b.md", "b2").unwrap();
        assert_eq!(ws.read_to_string("b.md").unwrap(), "b2");
        assert!(ws.write_atomic("../escape.md", "x").is_err());
        for root_like in ["", ".", "a/.."] {
            assert!(
                matches!(
                    ws.write_atomic(root_like, "x"),
                    Err(OkfError::InvalidPath { .. })
                ),
                "{root_like:?}"
            );
        }
        // A directory is not a file, and a file is not a directory.
        assert!(ws.write_atomic("infra", "x").is_err());
        assert!(ws.write_atomic("b.md/c.md", "x").is_err());
    }

    #[test]
    fn mem_workspace_contract() {
        assert_contract(&MemWorkspace::from_files(FILES));
    }

    #[test]
    fn disk_workspace_contract() {
        let (dir, ws) = disk_catalog(&FILES);
        assert_contract(&ws);
        assert!(leftover_temp_files(ws.root()).is_empty());
        // Nothing appeared next to the catalog root.
        let siblings: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(siblings, ["catalog"]);
    }

    #[test]
    fn new_fails_for_a_missing_root() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            DiskWorkspace::new(dir.path().join("nope")),
            Err(OkfError::Io { .. })
        ));
    }

    #[test]
    fn concurrent_writes_to_one_file_never_corrupt_it() {
        let (_dir, ws) = disk_catalog(&FILES);
        let payload_a = "A".repeat(1_000_000);
        let payload_b = "B".repeat(1_000_000);
        std::thread::scope(|scope| {
            for payload in [&payload_a, &payload_b] {
                let ws = &ws;
                scope.spawn(move || {
                    for _ in 0..50 {
                        ws.write_atomic("race.md", payload).unwrap();
                    }
                });
            }
        });
        let result = ws.read_to_string("race.md").unwrap();
        assert!(result == payload_a || result == payload_b);
        assert!(leftover_temp_files(ws.root()).is_empty());
    }

    #[test]
    fn failed_write_leaves_no_temp_file() {
        let (_dir, ws) = disk_catalog(&FILES);
        assert!(ws.write_atomic("infra", "x").is_err());
        assert!(leftover_temp_files(ws.root()).is_empty());
    }

    #[cfg(unix)]
    mod modes {
        use std::os::unix::fs::PermissionsExt;

        use super::*;

        fn mode(path: &Path) -> u32 {
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777
        }

        #[test]
        fn rewriting_a_file_keeps_its_mode() {
            let (_dir, ws) = disk_catalog(&FILES);
            let target = ws.root().join("b.md");
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o640)).unwrap();
            ws.write_atomic("b.md", "new").unwrap();
            assert_eq!(mode(&target), 0o640);
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
        }

        #[test]
        fn a_new_file_gets_the_umask_mode_like_python_open() {
            let (dir, ws) = disk_catalog(&FILES);
            // `File::create` asks for 0o666 and the kernel applies the umask,
            // exactly as Python's `open(path, "w")` does.
            let reference = dir.path().join("reference");
            std::fs::File::create(&reference).unwrap();
            ws.write_atomic("fresh.md", "x").unwrap();
            assert_eq!(mode(&ws.root().join("fresh.md")), mode(&reference));
            ws.write_atomic("new/dir/deep.md", "x").unwrap();
            assert_eq!(mode(&ws.root().join("new/dir/deep.md")), mode(&reference));
        }
    }

    #[cfg(unix)]
    mod symlinks {
        use std::os::unix::fs::symlink;

        use super::*;

        /// A catalog plus a sibling `outside` directory holding `secret.md`.
        fn with_outside() -> (tempfile::TempDir, DiskWorkspace, PathBuf) {
            let (dir, ws) = disk_catalog(&FILES);
            let outside = dir.path().join("outside");
            std::fs::create_dir(&outside).unwrap();
            std::fs::write(outside.join("secret.md"), "secret").unwrap();
            (dir, ws, outside)
        }

        #[test]
        fn read_through_symlinked_directory_is_refused() {
            let (_dir, ws, outside) = with_outside();
            symlink(&outside, ws.root().join("evil")).unwrap();
            assert!(matches!(
                ws.read_to_string("evil/secret.md"),
                Err(OkfError::InvalidPath { .. })
            ));
            assert!(!ws.is_file("evil/secret.md"));
            assert!(!ws.is_dir("evil"));
            assert!(!ws.exists("evil/secret.md"));
        }

        #[test]
        fn write_through_symlinked_directory_is_refused() {
            let (_dir, ws, outside) = with_outside();
            symlink(&outside, ws.root().join("out")).unwrap();
            assert!(ws.write_atomic("out/x.md", "x").is_err());
            assert!(ws.write_atomic("out/new/x.md", "x").is_err());
            assert!(!outside.join("x.md").exists());
            assert!(!outside.join("new").exists());
            assert!(leftover_temp_files(&outside).is_empty());
        }

        #[test]
        fn write_never_replaces_a_symlink() {
            let (_dir, ws, _outside) = with_outside();
            symlink(ws.root().join("b.md"), ws.root().join("link.md")).unwrap();
            assert!(ws.write_atomic("link.md", "x").is_err());
            assert_eq!(ws.read_to_string("b.md").unwrap(), "b");
        }

        #[test]
        fn list_markdown_does_not_traverse_symlinked_directories() {
            let (_dir, ws, outside) = with_outside();
            symlink(&outside, ws.root().join("evil")).unwrap();
            symlink(&outside, ws.root().join("dirlink.md")).unwrap();
            let listed = ws.list_markdown().unwrap();
            assert!(!listed.iter().any(|p| p.contains("secret")));
            assert!(!listed.contains(&"dirlink.md".to_string()));
        }

        #[test]
        fn listing_skips_dangling_and_outside_file_links() {
            let (_dir, ws, outside) = with_outside();
            symlink(ws.root().join("gone.md"), ws.root().join("dangling.md")).unwrap();
            symlink(outside.join("secret.md"), ws.root().join("leak.md")).unwrap();
            symlink(ws.root().join("b.md"), ws.root().join("ok.md")).unwrap();
            let listed = ws.list_markdown().unwrap();
            assert!(listed.contains(&"ok.md".to_string()));
            assert!(!listed.contains(&"dangling.md".to_string()));
            assert!(!listed.contains(&"leak.md".to_string()));
            for path in &listed {
                assert!(ws.read_to_string(path).is_ok(), "{path}");
            }
        }

        #[test]
        fn write_through_dangling_symlink_directory_says_so() {
            let (_dir, ws, outside) = with_outside();
            symlink(outside.join("missing"), ws.root().join("dang")).unwrap();
            let err = ws.write_atomic("dang/x.md", "x").unwrap_err();
            assert!(
                err.to_string().contains("dangling symlink in path"),
                "{err}"
            );
            assert!(!outside.join("missing").exists());
        }

        #[test]
        fn symlink_to_a_file_inside_the_catalog_is_readable() {
            let (_dir, ws, _outside) = with_outside();
            symlink(ws.root().join("b.md"), ws.root().join("alias.md")).unwrap();
            assert_eq!(ws.read_to_string("alias.md").unwrap(), "b");
            assert!(ws
                .list_markdown()
                .unwrap()
                .contains(&"alias.md".to_string()));
        }
    }
}

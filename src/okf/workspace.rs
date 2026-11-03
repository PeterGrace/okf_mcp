//! The filesystem a catalog lives in, behind a trait so the OKF logic can be
//! tested against an in-memory catalog.
//!
//! Paths are bundle-relative (`infra/time-server.md`). Implementations must
//! refuse paths that escape the catalog root.

use std::borrow::Cow;
use std::collections::BTreeMap;
use std::io::Write;
use std::ops::Bound;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use crate::error::OkfError;
use crate::okf::paths::{escapes_root, normpath, walk_order, SKIP_DIRS, SKIP_TOP};
use crate::okf::pystr::universal_newlines;

/// Read and write access to one catalog's files.
///
/// `Send + Sync` so a workspace can be shared across threads and async tasks.
pub trait Workspace: Send + Sync {
    /// Reads a file as UTF-8.
    ///
    /// # Errors
    /// [`OkfError::NotFound`] if there is no file at `path` (every
    /// implementation reports a missing file this way, never as `Io`);
    /// [`OkfError::Io`] if it is unreadable or not UTF-8;
    /// [`OkfError::InvalidPath`] if the path escapes the catalog.
    fn read_to_string(&self, path: &str) -> Result<String, OkfError>;

    /// Reads a file as UTF-8 text for markdown analysis (headings, links,
    /// footnotes, anchors), with `\r\n` and lone `\r` turned into `\n`.
    ///
    /// This is how `okf.py` reads files for analysis (Python text mode,
    /// universal newlines), so `## Closing ##\r\n` is the heading `Closing`
    /// here too. Frontmatter parsing and editing ([`crate::okf::doc::Doc`])
    /// must use [`Workspace::read_to_string`] instead, which keeps every byte.
    ///
    /// A file without `\r` is returned as read, with no extra allocation.
    ///
    /// # Errors
    /// As [`Workspace::read_to_string`].
    fn read_text(&self, path: &str) -> Result<String, OkfError> {
        let raw = self.read_to_string(path)?;
        match universal_newlines(&raw) {
            Cow::Owned(text) => Ok(text),
            Cow::Borrowed(_) => Ok(raw),
        }
    }

    /// Writes a file atomically (all or nothing), creating parent directories.
    ///
    /// On disk, as with `okf.py`'s `open(path, "w")`, an existing file keeps
    /// its permissions, one this process may not open for writing is refused
    /// (the kernel decides, as for Python), and a new one gets 0o666 less the
    /// process umask. Unlike Python's in-place rewrite, replacing by rename
    /// does not keep the owner, group, ACLs, extended attributes or hard links
    /// of an existing file.
    ///
    /// # Errors
    /// [`OkfError::Io`] on any filesystem failure (`PermissionDenied` for a
    /// file this process may not write); [`OkfError::InvalidPath`] if the
    /// path escapes the catalog, is not a file path (the root), is a
    /// directory, a symlink or another non-regular file (FIFO, socket,
    /// device), or has a file as a parent component.
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

    /// Every regular file of any type, skipping only directories named
    /// `.git` or `.worktrees` (at any depth), in `os.walk` order (see
    /// [`walk_order`]). This is `okf.py` `lint`'s secret-scan walk: unlike
    /// [`Workspace::list_markdown`], other dot-directories (`.claude`) are
    /// included. Only directories are pruned, so a `.git` *file* (the link a
    /// git worktree holds) is listed, as `os.walk` lists it. The same symlink
    /// rules apply: every returned path resolves to a regular file inside
    /// this workspace.
    ///
    /// A listed file's content may not be UTF-8 (images, other binaries), so
    /// [`Workspace::read_to_string`] and [`Workspace::read_text`] can still
    /// fail on it with [`OkfError::Io`]. Callers must tolerate that; the
    /// secret scan skips such a file, as `okf.py` skips `UnicodeDecodeError`.
    ///
    /// # Errors
    /// [`OkfError::Io`] if a directory can't be listed.
    fn list_files(&self) -> Result<Vec<String>, OkfError>;
}

/// The mode requested for a newly created file: 0o666, which the kernel
/// masks with the process umask, as for Python's `open(path, "w")`.
#[cfg(unix)]
pub(crate) fn new_file_permissions() -> std::fs::Permissions {
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

/// Where a path lands once symlinks are resolved.
enum Resolution {
    /// Nothing there (or a dangling link): the caller's usual behavior applies.
    Missing,
    /// Resolves to somewhere under the catalog root.
    Inside,
    /// Resolves outside the catalog root, through a symlink.
    Outside,
}

/// Which files a directory walk returns.
// `Copy` makes this enum a plain value (like an int): passing it to the
// recursive `collect` copies it instead of moving or borrowing it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Listing {
    /// `.md` files outside dot-directories, [`SKIP_DIRS`] at any depth and
    /// [`SKIP_TOP`] at the root (`okf.py`'s `walk_md`).
    Markdown,
    /// Every file outside `.git` and `.worktrees` directories (`lint`'s secret walk).
    AllFiles,
}

impl Listing {
    /// Does the walk descend into a directory with this name? `at_root` is
    /// true for a directory directly under the catalog root.
    fn descends_into(self, dir_name: &str, at_root: bool) -> bool {
        match self {
            Listing::Markdown => {
                !dir_name.starts_with('.')
                    && !SKIP_DIRS.contains(&dir_name)
                    && !(at_root && SKIP_TOP.contains(&dir_name))
            }
            Listing::AllFiles => !matches!(dir_name, ".git" | ".worktrees"),
        }
    }

    /// Does the walk list the file at `path` (bundle-relative), given that
    /// it exists? Checks every directory component and the file name.
    fn lists(self, path: &str) -> bool {
        // `rsplit_once` splits at the last `/`, like `path.rpartition("/")`.
        let Some((dir, name)) = path.rsplit_once('/') else {
            return self.includes(path);
        };
        self.includes(name)
            && dir
                .split('/')
                .enumerate()
                .all(|(depth, dir_name)| self.descends_into(dir_name, depth == 0))
    }

    /// Does the walk list a file with this name?
    fn includes(self, file_name: &str) -> bool {
        match self {
            Listing::Markdown => file_name.ends_with(".md"),
            Listing::AllFiles => true,
        }
    }
}

/// A catalog on disk, rooted at a directory.
///
/// Containment policy: the root is canonicalized once in [`DiskWorkspace::new`].
/// Every read and predicate canonicalizes its target and refuses (or reports
/// `false` for) anything that resolves outside the root, so a committed
/// symlink cannot expose `/etc`. Writes canonicalize the deepest existing
/// ancestor of the target before creating anything, and refuse to replace a
/// symlink. Symlinked directories are never traversed by `list_markdown` or
/// `list_files`.
///
/// Listing contract: `list_markdown` and `list_files` return only paths
/// readable through this workspace. A symlink is listed only if it resolves
/// inside the root to a regular file; dangling and outside links are skipped
/// with a warning. This deliberately differs from `okf.py`'s `os.walk`, which
/// lists them. Special files (FIFOs, sockets, devices) are never listed: a
/// read would block or fail on them.
///
/// Accepted gap: an entry whose name is not valid UTF-8 is skipped with a
/// warning (paths here are `String`s), and so is everything below a directory
/// with such a name. `os.walk` lists them, so `okf.py`'s secret scan reads such
/// a file and this port's does not.
///
/// Accepted gap: the check and the use are separate steps, so a symlink swapped
/// in between could win the race. That is accepted provided the caller
/// does not write the same files concurrently. The server never writes through
/// a `DiskWorkspace` (it stores catalogs in Postgres); import only reads one.
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

    /// Recursively lists the files `listing` selects under `dir`, whose
    /// catalog-relative name is `prefix`.
    fn collect(
        &self,
        listing: Listing,
        dir: &Path,
        prefix: &str,
        out: &mut Vec<String>,
    ) -> Result<(), OkfError> {
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
                if listing.descends_into(name, prefix.is_empty()) {
                    self.collect(listing, &entry.path(), &rel, out)?;
                }
                continue;
            }
            if !listing.includes(name) {
                continue;
            }
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
            } else if !file_type.is_file() {
                // Not a directory, symlink or regular file: a FIFO, socket or
                // device, which a read would block on or fail.
                tracing::warn!(path = %rel, "not listing a special file (FIFO, socket or device)");
                continue;
            }
            out.push(rel);
        }
        Ok(())
    }

    /// Runs one walk from the root and sorts it into `os.walk` order.
    fn listing(&self, listing: Listing) -> Result<Vec<String>, OkfError> {
        let mut out = Vec::new();
        self.collect(listing, &self.root, "", &mut out)?;
        out.sort_by(|a, b| walk_order(a, b));
        Ok(out)
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
        std::fs::read_to_string(full).map_err(|source| match source.kind() {
            std::io::ErrorKind::NotFound => OkfError::NotFound {
                path: path.to_string(),
            },
            _ => OkfError::Io {
                path: path.to_string(),
                source,
            },
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
        // Never replace a symlink silently, and check this first so nothing
        // below stats through one (`symlink_metadata` does not follow links).
        let target_type = std::fs::symlink_metadata(&target)
            .ok()
            .map(|m| m.file_type());
        if target_type.is_some_and(|t| t.is_symlink()) {
            return Err(refuse("is a symlink; refusing to replace it"));
        }
        // The same refusals as `MemWorkspace`, before touching the disk.
        if !ancestor.is_dir() {
            return Err(refuse("a parent component is a file"));
        }
        if target_type.is_some_and(|t| t.is_dir()) {
            return Err(refuse("is a directory"));
        }
        // A FIFO, socket or device would be silently replaced by the rename.
        if target_type.is_some_and(|t| !t.is_file()) {
            return Err(refuse("not a regular file"));
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
        let existing = std::fs::metadata(&target).ok().filter(|m| m.is_file());
        if existing.is_some() {
            // Python's `open(path, "w")` fails when this process may not write
            // the file; a rename would silently replace it. Ask the kernel the
            // same question (open for writing, no create, no truncate), so
            // ownership, ACLs and root all count as they do for Python.
            std::fs::OpenOptions::new()
                .write(true)
                .open(&target)
                .map_err(io_err)?;
        }
        let existing = existing.map(|m| m.permissions());
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
        // `existing_inside` already requires the path to exist.
        self.existing_inside(path).is_some()
    }

    fn is_file(&self, path: &str) -> bool {
        // `is_ok_and`/`is_some_and` run the closure only on `Ok`/`Some`.
        self.existing_inside(path).is_some_and(|p| p.is_file())
    }

    fn is_dir(&self, path: &str) -> bool {
        self.existing_inside(path).is_some_and(|p| p.is_dir())
    }

    fn list_markdown(&self) -> Result<Vec<String>, OkfError> {
        self.listing(Listing::Markdown)
    }

    fn list_files(&self) -> Result<Vec<String>, OkfError> {
        self.listing(Listing::AllFiles)
    }
}

/// Is there any key starting with `prefix`? One ordered-map probe, O(log n).
fn has_key_with_prefix(files: &BTreeMap<String, StoredFile>, prefix: &str) -> bool {
    files
        .range::<str, _>((Bound::Included(prefix), Bound::Unbounded))
        .next()
        .is_some_and(|(key, _)| key.starts_with(prefix))
}

/// Why no file can be placed at `rel`: it is a directory (a prefix of other
/// files), or one of its parent components is a file.
///
/// # Arguments
/// * `files` - the workspace's files.
/// * `rel` - a checked, catalog-relative file path.
///
/// # Returns
/// `write_atomic`'s reason, or `None` when the path is free to hold a file.
fn placement_conflict(files: &BTreeMap<String, StoredFile>, rel: &str) -> Option<&'static str> {
    if has_key_with_prefix(files, &format!("{rel}/")) {
        Some("is a directory")
    } else {
        // Any proper prefix of the path that is a file blocks it.
        rel.match_indices('/')
            .any(|(i, _)| files.contains_key(&rel[..i]))
            .then_some("a parent component is a file")
    }
}

/// Every file that is also another file's directory (`a` beside `a/b.md`), in
/// path order. Such a map is not a tree: [`MemWorkspace::from_stored`] refuses
/// it, and `crate::archive` lists every clash as an import problem.
///
/// # Arguments
/// * `files` - catalog-relative path to file.
///
/// # Returns
/// A lazy iterator over the clashing paths, O(log n) per path checked.
pub(crate) fn files_that_are_directories(
    files: &BTreeMap<String, StoredFile>,
) -> impl Iterator<Item = &String> {
    // One reused buffer for the `"<file>/"` probes, instead of a new `String`
    // per key. `move` hands the buffer to the closure, which owns it from then
    // on; `filter` needs `FnMut` because the closure changes it.
    let mut dir_prefix = String::new();
    files.keys().filter(move |key| {
        dir_prefix.clear();
        dir_prefix.push_str(key);
        dir_prefix.push('/');
        has_key_with_prefix(files, &dir_prefix)
    })
}

/// One stored file: its exact bytes and its execute bit (a record, so its
/// fields are public).
// `Eq` on top of `PartialEq` promises equality is total (no NaN-like values),
// which lets tests compare whole records with `assert_eq!`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredFile {
    /// The file's exact bytes (UTF-8 or not).
    pub content: Vec<u8>,
    /// Is the execute bit set (`chmod +x`)?
    pub executable: bool,
}

/// An in-memory catalog. Directories exist implicitly as prefixes
/// of file paths.
///
/// It refuses the same malformed writes as [`DiskWorkspace`] (onto a
/// directory, below a file) but does not simulate permissions, symlinks or
/// special files, so the disk-only refusals ("is a symlink", "not a regular
/// file", `PermissionDenied`) never occur here.
///
/// Production code: a stored catalog's snapshot is built over one (see
/// `crate::catalog`), and tests use it as a catalog fixture. Each file is
/// held as bytes with its execute bit, so non-text files exist (links to them
/// resolve) but cannot be read as text, as on disk.
#[derive(Debug, Default)]
pub struct MemWorkspace {
    // `RwLock` gives interior mutability: `write_atomic` takes `&self` (like the
    // disk version) yet can still change the map, safely across threads.
    files: RwLock<BTreeMap<String, StoredFile>>,
}

impl MemWorkspace {
    /// Builds a workspace from `(path, contents)` text fixtures, none of them
    /// executable.
    ///
    /// # Arguments
    /// * `files` - `(catalog-relative path, UTF-8 contents)` pairs.
    ///
    /// # Returns
    /// The workspace holding those files.
    ///
    /// # Panics
    /// If a path escapes the catalog (a bad test fixture).
    pub fn from_files<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let map = files
            .into_iter()
            .map(|(p, c)| {
                let rel = checked_file(p).expect("fixture path must be inside the catalog");
                let file = StoredFile {
                    content: c.as_bytes().to_vec(),
                    executable: false,
                };
                (rel, file)
            })
            .collect();
        Self {
            files: RwLock::new(map),
        }
    }

    /// Builds a workspace from stored files (a catalog's rows).
    ///
    /// Refuses the inputs [`Workspace::write_atomic`] could never have
    /// produced, so the result is always a consistent tree.
    ///
    /// # Arguments
    /// * `files` - `(catalog-relative path, file)` pairs. A leading `/` means
    ///   the catalog root, as for every [`Workspace`] path.
    ///
    /// # Returns
    /// The workspace holding those files, keyed by normalized path.
    ///
    /// # Errors
    /// [`OkfError::InvalidPath`] if a path escapes the catalog or is the root,
    /// if two paths normalize to the same one (`./a.md` and `a.md`), or if a
    /// file is also a directory of another (`a` beside `a/b.md`).
    pub fn from_stored(
        files: impl IntoIterator<Item = (String, StoredFile)>,
    ) -> Result<Self, OkfError> {
        let mut map = BTreeMap::new();
        for (path, file) in files {
            // `insert` returns the value it replaced (`Some`) for a key that
            // was already there: Python's `if key in d` and `d[key] = v` in one.
            if map.insert(checked_file(&path)?, file).is_some() {
                return Err(OkfError::InvalidPath {
                    path,
                    reason: "a duplicate of another path".to_string(),
                });
            }
        }
        // `.next()` takes the first clash only; the iterator stops there.
        if let Some(path) = files_that_are_directories(&map).next() {
            return Err(OkfError::InvalidPath {
                path: path.clone(),
                reason: "is a file and a directory".to_string(),
            });
        }
        Ok(Self {
            files: RwLock::new(map),
        })
    }

    /// The stored file at `path`, if there is one.
    ///
    /// # Arguments
    /// * `path` - Catalog-relative path.
    ///
    /// # Returns
    /// A copy of its bytes and execute bit; `None` if there is no such file
    /// (or the path escapes the catalog).
    pub fn stored(&self, path: &str) -> Option<StoredFile> {
        let rel = checked(path).ok()?;
        self.files().get(&rel).cloned()
    }

    /// Removes a file. Directories exist only as prefixes of file paths,
    /// so an emptied directory is gone too.
    ///
    /// Not part of [`Workspace`]: only the server's changesets delete, and
    /// they always work on a `MemWorkspace` (the write path's copy of an
    /// overlay).
    ///
    /// # Arguments
    /// * `path` - Catalog-relative path (a leading `/` means the root).
    ///
    /// # Returns
    /// `true` if a file was there (now removed), `false` if none was.
    ///
    /// # Errors
    /// [`OkfError::InvalidPath`] if the path escapes the catalog or is the root.
    pub fn remove(&self, path: &str) -> Result<bool, OkfError> {
        let rel = checked_file(path)?;
        // `remove` gives back the old value, if any: `is_some` says whether
        // there was a file.
        Ok(self
            .files
            .write()
            .expect("MemWorkspace lock poisoned")
            .remove(&rel)
            .is_some())
    }

    /// Moves a file to a new path, keeping its bytes and execute bit, as
    /// `git mv` keeps a file's mode (`move_concept`).
    ///
    /// Not part of [`Workspace`], for the reason [`MemWorkspace::remove`]
    /// gives.
    ///
    /// # Arguments
    /// * `from` - the file's catalog-relative path.
    /// * `to` - its new path.
    ///
    /// Checks that a new file could be written at `path` without writing
    /// it: the check [`Workspace::write_atomic`] makes before it writes, so
    /// an op can refuse a bad target before it writes anything else
    /// (`move_concept`).
    ///
    /// # Arguments
    /// * `path` - Catalog-relative path (a leading `/` means the root).
    ///
    /// # Errors
    /// [`OkfError::InvalidPath`] if the path escapes the catalog or is the
    /// root, is a directory, or sits below a file.
    pub fn check_placement(&self, path: &str) -> Result<(), OkfError> {
        let rel = checked_file(path)?;
        // `map_or`: no conflict is `Ok`, a conflict becomes the error.
        placement_conflict(&self.files(), &rel).map_or(Ok(()), |reason| {
            Err(OkfError::InvalidPath {
                path: path.to_string(),
                reason: reason.to_string(),
            })
        })
    }

    /// # Errors
    /// [`OkfError::InvalidPath`] if either path escapes the catalog or is
    /// the root, or `to` already exists, is a directory, or sits below a
    /// file; [`OkfError::NotFound`] if there is no file at `from`. `to` is
    /// checked first, so a missing `from` with a bad `to` is `InvalidPath`.
    pub fn rename(&self, from: &str, to: &str) -> Result<(), OkfError> {
        let (from_rel, to_rel) = (checked_file(from)?, checked_file(to)?);
        let mut files = self.files.write().expect("MemWorkspace lock poisoned");
        // `or_else` only runs the placement check when the path is not taken.
        let taken = files.contains_key(&to_rel).then_some("already exists");
        if let Some(reason) = taken.or_else(|| placement_conflict(&files, &to_rel)) {
            return Err(OkfError::InvalidPath {
                path: to.to_string(),
                reason: reason.to_string(),
            });
        }
        let file = files.remove(&from_rel).ok_or_else(|| OkfError::NotFound {
            path: from.to_string(),
        })?;
        files.insert(to_rel, file);
        Ok(())
    }

    /// Every file, consuming the workspace (no copy).
    ///
    /// # Returns
    /// Catalog-relative path to file, sorted by path.
    pub fn into_stored(self) -> BTreeMap<String, StoredFile> {
        // `into_inner` takes the map out of the lock; it fails only if a
        // thread panicked while holding the lock.
        self.files.into_inner().expect("MemWorkspace lock poisoned")
    }

    /// A copy of this workspace with `changes` applied: a changeset's view of
    /// the published files (an overlay).
    ///
    /// The copy is checked as [`MemWorkspace::from_stored`] checks its input,
    /// so it is always a consistent tree; this workspace is not changed.
    ///
    /// # Arguments
    /// * `changes` - `(catalog-relative path, file)` pairs, applied in order:
    ///   `Some` writes the file (bytes and execute bit as given), `None`
    ///   deletes the path (a missing path is fine).
    ///
    /// # Returns
    /// The new workspace.
    ///
    /// # Errors
    /// [`OkfError::InvalidPath`] if a path escapes the catalog or is the root,
    /// or if the result has a file that is also a directory of another.
    pub fn with_changes(
        &self,
        changes: impl IntoIterator<Item = (String, Option<StoredFile>)>,
    ) -> Result<Self, OkfError> {
        // `clone()` on the read guard copies the map it guards (the guard
        // itself is not `Clone`, so method lookup reaches the map); the
        // guard, and the lock, end with this statement.
        let mut map = self.files().clone();
        for (path, file) in changes {
            let rel = checked_file(&path)?;
            match file {
                Some(file) => {
                    map.insert(rel, file);
                }
                None => {
                    map.remove(&rel);
                }
            }
        }
        if let Some(path) = files_that_are_directories(&map).next() {
            return Err(OkfError::InvalidPath {
                path: path.clone(),
                reason: "is a file and a directory".to_string(),
            });
        }
        Ok(Self {
            files: RwLock::new(map),
        })
    }

    fn files(&self) -> std::sync::RwLockReadGuard<'_, BTreeMap<String, StoredFile>> {
        // A poisoned lock means a thread panicked mid-write; propagating the panic is right.
        self.files.read().expect("MemWorkspace lock poisoned")
    }

    /// The stored paths `listing` selects, in `os.walk` order: the same rules
    /// [`DiskWorkspace`]'s walk applies, checked per path instead of per directory.
    fn listing(&self, listing: Listing) -> Vec<String> {
        let mut out: Vec<String> = self
            .files()
            .keys()
            .filter(|path| listing.lists(path))
            .cloned()
            .collect();
        out.sort_by(|a, b| walk_order(a, b));
        out
    }
}

impl Workspace for MemWorkspace {
    fn read_to_string(&self, path: &str) -> Result<String, OkfError> {
        let rel = checked(path)?;
        let files = self.files();
        let file = files.get(&rel).ok_or_else(|| OkfError::NotFound {
            path: path.to_string(),
        })?;
        // The same error `std::fs::read_to_string` gives for non-UTF-8 bytes.
        // `from_utf8` borrows the bytes (no copy until it is known to be text).
        std::str::from_utf8(&file.content)
            .map(str::to_string)
            .map_err(|error| OkfError::Io {
                path: path.to_string(),
                source: std::io::Error::new(std::io::ErrorKind::InvalidData, error),
            })
    }

    fn write_atomic(&self, path: &str, contents: &str) -> Result<(), OkfError> {
        let rel = checked_file(path)?;
        let mut files = self.files.write().expect("MemWorkspace lock poisoned");
        // `&files` derefs from the lock guard to the map it guards.
        if let Some(reason) = placement_conflict(&files, &rel) {
            return Err(OkfError::InvalidPath {
                path: path.to_string(),
                reason: reason.to_string(),
            });
        }
        // An existing file keeps its execute bit, as a rewrite on disk keeps
        // its mode; a new file is not executable (0o666 less the umask).
        let executable = files.get(&rel).is_some_and(|file| file.executable);
        files.insert(
            rel,
            StoredFile {
                content: contents.as_bytes().to_vec(),
                executable,
            },
        );
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
        Ok(self.listing(Listing::Markdown))
    }

    fn list_files(&self) -> Result<Vec<String>, OkfError> {
        Ok(self.listing(Listing::AllFiles))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stored(content: &[u8], executable: bool) -> StoredFile {
        StoredFile {
            content: content.to_vec(),
            executable,
        }
    }

    #[test]
    fn rename_keeps_bytes_and_execute_bit_and_refuses_a_bad_target() {
        let workspace = MemWorkspace::from_stored([
            ("a/x.md".to_string(), stored(b"x", true)),
            ("b.md".to_string(), stored(b"b", false)),
        ])
        .unwrap();

        workspace.rename("a/x.md", "c/y.md").unwrap();
        let taken = workspace.rename("c/y.md", "b.md").unwrap_err();
        let below_a_file = workspace.rename("c/y.md", "b.md/z.md").unwrap_err();
        let missing = workspace.rename("a/x.md", "d.md").unwrap_err();
        let a_directory = workspace.rename("b.md", "c").unwrap_err();

        assert_eq!(workspace.stored("c/y.md"), Some(stored(b"x", true)));
        // Directories exist only as prefixes, so the emptied `a` is gone.
        assert!(!workspace.is_file("a/x.md") && !workspace.is_dir("a"));
        assert!(matches!(
            taken,
            OkfError::InvalidPath { ref reason, .. } if reason == "already exists"
        ));
        assert!(matches!(
            below_a_file,
            OkfError::InvalidPath { ref reason, .. } if reason == "a parent component is a file"
        ));
        assert!(matches!(missing, OkfError::NotFound { .. }));
        assert!(matches!(
            a_directory,
            OkfError::InvalidPath { ref reason, .. } if reason == "is a directory"
        ));
        assert_eq!(workspace.stored("b.md"), Some(stored(b"b", false)));
    }

    #[test]
    fn check_placement_refuses_a_directory_or_a_path_below_a_file() {
        let workspace = MemWorkspace::from_files([("a/x.md", "x")]);

        let free = workspace.check_placement("a/y.md");
        let taken_is_fine = workspace.check_placement("a/x.md");
        let a_directory = workspace.check_placement("a").unwrap_err();
        let below_a_file = workspace.check_placement("a/x.md/y.md").unwrap_err();

        // `check_placement` asks only whether a file can sit there; a taken
        // path is `rename`'s and `new_concept_path`'s concern.
        assert!(free.is_ok() && taken_is_fine.is_ok());
        assert_eq!(a_directory.to_string(), "a: is a directory");
        assert_eq!(
            below_a_file.to_string(),
            "a/x.md/y.md: a parent component is a file"
        );
    }

    #[test]
    fn stored_files_keep_bytes_and_execute_bits() {
        let ws = MemWorkspace::from_stored([
            ("tools/run.sh".to_string(), stored(b"#!/bin/sh\n", true)),
            ("img/logo.png".to_string(), stored(b"\x89PNG\0\xff", false)),
            ("a.md".to_string(), stored(b"# A\n", false)),
        ])
        .unwrap();

        assert_eq!(ws.read_to_string("tools/run.sh").unwrap(), "#!/bin/sh\n");
        assert_eq!(
            ws.stored("tools/run.sh"),
            Some(stored(b"#!/bin/sh\n", true))
        );
        // Non-UTF-8 content is listed and exists, but reading it as text fails
        // with `Io`, exactly as `DiskWorkspace` reports it.
        assert!(ws.is_file("img/logo.png"));
        assert!(ws
            .list_files()
            .unwrap()
            .contains(&"img/logo.png".to_string()));
        assert!(matches!(
            ws.read_to_string("img/logo.png"),
            Err(OkfError::Io { .. })
        ));
    }

    #[test]
    fn a_write_keeps_an_existing_execute_bit_and_new_files_are_not_executable() {
        let ws = MemWorkspace::from_stored([("run.sh".to_string(), stored(b"old", true))]).unwrap();

        ws.write_atomic("run.sh", "new").unwrap();
        ws.write_atomic("fresh.sh", "x").unwrap();

        assert_eq!(ws.stored("run.sh"), Some(stored(b"new", true)));
        assert_eq!(ws.stored("fresh.sh"), Some(stored(b"x", false)));
    }

    #[test]
    fn from_stored_refuses_a_path_outside_the_catalog() {
        let refused = MemWorkspace::from_stored([("../x.md".to_string(), stored(b"", false))]);
        assert!(matches!(refused, Err(OkfError::InvalidPath { .. })));
    }

    #[test]
    fn from_stored_refuses_paths_that_normalize_to_the_same_file() {
        let refused = MemWorkspace::from_stored([
            ("./a.md".to_string(), stored(b"one", false)),
            ("a.md".to_string(), stored(b"two", false)),
        ]);
        assert!(matches!(refused, Err(OkfError::InvalidPath { path, .. }) if path == "a.md"));
    }

    #[test]
    fn from_stored_refuses_a_file_that_is_also_a_directory() {
        // `a-x` sorts between `a` and `a/b.md` (`-` < `/`), so the check
        // cannot just look at the next key.
        let refused = MemWorkspace::from_stored([
            ("a".to_string(), stored(b"", false)),
            ("a-x".to_string(), stored(b"", false)),
            ("a/b.md".to_string(), stored(b"", false)),
        ]);
        assert!(matches!(refused, Err(OkfError::InvalidPath { path, .. }) if path == "a"));
    }

    #[test]
    fn remove_deletes_a_file_and_says_whether_there_was_one() {
        let workspace = MemWorkspace::from_files([("a/b.md", "x"), ("c.md", "y")]);

        let removed = workspace.remove("/a/b.md").unwrap();
        let again = workspace.remove("a/b.md").unwrap();
        let outside = workspace.remove("../x.md");

        assert!(removed && !again);
        assert!(!workspace.is_file("a/b.md") && !workspace.is_dir("a"));
        assert!(workspace.is_file("c.md"));
        assert!(matches!(outside, Err(OkfError::InvalidPath { .. })));
    }

    #[test]
    fn with_changes_copies_the_files_and_applies_writes_and_deletes() {
        let base = MemWorkspace::from_stored([
            ("a.md".to_string(), stored(b"# A\n", false)),
            ("b.md".to_string(), stored(b"# B\n", false)),
            ("run.sh".to_string(), stored(b"#!/bin/sh\n", true)),
        ])
        .unwrap();

        let changed = base
            .with_changes([
                ("a.md".to_string(), Some(stored(b"# A2\n", false))),
                ("b.md".to_string(), None),
                ("new/c.md".to_string(), Some(stored(b"# C\n", false))),
            ])
            .unwrap();
        let clash = base.with_changes([("a.md/x.md".to_string(), Some(stored(b"x", false)))]);
        let escape = base.with_changes([("../x.md".to_string(), None)]);

        assert_eq!(changed.read_to_string("a.md").unwrap(), "# A2\n");
        assert!(!changed.is_file("b.md"));
        assert_eq!(changed.read_to_string("new/c.md").unwrap(), "# C\n");
        assert_eq!(changed.stored("run.sh"), Some(stored(b"#!/bin/sh\n", true)));
        // The original is a separate copy, unchanged.
        assert_eq!(base.read_to_string("a.md").unwrap(), "# A\n");
        assert!(base.is_file("b.md"));
        assert!(matches!(clash, Err(OkfError::InvalidPath { .. })));
        assert!(matches!(escape, Err(OkfError::InvalidPath { .. })));
    }

    #[test]
    fn into_stored_returns_every_file() {
        let ws = MemWorkspace::from_files([("a.md", "a"), ("b/c.txt", "c")]);
        let files = ws.into_stored();
        assert_eq!(files.len(), 2);
        assert_eq!(files["b/c.txt"], stored(b"c", false));
    }

    const FILES: [(&str, &str); 11] = [
        ("index.md", "root"),
        ("b.md", "b"),
        ("infra/index.md", "infra"),
        ("infra/hosts/x.md", "x"),
        (".claude/skill.md", "hidden"),
        ("notes.txt", "not markdown"),
        ("crlf.txt", "a\r\nb\rc\n"),
        (".git/config", "git internals"),
        (".worktrees/s1/index.md", "a session worktree"),
        ("infra/.git/HEAD", "a nested repository"),
        ("infra/hosts/.git", "gitdir: a worktree's link file"),
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
        // Every file, in os.walk order (files before subdirectories), skipping
        // only `.git` and `.worktrees` directories. Other dot-directories count,
        // and so does a `.git` file.
        assert_eq!(
            ws.list_files().unwrap(),
            [
                "b.md",
                "crlf.txt",
                "index.md",
                "notes.txt",
                ".claude/skill.md",
                "infra/index.md",
                "infra/hosts/.git",
                "infra/hosts/x.md",
            ]
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
        // A missing file is `NotFound` on every workspace, through both reads.
        assert!(matches!(
            ws.read_to_string("missing.md"),
            Err(OkfError::NotFound { path }) if path == "missing.md"
        ));
        assert!(matches!(
            ws.read_text("infra/missing.md"),
            Err(OkfError::NotFound { path }) if path == "infra/missing.md"
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
        // A directory is not a file, and a file is not a directory: both
        // workspaces refuse these the same way.
        assert!(matches!(
            ws.write_atomic("infra", "x"),
            Err(OkfError::InvalidPath { reason, .. }) if reason == "is a directory"
        ));
        assert!(matches!(
            ws.write_atomic("b.md/c.md", "x"),
            Err(OkfError::InvalidPath { reason, .. }) if reason == "a parent component is a file"
        ));
        assert!(matches!(
            ws.write_atomic("b.md/deeper/c.md", "x"),
            Err(OkfError::InvalidPath { reason, .. }) if reason == "a parent component is a file"
        ));
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

    /// `install/` anywhere, plus `docs/` and `tests/` at the root, beside
    /// nested `docs/` and `tests/` that are walked as usual.
    const SKIP_FILES: [(&str, &str); 7] = [
        ("a.md", "a"),
        ("install/setup.md", "template machinery"),
        ("infra/install/x.md", "nested install"),
        ("docs/plan.md", "template docs"),
        ("tests/case.md", "template tests"),
        ("infra/docs/y.md", "nested docs"),
        ("infra/tests/z.md", "nested tests"),
    ];

    fn assert_skips_template_machinery(ws: &dyn Workspace) {
        assert_eq!(
            ws.list_markdown().unwrap(),
            ["a.md", "infra/docs/y.md", "infra/tests/z.md"]
        );
        // The secret walk is unchanged: it still reads every one of them.
        assert_eq!(ws.list_files().unwrap().len(), SKIP_FILES.len());
    }

    #[test]
    fn mem_markdown_walk_skips_template_machinery() {
        assert_skips_template_machinery(&MemWorkspace::from_files(SKIP_FILES));
    }

    #[test]
    fn disk_markdown_walk_skips_template_machinery() {
        let (_dir, ws) = disk_catalog(&SKIP_FILES);
        assert_skips_template_machinery(&ws);
    }

    #[test]
    fn listing_lists_checks_each_component() {
        assert!(Listing::Markdown.lists("a.md"));
        assert!(!Listing::Markdown.lists("a.txt"));
        assert!(!Listing::Markdown.lists("docs/a.md"));
        assert!(Listing::Markdown.lists("x/docs/a.md"));
        assert!(!Listing::Markdown.lists("x/install/a.md"));
        assert!(!Listing::Markdown.lists("x/.hidden/a.md"));
        assert!(Listing::AllFiles.lists("docs/a.txt"));
        assert!(!Listing::AllFiles.lists("x/.worktrees/a.txt"));
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
        fn a_read_only_file_is_refused_like_python_open() {
            let (dir, ws) = disk_catalog(&FILES);
            // As root, the kernel lets anyone write a 0o444 file (so does
            // Python); then there is nothing to refuse, and the test is moot.
            let probe = dir.path().join("probe");
            std::fs::write(&probe, "p").unwrap();
            std::fs::set_permissions(&probe, std::fs::Permissions::from_mode(0o444)).unwrap();
            if std::fs::OpenOptions::new().write(true).open(&probe).is_ok() {
                // Root: Python's `open(path, "w")` succeeds, so the port must too.
                let target = ws.root().join("b.md");
                std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o444)).unwrap();
                ws.write_atomic("b.md", "new").unwrap();
                assert_eq!(std::fs::read_to_string(&target).unwrap(), "new");
                return;
            }
            let target = ws.root().join("b.md");
            std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o444)).unwrap();
            let error = ws.write_atomic("b.md", "new").unwrap_err();
            assert!(
                matches!(&error, OkfError::Io { path, source }
                    if path == "b.md" && source.kind() == std::io::ErrorKind::PermissionDenied),
                "{error:?}"
            );
            assert_eq!(std::fs::read_to_string(&target).unwrap(), "b");
            assert_eq!(mode(&target), 0o444);
            assert!(leftover_temp_files(ws.root()).is_empty());
        }

        #[test]
        fn write_onto_a_fifo_is_refused() {
            let (_dir, ws) = disk_catalog(&FILES);
            let fifo = ws.root().join("pipe.md");
            let made = std::process::Command::new("mkfifo").arg(&fifo).status();
            if !made.is_ok_and(|status| status.success()) {
                return; // no mkfifo on this system
            }
            assert!(matches!(
                ws.write_atomic("pipe.md", "x"),
                Err(OkfError::InvalidPath { reason, .. }) if reason == "not a regular file"
            ));
            let kind = std::fs::symlink_metadata(&fifo).unwrap().file_type();
            assert!(std::os::unix::fs::FileTypeExt::is_fifo(&kind));
        }

        #[test]
        fn listings_skip_special_files() {
            let (_dir, ws) = disk_catalog(&FILES);
            let made = std::process::Command::new("mkfifo")
                .arg(ws.root().join("pipe.md"))
                .status();
            if !made.is_ok_and(|status| status.success()) {
                return; // no mkfifo on this system
            }
            // Reading a FIFO would block forever, so neither walk lists it.
            assert!(!ws.list_files().unwrap().contains(&"pipe.md".to_string()));
            assert!(!ws.list_markdown().unwrap().contains(&"pipe.md".to_string()));
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
        fn write_onto_a_symlink_to_an_outside_directory_says_symlink() {
            let (_dir, ws, outside) = with_outside();
            symlink(&outside, ws.root().join("dirlink")).unwrap();
            assert!(matches!(
                ws.write_atomic("dirlink", "x"),
                Err(OkfError::InvalidPath { reason, .. }) if reason.starts_with("is a symlink")
            ));
        }

        #[test]
        fn list_markdown_does_not_traverse_symlinked_directories() {
            let (_dir, ws, outside) = with_outside();
            symlink(&outside, ws.root().join("evil")).unwrap();
            symlink(&outside, ws.root().join("dirlink.md")).unwrap();
            // Both walks share the symlink rules.
            for listed in [ws.list_markdown().unwrap(), ws.list_files().unwrap()] {
                assert!(!listed.iter().any(|p| p.contains("secret")));
                assert!(!listed.contains(&"dirlink.md".to_string()));
                assert!(!listed.iter().any(|p| p.starts_with("evil")));
            }
        }

        #[test]
        fn listing_skips_dangling_and_outside_file_links() {
            let (_dir, ws, outside) = with_outside();
            symlink(ws.root().join("gone.md"), ws.root().join("dangling.md")).unwrap();
            symlink(outside.join("secret.md"), ws.root().join("leak.md")).unwrap();
            symlink(ws.root().join("b.md"), ws.root().join("ok.md")).unwrap();
            // Both walks share the symlink rules.
            for listed in [ws.list_markdown().unwrap(), ws.list_files().unwrap()] {
                assert!(listed.contains(&"ok.md".to_string()));
                assert!(!listed.contains(&"dangling.md".to_string()));
                assert!(!listed.contains(&"leak.md".to_string()));
                for path in &listed {
                    assert!(ws.read_to_string(path).is_ok(), "{path}");
                }
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

//! The filesystem a catalog lives in, behind a trait so the OKF logic can be
//! tested against an in-memory catalog.
//!
//! Paths are bundle-relative (`infra/time-server.md`). Implementations must
//! refuse paths that escape the catalog root.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use crate::error::OkfError;
use crate::okf::paths::{normpath, walk_order};

/// Read and write access to one catalog's files.
///
/// `Send + Sync` so a workspace can be shared across tokio tasks.
pub trait Workspace: Send + Sync {
    /// Reads a file as UTF-8.
    ///
    /// # Errors
    /// [`OkfError::Io`] if the file is missing, unreadable or not UTF-8.
    fn read_to_string(&self, path: &str) -> Result<String, OkfError>;

    /// Writes a file atomically (all or nothing), creating parent directories.
    ///
    /// # Errors
    /// [`OkfError::Io`] on any filesystem failure; [`OkfError::InvalidPath`]
    /// if the path escapes the catalog.
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
    /// (see [`walk_order`]).
    ///
    /// # Errors
    /// [`OkfError::Io`] if a directory can't be listed.
    fn list_markdown(&self) -> Result<Vec<String>, OkfError>;
}

/// Normalizes a bundle-relative path and refuses one that leaves the bundle.
fn checked(path: &str) -> Result<String, OkfError> {
    let normalized = normpath(path.trim_start_matches('/'));
    if normalized.starts_with("..") {
        return Err(OkfError::InvalidPath {
            path: path.to_string(),
            reason: "outside the catalog".to_string(),
        });
    }
    Ok(normalized)
}

/// Is any directory component hidden (starts with `.`)?
fn in_dot_directory(path: &str) -> bool {
    let mut parts: Vec<&str> = path.split('/').collect();
    parts.pop();
    parts.iter().any(|p| p.starts_with('.'))
}

/// A catalog on disk, rooted at a directory.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskWorkspace {
    root: PathBuf,
}

impl DiskWorkspace {
    /// Creates a workspace rooted at `root`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self { root: root.into() }
    }

    /// The root directory.
    pub fn root(&self) -> &Path {
        &self.root
    }

    fn full(&self, path: &str) -> Result<PathBuf, OkfError> {
        let rel = checked(path)?;
        Ok(if rel == "." {
            self.root.clone()
        } else {
            self.root.join(rel)
        })
    }

    fn collect(&self, dir: &Path, prefix: &str, out: &mut Vec<String>) -> Result<(), OkfError> {
        let io_err = |source| OkfError::Io {
            path: prefix.to_string(),
            source,
        };
        for entry in std::fs::read_dir(dir).map_err(io_err)? {
            let entry = entry.map_err(io_err)?;
            let name = entry.file_name().to_string_lossy().into_owned();
            let rel = if prefix.is_empty() {
                name.clone()
            } else {
                format!("{prefix}/{name}")
            };
            let file_type = entry.file_type().map_err(io_err)?;
            if file_type.is_dir() {
                if !name.starts_with('.') {
                    self.collect(&entry.path(), &rel, out)?;
                }
            } else if name.ends_with(".md") {
                out.push(rel);
            }
        }
        Ok(())
    }
}

impl Workspace for DiskWorkspace {
    fn read_to_string(&self, path: &str) -> Result<String, OkfError> {
        std::fs::read_to_string(self.full(path)?).map_err(|source| OkfError::Io {
            path: path.to_string(),
            source,
        })
    }

    fn write_atomic(&self, path: &str, contents: &str) -> Result<(), OkfError> {
        let target = self.full(path)?;
        let io_err = |source| OkfError::Io {
            path: path.to_string(),
            source,
        };
        let parent = target.parent().unwrap_or(&self.root).to_path_buf();
        std::fs::create_dir_all(&parent).map_err(io_err)?;
        // Write a sibling temp file, then rename over the target: a reader sees
        // the old file or the new one, never half of it.
        let file_name = target
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let temp = parent.join(format!(".{file_name}.tmp-{}", std::process::id()));
        std::fs::write(&temp, contents).map_err(io_err)?;
        std::fs::rename(&temp, &target).map_err(io_err)
    }

    fn is_file(&self, path: &str) -> bool {
        self.full(path).is_ok_and(|p| p.is_file())
    }

    fn is_dir(&self, path: &str) -> bool {
        self.full(path).is_ok_and(|p| p.is_dir())
    }

    fn list_markdown(&self) -> Result<Vec<String>, OkfError> {
        let mut out = Vec::new();
        self.collect(&self.root, "", &mut out)?;
        out.sort_by(|a, b| walk_order(a, b));
        Ok(out)
    }
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
    pub fn from_files<'a>(files: impl IntoIterator<Item = (&'a str, &'a str)>) -> Self {
        let map = files
            .into_iter()
            .map(|(p, c)| (p.to_string(), c.to_string()))
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
        let rel = checked(path)?;
        self.files
            .write()
            .expect("MemWorkspace lock poisoned")
            .insert(rel, contents.to_string());
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
        let prefix = format!("{rel}/");
        self.files().keys().any(|k| k.starts_with(&prefix))
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

    const FILES: [(&str, &str); 6] = [
        ("index.md", "root"),
        ("b.md", "b"),
        ("infra/index.md", "infra"),
        ("infra/hosts/x.md", "x"),
        (".claude/skill.md", "hidden"),
        ("notes.txt", "not markdown"),
    ];

    fn assert_contract(ws: &dyn Workspace) {
        assert_eq!(
            ws.list_markdown().unwrap(),
            ["b.md", "index.md", "infra/index.md", "infra/hosts/x.md"]
        );
        assert_eq!(ws.read_to_string("/infra/index.md").unwrap(), "infra");
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
    }

    #[test]
    fn mem_workspace_contract() {
        assert_contract(&MemWorkspace::from_files(FILES));
    }

    #[test]
    fn disk_workspace_contract() {
        let dir = tempfile::tempdir().unwrap();
        for (path, contents) in FILES {
            let full = dir.path().join(path);
            std::fs::create_dir_all(full.parent().unwrap()).unwrap();
            std::fs::write(full, contents).unwrap();
        }
        let ws = DiskWorkspace::new(dir.path());
        assert_contract(&ws);
        let leftovers: Vec<_> = std::fs::read_dir(dir.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().contains(".tmp-"))
            .collect();
        assert!(leftovers.is_empty());
    }
}

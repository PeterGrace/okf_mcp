//! Catalog path rules: bundle-relative path arithmetic, which files are
//! concepts, and how a caller's concept reference resolves to a file.
//!
//! All paths here are bundle-relative with `/` separators and no leading
//! slash (`infra/time-server.md`); `/` alone is never a filesystem root.
//! Unlike `okf.py`, filesystem paths (absolute or relative to a working
//! directory) are not accepted: a server has no meaningful working directory.
//! Escapes are detected by exact `..` components ([`escapes_root`]), so a name
//! like `..a` is an ordinary component: a deliberate divergence from
//! `okf.py`'s `startswith("..")`.

use std::borrow::Cow;
use std::cmp::Ordering;
use std::sync::LazyLock;

use regex::Regex;

use crate::error::OkfError;
use crate::okf::pystr::py_strip;
use crate::okf::workspace::Workspace;

/// Markdown files that are never concepts.
pub const NON_CONCEPTS: [&str; 5] = ["index.md", "log.md", "AGENTS.md", "CLAUDE.md", "README.md"];
/// OKF's reserved filenames.
pub const RESERVED: [&str; 2] = ["index.md", "log.md"];
/// Top-level directories that hold repository machinery, not concepts.
pub const MACHINERY_DIRS: [&str; 3] = [".git", ".claude", "tools"];

// `LazyLock` builds the value on first use and caches it: like a module-level
// `re.compile(...)` in Python, but deferred until needed.
static KEBAB_NAME: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"^[a-z0-9][a-z0-9-]*\.md$").expect("KEBAB_NAME regex is valid"));

/// Python's `posixpath.normpath` for relative paths: collapses `.`, `..` and
/// repeated slashes. An empty result is `.`; leading `..` components are kept.
/// A leading `//` collapses to `/` (Python keeps `//`), which never matters
/// because callers strip leading slashes first.
///
/// # Examples
/// ```
/// use okf_mcp::okf::paths::normpath;
/// assert_eq!(normpath("infra/./hosts/../time-server.md"), "infra/time-server.md");
/// assert_eq!(normpath("../x"), "../x");
/// assert_eq!(normpath(""), ".");
/// ```
pub fn normpath(path: &str) -> String {
    let absolute = path.starts_with('/');
    let mut parts: Vec<&str> = Vec::new();
    for component in path.split('/') {
        match component {
            "" | "." => {}
            ".." => {
                if parts.last().is_some_and(|p| *p != "..") {
                    parts.pop();
                } else if !absolute {
                    parts.push("..");
                }
            }
            other => parts.push(other),
        }
    }
    let joined = parts.join("/");
    match (absolute, joined.is_empty()) {
        (true, _) => format!("/{joined}"),
        (false, true) => ".".to_string(),
        (false, false) => joined,
    }
}

/// Does this normalized path leave the catalog (is `..` or starts with `../`)?
///
/// A component such as `..a` is an ordinary name, not a parent reference.
///
/// # Arguments
/// * `normalized` - a path already passed through [`normpath`].
///
/// # Returns
/// `true` when the path climbs above its starting directory.
pub fn escapes_root(normalized: &str) -> bool {
    normalized == ".." || normalized.starts_with("../")
}

/// The directory part of a path.
///
/// # Arguments
/// * `path` - a `/`-separated path.
///
/// # Returns
/// Everything before the last `/`, or `""` for a top-level file.
pub fn dirname(path: &str) -> &str {
    path.rsplit_once('/').map_or("", |(dir, _)| dir)
}

/// The final component of a path.
///
/// # Arguments
/// * `path` - a `/`-separated path.
///
/// # Returns
/// Everything after the last `/`, or the whole path if it has none.
pub fn basename(path: &str) -> &str {
    path.rsplit_once('/').map_or(path, |(_, name)| name)
}

/// Joins a directory and a path (Python's `os.path.join` for two parts).
///
/// # Arguments
/// * `dir` - the directory; may be empty.
/// * `path` - the path to append; if it starts with `/` it replaces `dir`.
///
/// # Returns
/// The joined path.
pub fn join(dir: &str, path: &str) -> String {
    if path.starts_with('/') || dir.is_empty() {
        path.to_string()
    } else if dir.ends_with('/') {
        format!("{dir}{path}")
    } else {
        format!("{dir}/{path}")
    }
}

/// Is this markdown file a concept (not `index.md`, `log.md` or machinery docs)?
///
/// # Arguments
/// * `path` - a bundle-relative markdown path.
///
/// # Returns
/// `true` when the file's name is not in [`NON_CONCEPTS`].
pub fn is_concept(path: &str) -> bool {
    !NON_CONCEPTS.contains(&basename(path))
}

/// Orders paths the way `os.walk` with sorted names visits them: in each
/// directory, its files (sorted) come before its subdirectories (sorted).
///
/// # Examples
/// ```
/// use okf_mcp::okf::paths::walk_order;
/// let mut paths = vec!["b/x.md", "a.md", "a/z.md", "c.md"];
/// paths.sort_by(|x, y| walk_order(x, y));
/// assert_eq!(paths, ["a.md", "c.md", "a/z.md", "b/x.md"]);
/// ```
pub fn walk_order(left: &str, right: &str) -> Ordering {
    // `split` is a lazy iterator, and `peekable` lets us look one item ahead
    // without consuming it: a component is a file when nothing follows it.
    let mut left_parts = left.split('/').peekable();
    let mut right_parts = right.split('/').peekable();
    loop {
        match (left_parts.next(), right_parts.next()) {
            (Some(l), Some(r)) if l == r => {}
            (Some(l), Some(r)) => {
                let left_is_file = left_parts.peek().is_none();
                let right_is_file = right_parts.peek().is_none();
                return match (left_is_file, right_is_file) {
                    (true, false) => Ordering::Less,
                    (false, true) => Ordering::Greater,
                    (true, true) | (false, false) => l.cmp(r),
                };
            }
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
        }
    }
}

/// Resolves a caller's concept reference to a bundle-relative path.
///
/// Accepts `/dir/x.md`, `dir/x.md`, `/dir/x` and `dir/x`.
///
/// # Arguments
/// * `workspace` - the catalog to look in.
/// * `reference` - the caller's reference.
///
/// # Errors
/// * [`OkfError::InvalidPath`] when the file exists but is not a concept.
/// * [`OkfError::NotFound`] when no candidate exists.
pub fn resolve_concept(workspace: &dyn Workspace, reference: &str) -> Result<String, OkfError> {
    let bare = reference.trim_start_matches('/');
    // `Cow` holds either a borrowed `&str` or an owned `String`, so the first
    // candidate needs no allocation.
    let candidates = [Cow::Borrowed(bare), Cow::Owned(format!("{bare}.md"))];
    for candidate in candidates {
        let candidate = normpath(&candidate);
        if escapes_root(&candidate) || !candidate.ends_with(".md") || !workspace.is_file(&candidate)
        {
            continue;
        }
        if !is_concept(&candidate) {
            return Err(OkfError::InvalidPath {
                path: candidate,
                reason: "is not a concept document".to_string(),
            });
        }
        return Ok(candidate);
    }
    Err(OkfError::NotFound {
        path: reference.to_string(),
    })
}

/// Validates the path for a new concept (`create`, `mv`, `split` targets).
///
/// # Arguments
/// * `workspace` - the catalog.
/// * `reference` - the requested path, with or without `.md` and a leading `/`.
///
/// # Returns
/// The normalized path and, when the filename isn't short, lowercase and
/// hyphenated, a note to pass back to the caller.
///
/// # Errors
/// [`OkfError::InvalidPath`] when the path leaves the catalog, uses a
/// reserved name, sits in machinery or a dot-directory, or already exists.
pub fn new_concept_path(
    workspace: &dyn Workspace,
    reference: &str,
) -> Result<(String, Option<String>), OkfError> {
    let invalid = |reason: &str| OkfError::InvalidPath {
        path: reference.to_string(),
        reason: reason.to_string(),
    };
    let mut path = py_strip(reference).trim_start_matches('/').to_string();
    if !path.ends_with(".md") {
        path.push_str(".md");
    }
    let path = normpath(&path);
    if escapes_root(&path) || path.starts_with('/') {
        return Err(invalid("not a path inside the catalog"));
    }
    let name = basename(&path);
    if NON_CONCEPTS.contains(&name) {
        return Err(invalid(&format!(
            "{name} is a reserved name, not a concept"
        )));
    }
    let first = path.split('/').next().unwrap_or_default();
    if MACHINERY_DIRS.contains(&first) || path.split('/').any(|p| p.starts_with('.')) {
        return Err(invalid(
            "that's repository machinery, not a place for concepts",
        ));
    }
    if workspace.exists(&path) {
        return Err(OkfError::InvalidPath {
            path: path.clone(),
            reason: "already exists".to_string(),
        });
    }
    // `bool::then` runs the closure only when the bool is true, giving `Some(..)`
    // (else `None`): Python's `x if cond else None`.
    let note = (!KEBAB_NAME.is_match(name))
        .then(|| format!("{name} isn't short, lowercase and hyphenated"));
    Ok((path, note))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::okf::workspace::MemWorkspace;

    fn workspace() -> MemWorkspace {
        MemWorkspace::from_files([
            ("index.md", "# Root\n"),
            ("infra/index.md", "# Infra\n"),
            ("infra/time-server.md", "---\ntype: Service\n---\n"),
            ("AGENTS.md", "# Agents\n"),
        ])
    }

    #[test]
    fn normpath_cases() {
        assert_eq!(normpath("a//b/./c/"), "a/b/c");
        assert_eq!(normpath("a/../../b"), "../b");
        assert_eq!(normpath("/a/../.."), "/");
        assert_eq!(normpath("."), ".");
    }

    #[test]
    fn dirname_basename_join() {
        assert_eq!(dirname("a/b/c.md"), "a/b");
        assert_eq!(dirname("c.md"), "");
        assert_eq!(basename("a/b/c.md"), "c.md");
        assert_eq!(join("", "x.md"), "x.md");
        assert_eq!(join("a", "x.md"), "a/x.md");
        assert_eq!(join("a", "/x.md"), "/x.md");
    }

    #[test]
    fn concepts_exclude_reserved_and_machinery_docs() {
        assert!(is_concept("infra/time-server.md"));
        assert!(!is_concept("infra/index.md"));
        assert!(!is_concept("AGENTS.md"));
    }

    #[test]
    fn resolve_accepts_every_spelling() {
        let ws = workspace();
        for reference in [
            "/infra/time-server.md",
            "infra/time-server.md",
            "/infra/time-server",
            "infra/time-server",
        ] {
            assert_eq!(
                resolve_concept(&ws, reference).unwrap(),
                "infra/time-server.md",
                "{reference}"
            );
        }
    }

    #[test]
    fn resolve_refuses_non_concepts_and_missing() {
        let ws = workspace();
        assert!(matches!(
            resolve_concept(&ws, "/infra/index.md"),
            Err(OkfError::InvalidPath { .. })
        ));
        assert!(matches!(
            resolve_concept(&ws, "/nope"),
            Err(OkfError::NotFound { .. })
        ));
        assert!(matches!(
            resolve_concept(&ws, "../etc/passwd"),
            Err(OkfError::NotFound { .. })
        ));
    }

    #[test]
    fn new_concept_path_rules() {
        let ws = workspace();
        assert_eq!(
            new_concept_path(&ws, "/infra/new-thing").unwrap(),
            ("infra/new-thing.md".to_string(), None)
        );
        let (_, note) = new_concept_path(&ws, "infra/New_Thing.md").unwrap();
        assert!(note.is_some());
        for bad in [
            "../x",
            "infra/index.md",
            "tools/x",
            ".claude/x",
            "a/.hidden/x",
            "/infra/time-server",
        ] {
            assert!(new_concept_path(&ws, bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn new_concept_path_strips_python_whitespace() {
        // Python's str.strip() removes U+001F; Rust's trim() does not.
        let ws = workspace();
        let (path, _) = new_concept_path(&ws, "\u{1f}infra/padded\u{1f}").unwrap();
        assert_eq!(path, "infra/padded.md");
    }

    #[test]
    fn normpath_matches_python_pairs() {
        for (input, expected) in [
            ("a//b/./c/", "a/b/c"),
            ("a/../../b", "../b"),
            ("", "."),
            ("x/..", "."),
            ("..a/b", "..a/b"),
        ] {
            assert_eq!(normpath(input), expected, "{input:?}");
        }
    }

    #[test]
    fn dotdot_prefixed_names_are_not_escapes() {
        let ws = MemWorkspace::from_files([("..a/b.md", "x")]);
        assert_eq!(resolve_concept(&ws, "..a/b").unwrap(), "..a/b.md");
        assert!(!escapes_root("..a/b") && escapes_root("../a") && escapes_root(".."));
    }

    #[hegel::test]
    fn normpath_is_idempotent(tc: hegel::TestCase) {
        use hegel::generators;
        let path: String = tc.draw(generators::text());
        let once = normpath(&path);
        assert_eq!(normpath(&once), once);
    }
}

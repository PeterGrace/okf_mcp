//! The rules every write obeys before anything is stored (design "Rules
//! enforced on every write"):
//! - paths stay inside the catalog;
//! - machinery is read-only;
//! - new content holds no likely secret;
//! - a markdown file's frontmatter still parses.
//!
//! The write path (`crate::write`) runs [`check_path`] and [`check_content`]
//! on every file an op touched, and publish runs them again on replayed
//! results. `revert` goes through the same path, so machinery cannot be
//! reverted either. Only import writes machinery.

use crate::error::OkfError;
use crate::okf::analysis::secrets::scan_secrets;
use crate::okf::doc::Doc;
use crate::okf::paths::{escapes_root, normpath, NON_CONCEPTS, RESERVED, SKIP_DIRS, SKIP_TOP};
use crate::okf::pystr::universal_newlines;

/// The catalog's own tooling directory, at the root (`okf.py` lives there).
const TOOLS: &str = "tools";

/// Is this catalog-relative path repository machinery, read-only to every
/// write tool (Decision 4)?
///
/// Machinery is:
/// - `AGENTS.md`, `CLAUDE.md` and `README.md` at the root ([`NON_CONCEPTS`]
///   without the reserved `index.md` and `log.md`);
/// - anything under the root's `tools/` and the [`SKIP_TOP`] directories
///   (`docs/`, `tests/`);
/// - any [`SKIP_DIRS`] directory (`.git`, `.claude`, `install`) at any
///   depth, and any other component starting with `.git` (`.gitignore`,
///   `.github/`).
///
/// # Arguments
/// * `path` - a normalized catalog-relative path (see [`check_path`]).
///
/// # Returns
/// `true` for machinery.
///
/// # Examples
/// ```
/// use okf_mcp::okf::rules::is_machinery;
/// assert!(is_machinery("tools/okf.py"));
/// assert!(is_machinery("infra/install/setup.md"));
/// assert!(!is_machinery("infra/docs/notes.md"));
/// ```
pub fn is_machinery(path: &str) -> bool {
    // `split_once` is Python's `str.partition` without the separator: `None`
    // when there is no `/`, i.e. a file at the catalog root.
    let (first, nested) = match path.split_once('/') {
        Some((first, _)) => (first, true),
        None => (path, false),
    };
    let root_doc = !nested && NON_CONCEPTS.contains(&path) && !RESERVED.contains(&path);
    root_doc
        || first == TOOLS
        || SKIP_TOP.contains(&first)
        || path
            .split('/')
            .any(|part| SKIP_DIRS.contains(&part) || part.starts_with(".git"))
}

/// Normalizes a path a write targets and checks that a write tool may
/// change it.
///
/// # Arguments
/// * `path` - catalog-relative; a leading `/` means the catalog root.
///
/// # Returns
/// The normalized path (`/infra/./x.md` becomes `infra/x.md`).
///
/// # Errors
/// [`OkfError::InvalidPath`] for a path outside the catalog (`..`), the
/// root itself, or machinery ([`is_machinery`]).
pub fn check_path(path: &str) -> Result<String, OkfError> {
    let normalized = normpath(path.trim_start_matches('/'));
    if normalized == "." || escapes_root(&normalized) {
        return Err(OkfError::InvalidPath {
            path: path.to_string(),
            reason: "not a path inside the catalog".to_string(),
        });
    }
    if is_machinery(&normalized) {
        return Err(OkfError::InvalidPath {
            path: normalized,
            reason: "is repository machinery, read-only to every write tool; only import \
                     writes it"
                .to_string(),
        });
    }
    Ok(normalized)
}

/// Checks a file's new content: no likely secret, and (for markdown)
/// frontmatter that parses.
///
/// # Arguments
/// * `path` - the normalized path, reported in errors.
/// * `text` - the complete new content.
///
/// # Errors
/// [`OkfError::SecretDetected`] for the first hit of the secret scan, on
/// the line `lint` would report (the scan reads universal-newline text);
/// [`OkfError::FrontmatterParse`] for a `.md` file whose frontmatter does
/// not parse.
pub fn check_content(path: &str, text: &str) -> Result<(), OkfError> {
    // `into_iter().next()` takes the first finding, by value.
    if let Some(hit) = scan_secrets(path, &universal_newlines(text))
        .into_iter()
        .next()
    {
        return Err(OkfError::SecretDetected {
            path: path.to_string(),
            line: hit.line,
            rule: hit.rule,
            message: hit.message,
        });
    }
    if path.ends_with(".md") {
        let doc = Doc::from_text(path, text);
        if doc.has_frontmatter() {
            doc.parse_checked()?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A secret-shaped value built at runtime, so no source file holds one.
    fn secret_value() -> String {
        "Ab1".repeat(4)
    }

    #[test]
    fn machinery_follows_decision_4() {
        let machinery = [
            "AGENTS.md",
            "CLAUDE.md",
            "README.md",
            "tools",
            "tools/okf.py",
            ".claude/settings.json",
            "infra/.claude/notes.md",
            ".git/config",
            ".gitignore",
            ".github/workflows/ci.yml",
            "infra/.gitattributes",
            "install/setup.md",
            "infra/install/x.md",
            "docs/plan.md",
            "tests/x.md",
        ];
        let writable = [
            "index.md",
            "log.md",
            "infra/index.md",
            "infra/time-server.md",
            "infra/AGENTS.md",
            "infra/README.md",
            "infra/docs/x.md",
            "infra/tests/x.md",
            "infra/tools/x.md",
            ".hidden/notes.md",
            "gitlab.md",
            "toolsmith.md",
            "installation.md",
            "docs.md",
        ];

        let wrongly_writable: Vec<&str> =
            machinery.into_iter().filter(|p| !is_machinery(p)).collect();
        let wrongly_machinery: Vec<&str> =
            writable.into_iter().filter(|p| is_machinery(p)).collect();

        assert!(wrongly_writable.is_empty(), "{wrongly_writable:?}");
        assert!(wrongly_machinery.is_empty(), "{wrongly_machinery:?}");
    }

    #[test]
    fn check_path_normalizes_and_refuses_what_is_outside_or_machinery() {
        let refused = [
            "../x.md",
            "a/../../x.md",
            "",
            "/",
            ".",
            "tools/okf.py",
            "./AGENTS.md",
        ];

        let normalized = [
            check_path("/infra/./hosts/../x.md").unwrap(),
            check_path("infra//x.md").unwrap(),
        ];
        let errors: Vec<OkfError> = refused
            .iter()
            .map(|path| check_path(path).unwrap_err())
            .collect();

        assert_eq!(normalized, ["infra/x.md", "infra/x.md"]);
        assert!(
            errors
                .iter()
                .all(|e| matches!(e, OkfError::InvalidPath { .. })),
            "{errors:?}"
        );
        assert!(matches!(
            &errors[5],
            OkfError::InvalidPath { path, reason }
                if path == "tools/okf.py" && reason.contains("machinery")
        ));
    }

    #[test]
    fn check_content_refuses_a_secret_on_the_line_lint_reports() {
        // A lone `\r` ends a line for lint's universal-newline read.
        let text = format!(
            "---\ntype: Note\n---\nold\rmac\npassword = {}\n",
            secret_value()
        );

        let error = check_content("notes.md", &text).unwrap_err();

        let OkfError::SecretDetected {
            path,
            line,
            rule,
            message,
        } = &error
        else {
            panic!("expected SecretDetected, got {error:?}");
        };
        assert_eq!(
            (path.as_str(), *line, rule.as_str()),
            ("notes.md", 6, "secret:assignment")
        );
        assert_eq!(
            message,
            "possible secret: 'password' followed by a literal value Ab1A... \
             (value mixes letters and digits)"
        );
        assert!(!error.to_json().to_string().contains(&secret_value()));
    }

    #[test]
    fn check_content_passes_placeholders_and_unparsed_non_markdown() {
        let placeholder = check_content("a.md", "---\ntype: Note\n---\npassword = changeme\n");
        let not_markdown = check_content("notes.txt", "---\nbroken: [\n---\n");
        let bare = check_content("b.md", "No frontmatter: at all\n");

        assert!(placeholder.is_ok());
        assert!(not_markdown.is_ok());
        assert!(bare.is_ok());
    }

    #[test]
    fn check_content_refuses_markdown_whose_frontmatter_does_not_parse() {
        let error = check_content("a.md", "---\ntype: Note\ntype: again\n---\n").unwrap_err();

        assert!(matches!(
            error,
            OkfError::FrontmatterParse { ref path, line: 3, .. } if path == "a.md"
        ));
    }
}

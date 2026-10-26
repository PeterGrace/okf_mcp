//! The crate's error type.

/// Errors from reading, parsing or editing an OKF catalog.
///
/// Later milestones add variants (auth, git, lint, publish); each variant
/// carries the catalog-relative path it concerns so an agent can act on it.
#[derive(Debug, thiserror::Error)]
pub enum OkfError {
    /// No concept or document at the given path.
    #[error("no concept {path:?}")]
    NotFound {
        /// The path as the caller gave it.
        path: String,
    },

    /// The path is outside the catalog, reserved, machinery, or already taken.
    #[error("{path}: {reason}")]
    InvalidPath {
        /// The path as the caller gave it.
        path: String,
        /// Why it was refused.
        reason: String,
    },

    /// A document's YAML frontmatter does not parse (or would not, after an edit).
    #[error("{path}: frontmatter does not parse (line {line}): {message}")]
    FrontmatterParse {
        /// Catalog-relative path of the document.
        path: String,
        /// 1-based line number in the file.
        line: usize,
        /// The parser's message.
        message: String,
    },

    /// A frontmatter key has a shape the edit can't handle (e.g. `sources` is a string).
    #[error("{path}: {key} has an unexpected shape; fix it by hand")]
    Shape {
        /// Catalog-relative path of the document.
        path: String,
        /// The frontmatter key.
        key: String,
    },

    /// A filesystem operation failed.
    #[error("{path}: {source}")]
    Io {
        /// Catalog-relative path being read or written.
        path: String,
        /// The underlying I/O error.
        #[source]
        source: std::io::Error,
    },
}

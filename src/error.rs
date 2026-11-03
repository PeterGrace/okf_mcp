//! The crate's error type.

/// Errors from reading, parsing or editing an OKF catalog.
///
/// Later milestones add variants (auth, lint, publish); variants that
/// concern one document carry its catalog-relative path, so an agent can act
/// on it.
#[derive(Debug, thiserror::Error)]
pub enum OkfError {
    /// No concept or document at the given path.
    #[error("{path}: no such concept or document")]
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

    /// A write was refused by the secret scan. Only the rule, the line and
    /// the scan's message are kept; the message shows at most the value's
    /// first four characters, so the secret itself is never returned or logged.
    #[error("{path}: line {line}: write refused, {message} ({rule})")]
    SecretDetected {
        /// Catalog-relative path of the file.
        path: String,
        /// 1-based line of the first hit.
        line: usize,
        /// The scan rule, e.g. `secret:github-token`.
        rule: String,
        /// The scan's message, with the value masked.
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

    /// `edit_document`'s old text does not occur exactly once (or, with
    /// `replace_all`, not at all).
    #[error(
        "{path}: the old text occurs {matches} times; it must occur exactly once, \
         or at least once with replace_all"
    )]
    EditMismatch {
        /// Catalog-relative path of the document.
        path: String,
        /// How many times the old text occurs.
        matches: usize,
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

    /// An import was refused; every problem found is listed.
    // `.problems` in the format arguments refers to the field (thiserror syntax).
    #[error("import refused: {}", .problems.join("; "))]
    ImportRefused {
        /// One line per problem: the entry, and what is wrong with it.
        problems: Vec<String>,
    },

    /// A database operation failed. M4 logs the details and returns only the
    /// variant to MCP clients; the CLI prints them.
    #[error("database: {source}")]
    Database {
        /// The driver's error.
        // `#[source]`, not `#[from]`: the `From` impl below is written by
        // hand, so a deadlock becomes `Busy` instead.
        #[source]
        source: sqlx::Error,
    },

    /// Applying the schema migrations failed.
    #[error("migrations: {source}")]
    Migrate {
        /// The migrator's error.
        #[from]
        source: sqlx::migrate::MigrateError,
    },

    /// The configuration file is unreadable or invalid.
    #[error("{path}: {message}")]
    Config {
        /// The file, as given.
        path: String,
        /// What is wrong with it.
        message: String,
    },

    /// The named catalog does not exist (from M4, also: the key may not reach it).
    #[error("catalog {catalog:?} does not exist or is not available")]
    CatalogNotSelected {
        /// The catalog name as given.
        catalog: String,
    },

    /// The key reaches the catalog but its access level does not allow this.
    #[error("forbidden: {reason}")]
    Forbidden {
        /// What was refused, and the access it needs.
        reason: String,
    },

    /// An administrative request was refused; the reason says why and what to do.
    #[error("{reason}")]
    Refused {
        /// Why.
        reason: String,
    },

    /// The database aborted the request to keep others going (a deadlock or
    /// serialization failure), or the changeset changed under it. Nothing
    /// was stored: retry.
    #[error("busy: {reason}; retry")]
    Busy {
        /// What was aborted.
        reason: String,
    },
}

impl OkfError {
    /// The variant's name, as clients see it in a tool error's `error` field.
    ///
    /// # Returns
    /// The name, e.g. `"NotFound"`.
    pub fn kind(&self) -> &'static str {
        // `{ .. }` matches a variant whatever its fields hold. No `_` arm:
        // a new variant fails to compile here until it is given a name.
        match self {
            OkfError::NotFound { .. } => "NotFound",
            OkfError::InvalidPath { .. } => "InvalidPath",
            OkfError::FrontmatterParse { .. } => "FrontmatterParse",
            OkfError::SecretDetected { .. } => "SecretDetected",
            OkfError::Shape { .. } => "Shape",
            OkfError::EditMismatch { .. } => "EditMismatch",
            OkfError::Io { .. } => "Io",
            OkfError::ImportRefused { .. } => "ImportRefused",
            OkfError::Database { .. } => "Database",
            OkfError::Migrate { .. } => "Migrate",
            OkfError::Config { .. } => "Config",
            OkfError::CatalogNotSelected { .. } => "CatalogNotSelected",
            OkfError::Forbidden { .. } => "Forbidden",
            OkfError::Refused { .. } => "Refused",
            OkfError::Busy { .. } => "Busy",
        }
    }

    /// Is this the server's fault, with details for the log only?
    ///
    /// # Returns
    /// `true` for database, migration, filesystem and configuration failures:
    /// the errors the HTTP routes answer with 500.
    pub fn is_internal(&self) -> bool {
        matches!(
            self,
            OkfError::Database { .. }
                | OkfError::Migrate { .. }
                | OkfError::Io { .. }
                | OkfError::Config { .. }
        )
    }

    /// The error as clients see it: `{"error": kind, "message": text}`, plus
    /// the `path`, `catalog` or `problems` it concerns.
    ///
    /// # Returns
    /// The JSON body. An internal error's message says only that the server
    /// log has the details, so a driver message never reaches a client.
    pub fn to_json(&self) -> serde_json::Value {
        let message = if self.is_internal() {
            "internal error; the server log has the details".to_string()
        } else {
            // `to_string` comes from the `#[error(...)]` text (thiserror
            // implements `Display`), like Python's `str(exc)`.
            self.to_string()
        };
        let mut body = serde_json::json!({ "error": self.kind(), "message": message });
        // One arm can bind `path` from several variants (`|`) because each
        // has a field of that name and type.
        match self {
            OkfError::NotFound { path }
            | OkfError::InvalidPath { path, .. }
            | OkfError::FrontmatterParse { path, .. }
            | OkfError::Shape { path, .. }
            | OkfError::Io { path, .. } => body["path"] = path.as_str().into(),
            OkfError::SecretDetected {
                path, line, rule, ..
            } => {
                body["path"] = path.as_str().into();
                body["line"] = (*line).into();
                body["rule"] = rule.as_str().into();
            }
            OkfError::EditMismatch { path, matches } => {
                body["path"] = path.as_str().into();
                // `*matches` copies the `usize` out of the borrowed field.
                body["matches"] = (*matches).into();
            }
            OkfError::CatalogNotSelected { catalog } => body["catalog"] = catalog.as_str().into(),
            OkfError::ImportRefused { problems } => body["problems"] = problems.as_slice().into(),
            OkfError::Database { .. }
            | OkfError::Migrate { .. }
            | OkfError::Config { .. }
            | OkfError::Forbidden { .. }
            | OkfError::Refused { .. }
            | OkfError::Busy { .. } => {}
        }
        body
    }
}

/// SQLSTATEs that mean "Postgres aborted this transaction so others could go
/// on; running it again will likely succeed": `40P01` (deadlock detected) and
/// `40001` (serialization failure, from a repeatable-read snapshot).
const RETRYABLE_SQLSTATES: [&str; 2] = ["40P01", "40001"];

// The conversion `?` uses for every driver error: a retryable failure is
// `Busy`, anything else `Database`. (Before M5, `#[from]` generated this
// impl without the check.)
impl From<sqlx::Error> for OkfError {
    fn from(source: sqlx::Error) -> Self {
        // `code()` borrows `source`, so the check finishes before `source`
        // moves into the variant below. It returns `Option<Cow<str>>`, and
        // `as_ref()` views the `Cow` as a `&str`.
        let retryable = source
            .as_database_error()
            .and_then(|db_error| db_error.code())
            .is_some_and(|code| RETRYABLE_SQLSTATES.contains(&code.as_ref()));
        if retryable {
            // `Busy` drops the driver error, and as a non-internal error it is
            // logged only at debug level, so keep Postgres' report (the
            // statement and the conflicting lock, never document text) here.
            // `?source` logs its `Debug` form, which includes the detail.
            tracing::warn!(error = ?source, "retryable database error, answered Busy");
            OkfError::Busy {
                reason: "the database aborted this request to resolve a lock conflict".to_string(),
            }
        } else {
            OkfError::Database { source }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn to_json_names_the_kind_and_what_to_act_on() {
        let not_found = OkfError::NotFound {
            path: "/x.md".to_string(),
        };
        let unselected = OkfError::CatalogNotSelected {
            catalog: "work".to_string(),
        };
        let refused = OkfError::ImportRefused {
            problems: vec!["a: bad".to_string()],
        };

        let not_found_body = not_found.to_json();
        let unselected_body = unselected.to_json();
        let refused_body = refused.to_json();

        assert_eq!(
            not_found_body,
            serde_json::json!({
                "error": "NotFound",
                "message": "/x.md: no such concept or document",
                "path": "/x.md",
            })
        );
        assert_eq!(unselected_body["error"], "CatalogNotSelected");
        assert_eq!(unselected_body["catalog"], "work");
        assert_eq!(refused_body["problems"], serde_json::json!(["a: bad"]));
    }

    #[test]
    fn secret_detected_names_the_rule_and_line_but_not_the_value() {
        let error = OkfError::SecretDetected {
            path: "notes.md".to_string(),
            line: 6,
            rule: "secret:assignment".to_string(),
            message: "possible secret: 'password' followed by a literal value Ab1A... \
                      (value mixes letters and digits)"
                .to_string(),
        };

        let body = error.to_json();

        assert!(!error.is_internal());
        assert_eq!(body["error"], "SecretDetected");
        assert_eq!(body["path"], "notes.md");
        assert_eq!(body["line"], 6);
        assert_eq!(body["rule"], "secret:assignment");
        assert_eq!(
            body["message"],
            "notes.md: line 6: write refused, possible secret: 'password' followed by a \
             literal value Ab1A... (value mixes letters and digits) (secret:assignment)"
        );
    }

    #[test]
    fn internal_errors_hide_their_details() {
        let error = OkfError::Database {
            source: sqlx::Error::PoolTimedOut,
        };

        let body = error.to_json();

        assert!(error.is_internal());
        assert_eq!(body["error"], "Database");
        assert!(!body["message"].as_str().unwrap().contains("pool"));
        assert_eq!(body.as_object().unwrap().len(), 2);
    }

    #[test]
    fn filesystem_errors_are_internal_and_keep_only_their_path() {
        let error = OkfError::Io {
            path: "a.md".to_string(),
            source: std::io::Error::other("disk on fire"),
        };

        let body = error.to_json();

        assert!(error.is_internal());
        assert_eq!(
            body,
            serde_json::json!({
                "error": "Io",
                "message": "internal error; the server log has the details",
                "path": "a.md",
            })
        );
    }

    #[test]
    fn edit_mismatch_names_the_path_and_the_count() {
        let error = OkfError::EditMismatch {
            path: "a.md".to_string(),
            matches: 2,
        };

        let body = error.to_json();

        assert!(!error.is_internal());
        assert_eq!(
            body,
            serde_json::json!({
                "error": "EditMismatch",
                "message": "a.md: the old text occurs 2 times; it must occur exactly once, \
                            or at least once with replace_all",
                "path": "a.md",
                "matches": 2,
            })
        );
    }
}

//! Replay operations: how a bookkeeping edit of `log.md` or an `index.md`
//! is redone on newer text at publish (design "Data flow", "Write" step 3).
//! Task 3 adds the functions that apply them.

use serde::{Deserialize, Serialize};

/// One replayable bookkeeping edit, stored in a changeset entry's `replay`
/// column as JSON (`{"op": "log", "date": ..., "bullet": ...}`).
///
/// `#[serde(tag = "op")]` writes the variant name into an `op` field next
/// to the variant's own fields, like a Python dict with a "type" key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum ReplayOp {
    /// `log`: add `bullet` under `## <date>` in `log.md`.
    Log {
        /// The UTC date of the heading, `2026-11-03`.
        date: String,
        /// The whole bullet line, `* **Update:** text`.
        bullet: String,
    },
    /// `index` and folder listings: add `bullet` to an `index.md` unless it
    /// already links `target`.
    ListBullet {
        /// Catalog-relative path the bullet lists (a concept, or a folder's
        /// `index.md`).
        target: String,
        /// The whole bullet line.
        bullet: String,
        /// The heading to put it under; `None` for `okf.py`'s default place.
        section: Option<String>,
        /// Create the heading when it is missing (folder listings under `# Sections`).
        create_section: bool,
    },
    /// `ensure_index`: create a folder's `index.md` unless it exists.
    CreateIndex {
        /// Catalog-relative folder, `infra/hosts`.
        folder: String,
    },
}

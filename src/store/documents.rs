//! Document rows: the columns derived from content, a catalog's published
//! documents, and the writers import and publish share.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgExecutor};

use crate::error::OkfError;
use crate::okf::analysis::lint::CommitTimes;
use crate::okf::snapshot::{DocKind, SnapshotDoc};
use crate::okf::workspace::StoredFile;
use crate::okf::yaml::Value;

/// A live (published, not deleted) document.
// `derive(sqlx::FromRow)` maps a result row's columns onto the fields by name.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct DocumentRow {
    /// Catalog-relative path.
    pub path: String,
    /// The exact bytes.
    pub content: Vec<u8>,
    /// The execute bit.
    pub executable: bool,
    /// Its current revision.
    pub revision: i32,
    /// When that revision was written (server clock).
    pub updated_at: DateTime<Utc>,
}

/// Every live document of a catalog, by path.
///
/// # Arguments
/// * `executor` - a pool, connection or transaction (`&mut *tx`).
/// * `catalog_id` - the catalog's id.
///
/// # Returns
/// The rows without tombstones, ordered by path.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn published<'e>(
    executor: impl PgExecutor<'e>,
    catalog_id: i64,
) -> Result<Vec<DocumentRow>, OkfError> {
    let rows = sqlx::query_as::<_, DocumentRow>(
        "SELECT path, content, executable, revision, updated_at FROM documents \
         WHERE catalog_id = $1 AND deleted_at IS NULL ORDER BY path",
    )
    .bind(catalog_id)
    .fetch_all(executor)
    .await?;
    Ok(rows)
}

/// Lint's commit times from stored documents: each live document's last
/// revision time stands in for its last git commit (design "Read tools").
///
/// After a first import every document's time is the import time, so
/// `generated-after-commit` flags only stamps later than that import.
///
/// # Arguments
/// * `rows` - the live documents, as [`published`] returns them.
///
/// # Returns
/// Times keyed by catalog-relative path (the stored paths already have the
/// spelling lint looks up); no dirty paths (M5 adds a session's changeset
/// paths).
pub fn commit_times(rows: &[DocumentRow]) -> CommitTimes {
    let times = rows
        .iter()
        // `fixed_offset()` turns a UTC time into one with an explicit `+00:00`
        // offset, the type lint compares stamps with.
        .map(|row| (row.path.clone(), row.updated_at.fixed_offset()))
        .collect();
    CommitTimes::new(times, HashSet::new())
}

/// The columns derived from a document's content (design "Storage"),
/// recomputed on every write so they never disagree with it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Derived {
    /// `concept`, `index`, `log` or `machinery`; `None` for files the
    /// snapshot does not hold (non-markdown files, skipped directories).
    // `&'static str`: always one of four string literals, so no allocation.
    pub kind: Option<&'static str>,
    /// Frontmatter `type`, if a string.
    pub concept_type: Option<String>,
    /// The title (frontmatter `title`, else the first heading).
    pub title: Option<String>,
    /// Frontmatter `status`, if a string.
    pub status: Option<String>,
    /// Frontmatter `tags`: the string items of a list.
    pub tags: Vec<String>,
}

impl Derived {
    /// The derived columns of a snapshot document.
    ///
    /// # Arguments
    /// * `doc` - the document; `None` (a file the snapshot does not hold, such
    ///   as an image or a script) gives all-empty columns.
    ///
    /// # Returns
    /// The columns to store beside the content.
    pub fn of(doc: Option<&SnapshotDoc>) -> Self {
        // `let ... else` is an early return when the pattern does not match,
        // like `if doc is None: return Derived()`.
        let Some(doc) = doc else {
            return Self::default();
        };
        let data = doc.frontmatter().data();
        // A closure (Python lambda) reading a string-valued frontmatter key.
        let text = |key: &str| {
            data.and_then(|data| data.get(key))
                .and_then(Value::as_str)
                .map(str::to_string)
        };
        let tags = match data.and_then(|data| data.get("tags")) {
            Some(Value::Seq(items)) => items
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_string)
                .collect(),
            Some(Value::Null | Value::Str(_) | Value::Map(_)) | None => Vec::new(),
        };
        Self {
            kind: Some(kind_name(doc.kind())),
            concept_type: text("type"),
            title: doc.title().map(str::to_string),
            status: text("status"),
            tags,
        }
    }
}

/// Which document one write touches, at which revision, by whom and when.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DocumentWrite<'a> {
    /// The catalog.
    pub catalog_id: i64,
    /// Catalog-relative path, as stored.
    pub path: &'a str,
    /// The new revision (the old one + 1, tombstones included; 1 for a new path).
    pub revision: i32,
    /// Recorded as `updated_by` and the revision's `actor`.
    pub actor: &'a str,
    /// Recorded as `updated_at` (and `deleted_at`) and the revision's `created_at`.
    pub now: DateTime<Utc>,
}

/// A revision's operation (the schema's `CHECK` list).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevisionOp {
    /// A publish wrote a path that had no live document.
    Create,
    /// A publish changed a live document.
    Update,
    /// A publish or an import removed a live document.
    Delete,
    /// An import added or changed a document.
    Import,
}

impl RevisionOp {
    /// The spelling stored in `revisions.op`.
    ///
    /// # Returns
    /// `"create"`, `"update"`, `"delete"` or `"import"`.
    pub fn as_str(self) -> &'static str {
        match self {
            RevisionOp::Create => "create",
            RevisionOp::Update => "update",
            RevisionOp::Delete => "delete",
            RevisionOp::Import => "import",
        }
    }
}

/// What a `revisions` row records besides the document's identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevisionRecord<'a> {
    /// The operation.
    pub op: RevisionOp,
    /// The content written (`None` for a delete: the row's content is NULL).
    pub file: Option<&'a StoredFile>,
    /// The changeset published (`None` for an import).
    pub changeset_id: Option<i64>,
    /// The publish message (`import` for an import).
    pub message: &'a str,
}

impl<'a> RevisionRecord<'a> {
    /// An import's record: `import` with the content, or `delete` without.
    ///
    /// # Arguments
    /// * `file` - the imported file, or `None` for a file the import drops.
    ///
    /// # Returns
    /// The record, with the message `import` and no changeset.
    pub fn import(file: Option<&'a StoredFile>) -> Self {
        Self {
            op: if file.is_some() {
                RevisionOp::Import
            } else {
                RevisionOp::Delete
            },
            file,
            changeset_id: None,
            message: "import",
        }
    }
}

/// Inserts or overwrites a document (reviving a tombstone), with its
/// derived columns.
///
/// # Arguments
/// * `conn` - the transaction's connection (pass `&mut tx`).
/// * `write` - path, revision, actor and time.
/// * `file` - the content and execute bit.
/// * `derived` - [`Derived::of`] the content's snapshot document.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn write_document(
    conn: &mut PgConnection,
    write: &DocumentWrite<'_>,
    file: &StoredFile,
    derived: &Derived,
) -> Result<(), OkfError> {
    sqlx::query(
        "INSERT INTO documents (catalog_id, path, content, executable, revision, deleted_at, \
                                updated_at, updated_by, kind, concept_type, title, status, tags) \
         VALUES ($1, $2, $3, $4, $5, NULL, $6, $7, $8, $9, $10, $11, $12) \
         ON CONFLICT (catalog_id, path) DO UPDATE SET \
             content = EXCLUDED.content, executable = EXCLUDED.executable, \
             revision = EXCLUDED.revision, deleted_at = NULL, \
             updated_at = EXCLUDED.updated_at, updated_by = EXCLUDED.updated_by, \
             kind = EXCLUDED.kind, concept_type = EXCLUDED.concept_type, \
             title = EXCLUDED.title, status = EXCLUDED.status, tags = EXCLUDED.tags",
    )
    .bind(write.catalog_id)
    .bind(write.path)
    .bind(&file.content)
    .bind(file.executable)
    .bind(write.revision)
    .bind(write.now)
    .bind(write.actor)
    .bind(derived.kind)
    .bind(&derived.concept_type)
    .bind(&derived.title)
    .bind(&derived.status)
    .bind(&derived.tags)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Turns a live document into a tombstone (empty content, no derived columns).
///
/// # Arguments
/// * `conn` - the transaction's connection.
/// * `write` - path, revision, actor and time (also `deleted_at`).
///
/// # Errors
/// [`OkfError::Database`].
pub async fn delete_document(
    conn: &mut PgConnection,
    write: &DocumentWrite<'_>,
) -> Result<(), OkfError> {
    sqlx::query(
        "UPDATE documents SET content = ''::bytea, executable = false, revision = $3, \
             deleted_at = $4, updated_at = $4, updated_by = $5, kind = NULL, \
             concept_type = NULL, title = NULL, status = NULL, tags = '{}' \
         WHERE catalog_id = $1 AND path = $2",
    )
    .bind(write.catalog_id)
    .bind(write.path)
    .bind(write.revision)
    .bind(write.now)
    .bind(write.actor)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Appends a history row.
///
/// # Arguments
/// * `conn` - the transaction's connection.
/// * `write` - path, revision, actor and time.
/// * `record` - op, content, changeset and message.
///
/// # Errors
/// [`OkfError::Database`]: a unique violation if the revision already
/// exists (which the publish lock rules out; publish maps it to a conflict).
pub async fn record_revision(
    conn: &mut PgConnection,
    write: &DocumentWrite<'_>,
    record: &RevisionRecord<'_>,
) -> Result<(), OkfError> {
    sqlx::query(
        "INSERT INTO revisions (catalog_id, path, revision, op, content, executable, actor, \
                                changeset_id, message, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)",
    )
    .bind(write.catalog_id)
    .bind(write.path)
    .bind(write.revision)
    .bind(record.op.as_str())
    .bind(record.file.map(|file| &file.content))
    .bind(record.file.is_some_and(|file| file.executable))
    .bind(write.actor)
    .bind(record.changeset_id)
    .bind(record.message)
    .bind(write.now)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// The spelling stored in `documents.kind` (the schema's `CHECK` list).
fn kind_name(kind: DocKind) -> &'static str {
    match kind {
        DocKind::Concept => "concept",
        DocKind::Index => "index",
        DocKind::Log => "log",
        DocKind::Machinery => "machinery",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;
    use crate::okf::snapshot::Snapshot;
    use crate::okf::workspace::MemWorkspace;
    use crate::store::catalogs;
    use chrono::TimeZone;

    #[test]
    fn commit_times_are_the_documents_update_times() {
        let at = Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap();
        let rows = [DocumentRow {
            path: "a.md".to_string(),
            content: Vec::new(),
            executable: false,
            revision: 1,
            updated_at: at,
        }];

        let times = commit_times(&rows);

        let expected = CommitTimes::new(
            [("a.md".to_string(), at.fixed_offset())]
                .into_iter()
                .collect(),
            HashSet::new(),
        );
        assert_eq!(times, expected);
    }

    #[test]
    fn derived_columns_come_from_the_frontmatter() {
        let snapshot = Snapshot::build(&MemWorkspace::from_files([
            (
                "infra/time.md",
                "---\ntype: System\ntitle: Time server\nstatus: draft\ntags: [ntp, gps]\n---\n\
                 Body\n",
            ),
            ("infra/index.md", "# Infra\n"),
            ("log.md", "# Log\n"),
            ("AGENTS.md", "# Agents\n"),
        ]))
        .unwrap();

        let concept = Derived::of(snapshot.get("infra/time.md"));

        assert_eq!(
            concept,
            Derived {
                kind: Some("concept"),
                concept_type: Some("System".to_string()),
                title: Some("Time server".to_string()),
                status: Some("draft".to_string()),
                tags: vec!["ntp".to_string(), "gps".to_string()],
            }
        );
        assert_eq!(
            Derived::of(snapshot.get("infra/index.md")).kind,
            Some("index")
        );
        assert_eq!(Derived::of(snapshot.get("log.md")).kind, Some("log"));
        assert_eq!(
            Derived::of(snapshot.get("AGENTS.md")).kind,
            Some("machinery")
        );
        assert_eq!(Derived::of(None), Derived::default());
    }

    #[test]
    fn an_import_record_is_import_or_delete_with_the_message_import() {
        let file = StoredFile {
            content: b"x".to_vec(),
            executable: false,
        };

        let added = RevisionRecord::import(Some(&file));
        let dropped = RevisionRecord::import(None);

        assert_eq!(
            (added.op.as_str(), added.message, added.changeset_id),
            ("import", "import", None)
        );
        assert_eq!(dropped.op, RevisionOp::Delete);
        assert_eq!(dropped.file, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn published_lists_live_documents_by_path() {
        let db = test_db().await;
        let at = Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap();
        let catalog = catalogs::create(&db.pool, "pg", "", crate::db::TEST_TEAM, at)
            .await
            .unwrap();
        let other = catalogs::create(&db.pool, "other", "", crate::db::TEST_TEAM, at)
            .await
            .unwrap();
        for (catalog_id, path, deleted) in [
            (catalog.id, "b.md", false),
            (catalog.id, "a.md", false),
            (catalog.id, "gone.md", true),
            (other.id, "c.md", false),
        ] {
            sqlx::query(
                "INSERT INTO documents (catalog_id, path, content, executable, revision, \
                                        deleted_at, updated_at, updated_by) \
                 VALUES ($1, $2, $3, false, 1, $4, $5, 'test')",
            )
            .bind(catalog_id)
            .bind(path)
            .bind(path.as_bytes())
            .bind(deleted.then_some(at))
            .bind(at)
            .execute(&db.pool)
            .await
            .unwrap();
        }

        let rows = published(&db.pool, catalog.id).await.unwrap();

        let paths: Vec<&str> = rows.iter().map(|row| row.path.as_str()).collect();
        assert_eq!(paths, ["a.md", "b.md"]);
        assert_eq!(
            rows[0],
            DocumentRow {
                path: "a.md".to_string(),
                content: b"a.md".to_vec(),
                executable: false,
                revision: 1,
                updated_at: at,
            }
        );
    }
}

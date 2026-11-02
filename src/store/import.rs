//! Import (design "Import"): load a catalog's files into the database.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgPool};

use crate::archive::Files;
use crate::error::OkfError;
use crate::okf::analysis::lint::{lint, LintReport};
use crate::okf::snapshot::Snapshot;
use crate::okf::workspace::{MemWorkspace, StoredFile};
use crate::store::catalogs;
use crate::store::documents::Derived;

/// How to import.
// `<'a>` is a lifetime parameter: the options borrow `actor` from the caller
// instead of copying it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportOptions<'a> {
    /// Replace a non-empty catalog's contents (write the differences).
    pub replace: bool,
    /// Recorded as `updated_by` and the revisions' `actor`.
    pub actor: &'a str,
    /// The time of every row written, and lint's "now" (server clock).
    pub now: DateTime<Utc>,
}

/// What an import did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    /// New paths (or paths whose document had been deleted).
    pub added: usize,
    /// Live documents whose content or execute bit changed.
    pub updated: usize,
    /// Live documents missing from the import, now deleted.
    pub deleted: usize,
    /// Live documents imported unchanged.
    pub unchanged: usize,
    /// Lint of the imported files (reported, never refused on).
    pub lint: LintReport,
}

/// A document row as import compares it (tombstones included).
#[derive(sqlx::FromRow)]
struct ExistingRow {
    path: String,
    content: Vec<u8>,
    executable: bool,
    revision: i32,
    deleted_at: Option<DateTime<Utc>>,
}

impl ExistingRow {
    /// Not a tombstone?
    fn is_live(&self) -> bool {
        self.deleted_at.is_none()
    }

    /// Live with exactly this content and execute bit?
    fn is_live_with(&self, file: &StoredFile) -> bool {
        self.is_live() && self.executable == file.executable && self.content == file.content
    }
}

/// The incoming files with their snapshot and lint, from [`analyze`].
struct Analyzed {
    files: Files,
    snapshot: Snapshot,
    lint: LintReport,
}

/// Imports files into a catalog, in one transaction under the catalog's lock.
///
/// Into an empty catalog every file is inserted; with `replace`, only the
/// differences are written: changed and new files get `import` revisions,
/// live documents missing from `files` become tombstones with `delete`
/// revisions (message `import`), and unchanged files keep their revision.
/// A path that had been deleted continues its revision numbers. The
/// catalog's `generation` is bumped when anything was written.
///
/// # Arguments
/// * `pool` - the database.
/// * `catalog` - the catalog's name (it must exist: `okf-mcp catalog create`).
/// * `files` - the files, from [`crate::archive`].
/// * `options` - replace, actor and time.
///
/// # Returns
/// The counts and the lint report.
///
/// # Errors
/// [`OkfError::ImportRefused`] if a markdown file is not UTF-8 text;
/// [`OkfError::InvalidPath`] if `files` holds paths no workspace can (see
/// [`MemWorkspace::from_stored`]); [`OkfError::CatalogNotSelected`] for an
/// unknown catalog, or one deleted while the import waited for its lock;
/// [`OkfError::Refused`] for a non-empty catalog without `replace`;
/// [`OkfError::Database`].
pub async fn import(
    pool: &PgPool,
    catalog: &str,
    files: Files,
    options: &ImportOptions<'_>,
) -> Result<ImportReport, OkfError> {
    let now = options.now;
    // Building a snapshot and linting it is CPU work (rayon across
    // documents): run it on a blocking thread so the async runtime's threads
    // stay free. `move` hands `files` to the closure, which hands it back.
    let analyzed = tokio::task::spawn_blocking(move || analyze(files, now))
        .await
        // A panic in the blocking task is re-raised here, as if it ran inline.
        // `into_panic` itself panics for a cancelled task, which only happens
        // when the runtime is shutting down.
        .unwrap_or_else(|error| std::panic::resume_unwind(error.into_panic()))?;
    let Analyzed {
        files,
        snapshot,
        lint,
    } = analyzed;
    let mut report = ImportReport {
        lint,
        ..ImportReport::default()
    };

    // Dropping `tx` without `commit()` (any `?` or early `return`) rolls back.
    let mut tx = pool.begin().await?;
    let found = catalogs::find(&mut *tx, catalog).await?;
    catalogs::lock(&mut tx, found.id).await?;
    // A delete holding the lock may have removed (or replaced) the catalog
    // while this import waited; look again now that nothing else can.
    if catalogs::find(&mut *tx, catalog).await?.id != found.id {
        return Err(OkfError::CatalogNotSelected {
            catalog: catalog.to_string(),
        });
    }
    let existing = existing_rows(&mut tx, found.id).await?;
    if !options.replace && existing.values().any(ExistingRow::is_live) {
        return Err(OkfError::Refused {
            reason: format!(
                "catalog {catalog:?} is not empty; pass --replace to replace its contents"
            ),
        });
    }
    for (path, file) in &files {
        let previous = existing.get(path);
        if previous.is_some_and(|row| row.is_live_with(file)) {
            report.unchanged += 1;
            continue;
        }
        // A tombstone keeps its revision, so a recreated path continues it.
        let revision = previous.map_or(1, |row| row.revision + 1);
        let derived = Derived::of(snapshot.get(path));
        let write = Write {
            catalog_id: found.id,
            path,
            revision,
            options,
        };
        write_document(&mut tx, &write, file, &derived).await?;
        record_revision(&mut tx, &write, Some(file)).await?;
        if previous.is_some_and(ExistingRow::is_live) {
            report.updated += 1;
        } else {
            report.added += 1;
        }
    }
    let missing = existing
        .values()
        .filter(|row| row.is_live() && !files.contains_key(&row.path));
    for row in missing {
        let write = Write {
            catalog_id: found.id,
            path: &row.path,
            revision: row.revision + 1,
            options,
        };
        delete_document(&mut tx, &write).await?;
        record_revision(&mut tx, &write, None).await?;
        report.deleted += 1;
    }
    if report.added + report.updated + report.deleted > 0 {
        sqlx::query("UPDATE catalogs SET generation = generation + 1 WHERE id = $1")
            .bind(found.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(report)
}

/// Refuses markdown that is not text, then builds and lints the snapshot of
/// the incoming files (runs on a blocking thread).
fn analyze(files: Files, now: DateTime<Utc>) -> Result<Analyzed, OkfError> {
    refuse_markdown_that_is_not_text(&files)?;
    let workspace = MemWorkspace::from_stored(files)?;
    let snapshot = Snapshot::build(&workspace)?;
    let lint = lint(&snapshot, now, None);
    Ok(Analyzed {
        files: workspace.into_stored(),
        snapshot,
        lint,
    })
}

/// A markdown file must be UTF-8 text: it is analyzed. Lists every one that
/// is not.
fn refuse_markdown_that_is_not_text(files: &Files) -> Result<(), OkfError> {
    let problems: Vec<String> = files
        .iter()
        .filter(|(path, file)| path.ends_with(".md") && std::str::from_utf8(&file.content).is_err())
        .map(|(path, _)| format!("{path}: a markdown file must be UTF-8 text"))
        .collect();
    if problems.is_empty() {
        Ok(())
    } else {
        Err(OkfError::ImportRefused { problems })
    }
}

/// Every row of the catalog, tombstones included, by path.
async fn existing_rows(
    conn: &mut PgConnection,
    catalog_id: i64,
) -> Result<HashMap<String, ExistingRow>, OkfError> {
    let rows = sqlx::query_as::<_, ExistingRow>(
        "SELECT path, content, executable, revision, deleted_at FROM documents \
         WHERE catalog_id = $1",
    )
    .bind(catalog_id)
    .fetch_all(&mut *conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| (row.path.clone(), row))
        .collect())
}

/// Which document one write touches, at which revision, by whom and when.
struct Write<'a> {
    catalog_id: i64,
    path: &'a str,
    revision: i32,
    options: &'a ImportOptions<'a>,
}

/// Inserts or overwrites a document (reviving a tombstone).
async fn write_document(
    conn: &mut PgConnection,
    write: &Write<'_>,
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
    .bind(write.options.now)
    .bind(write.options.actor)
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
async fn delete_document(conn: &mut PgConnection, write: &Write<'_>) -> Result<(), OkfError> {
    sqlx::query(
        "UPDATE documents SET content = ''::bytea, executable = false, revision = $3, \
             deleted_at = $4, updated_at = $4, updated_by = $5, kind = NULL, \
             concept_type = NULL, title = NULL, status = NULL, tags = '{}' \
         WHERE catalog_id = $1 AND path = $2",
    )
    .bind(write.catalog_id)
    .bind(write.path)
    .bind(write.revision)
    .bind(write.options.now)
    .bind(write.options.actor)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Appends a history row: an `import` with the content, or a `delete`
/// (`file` is `None`, content `NULL`); both with the message `import`.
async fn record_revision(
    conn: &mut PgConnection,
    write: &Write<'_>,
    file: Option<&StoredFile>,
) -> Result<(), OkfError> {
    let op = if file.is_some() { "import" } else { "delete" };
    sqlx::query(
        "INSERT INTO revisions (catalog_id, path, revision, op, content, executable, actor, \
                                message, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, 'import', $8)",
    )
    .bind(write.catalog_id)
    .bind(write.path)
    .bind(write.revision)
    .bind(op)
    .bind(file.map(|file| &file.content))
    .bind(file.is_some_and(|file| file.executable))
    .bind(write.options.actor)
    .bind(write.options.now)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;
    use chrono::TimeZone;

    fn at(minute: u32) -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 2, 12, minute, 0).unwrap()
    }

    fn options(replace: bool, now: DateTime<Utc>) -> ImportOptions<'static> {
        ImportOptions {
            replace,
            actor: "test/import",
            now,
        }
    }

    fn files(entries: &[(&str, &[u8], bool)]) -> Files {
        entries
            .iter()
            .map(|(path, content, executable)| {
                let file = StoredFile {
                    content: content.to_vec(),
                    executable: *executable,
                };
                (path.to_string(), file)
            })
            .collect()
    }

    /// `(path, revision, deleted, content)` of every row, by path.
    async fn rows(pool: &PgPool) -> Vec<(String, i32, bool, Vec<u8>)> {
        sqlx::query_as(
            "SELECT path, revision, deleted_at IS NOT NULL, content FROM documents ORDER BY path",
        )
        .fetch_all(pool)
        .await
        .unwrap()
    }

    async fn generation(pool: &PgPool) -> i64 {
        catalogs::find(pool, "pg").await.unwrap().generation
    }

    const CONCEPT: &[u8] = b"---\ntype: Note\ntitle: A\ntags: [x]\n---\nSee [b](b.md).\n";

    #[tokio::test(flavor = "multi_thread")]
    async fn an_import_fills_an_empty_catalog() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", at(0)).await.unwrap();
        let incoming = files(&[
            ("a.md", CONCEPT, false),
            ("index.md", b"# Index\n- [A](a.md)\n", false),
            ("tools/run.sh", b"#!/bin/sh\n", true),
            ("img/logo.png", b"\x89PNG\0\xff", false),
        ]);

        let report = import(&db.pool, "pg", incoming, &options(false, at(1)))
            .await
            .unwrap();

        assert_eq!(
            (
                report.added,
                report.updated,
                report.deleted,
                report.unchanged
            ),
            (4, 0, 0, 0)
        );
        // a.md links to a missing b.md: reported, not refused.
        assert!(report.lint.errors.iter().any(|f| f.rule == "broken-link"));
        type DerivedRow = (
            Option<String>,
            Option<String>,
            Option<String>,
            Vec<String>,
            DateTime<Utc>,
            String,
        );
        let derived: DerivedRow = sqlx::query_as(
            "SELECT kind, concept_type, title, tags, updated_at, updated_by FROM documents \
             WHERE path = 'a.md'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(
            derived,
            (
                Some("concept".into()),
                Some("Note".into()),
                Some("A".into()),
                vec!["x".into()],
                at(1),
                "test/import".into()
            )
        );
        let png: (Option<String>, Vec<u8>) =
            sqlx::query_as("SELECT kind, content FROM documents WHERE path = 'img/logo.png'")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(png, (None, b"\x89PNG\0\xff".to_vec()));
        let executable: bool =
            sqlx::query_scalar("SELECT executable FROM documents WHERE path = 'tools/run.sh'")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert!(executable);
        let history: Vec<(String, i32, String, String, DateTime<Utc>)> = sqlx::query_as(
            "SELECT path, revision, op, actor, created_at FROM revisions ORDER BY path",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert_eq!(history.len(), 4);
        assert!(history
            .iter()
            .all(|(_, revision, op, actor, created_at)| *revision == 1
                && op == "import"
                && actor == "test/import"
                && *created_at == at(1)));
        assert_eq!(generation(&db.pool).await, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_import_needs_replace_and_writes_only_differences() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", at(0)).await.unwrap();
        let first = files(&[
            ("keep.md", b"# Keep\n", false),
            ("change.md", b"# Old\n", false),
            ("drop.md", b"# Drop\n", false),
        ]);
        import(&db.pool, "pg", first.clone(), &options(false, at(1)))
            .await
            .unwrap();

        let refused = import(&db.pool, "pg", first, &options(false, at(2))).await;
        let second = files(&[
            ("keep.md", b"# Keep\n", false),
            ("change.md", b"# New\n", false),
            ("new.md", b"# New\n", false),
        ]);
        let report = import(&db.pool, "pg", second, &options(true, at(3)))
            .await
            .unwrap();

        assert!(
            matches!(refused, Err(OkfError::Refused { ref reason }) if reason.contains("--replace"))
        );
        assert_eq!(
            (
                report.added,
                report.updated,
                report.deleted,
                report.unchanged
            ),
            (1, 1, 1, 1)
        );
        assert_eq!(
            rows(&db.pool).await,
            [
                ("change.md".to_string(), 2, false, b"# New\n".to_vec()),
                ("drop.md".to_string(), 2, true, Vec::new()),
                ("keep.md".to_string(), 1, false, b"# Keep\n".to_vec()),
                ("new.md".to_string(), 1, false, b"# New\n".to_vec()),
            ]
        );
        type RevisionRow = (i32, String, Option<Vec<u8>>, String, DateTime<Utc>);
        let drop_history: Vec<RevisionRow> = sqlx::query_as(
            "SELECT revision, op, content, message, created_at FROM revisions \
                 WHERE path = 'drop.md' ORDER BY revision",
        )
        .fetch_all(&db.pool)
        .await
        .unwrap();
        assert_eq!(
            drop_history[1],
            (2, "delete".to_string(), None, "import".to_string(), at(3))
        );
        let tombstone: (Option<String>, Option<String>, DateTime<Utc>) =
            sqlx::query_as("SELECT kind, title, deleted_at FROM documents WHERE path = 'drop.md'")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(tombstone, (None, None, at(3)));
        assert_eq!(generation(&db.pool).await, 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_recreated_path_continues_its_revisions_and_a_same_import_changes_nothing() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", at(0)).await.unwrap();
        import(
            &db.pool,
            "pg",
            files(&[("a.md", b"1", false)]),
            &options(false, at(1)),
        )
        .await
        .unwrap();
        import(
            &db.pool,
            "pg",
            files(&[("b.md", b"b", false)]),
            &options(true, at(2)),
        )
        .await
        .unwrap();
        let both = files(&[("a.md", b"3", false), ("b.md", b"b", false)]);

        let recreated = import(&db.pool, "pg", both.clone(), &options(true, at(3)))
            .await
            .unwrap();
        let again = import(&db.pool, "pg", both, &options(true, at(4)))
            .await
            .unwrap();

        assert_eq!(
            (recreated.added, recreated.unchanged),
            (1, 1),
            "a revived tombstone counts as added"
        );
        assert_eq!(
            rows(&db.pool).await[0],
            ("a.md".to_string(), 3, false, b"3".to_vec())
        );
        assert_eq!(
            (again.added, again.updated, again.deleted, again.unchanged),
            (0, 0, 0, 2)
        );
        assert_eq!(generation(&db.pool).await, 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_changed_execute_bit_is_an_update() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", at(0)).await.unwrap();
        import(
            &db.pool,
            "pg",
            files(&[("run.sh", b"x", false)]),
            &options(false, at(1)),
        )
        .await
        .unwrap();

        let report = import(
            &db.pool,
            "pg",
            files(&[("run.sh", b"x", true)]),
            &options(true, at(2)),
        )
        .await
        .unwrap();

        assert_eq!((report.updated, report.unchanged), (1, 0));
        let history: Vec<(i32, bool)> =
            sqlx::query_as("SELECT revision, executable FROM revisions ORDER BY revision")
                .fetch_all(&db.pool)
                .await
                .unwrap();
        assert_eq!(history, [(1, false), (2, true)]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn markdown_that_is_not_text_and_unknown_catalogs_are_refused() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", at(0)).await.unwrap();
        let binary_markdown = files(&[
            ("a.md", b"\xff\xfe", false),
            ("b.md", b"# B\n", false),
            ("c.md", b"\xc3", false),
        ]);

        let binary = import(&db.pool, "pg", binary_markdown, &options(false, at(1))).await;
        let unknown = import(
            &db.pool,
            "nope",
            files(&[("a.md", b"# A\n", false)]),
            &options(false, at(1)),
        )
        .await;

        assert!(matches!(
            binary,
            Err(OkfError::ImportRefused { ref problems }) if problems == &[
                "a.md: a markdown file must be UTF-8 text",
                "c.md: a markdown file must be UTF-8 text",
            ]
        ));
        assert!(matches!(
            unknown,
            Err(OkfError::CatalogNotSelected { ref catalog }) if catalog == "nope"
        ));
        assert!(rows(&db.pool).await.is_empty());
        assert_eq!(generation(&db.pool).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_import_that_waited_for_a_delete_finds_the_catalog_gone() {
        let db = test_db().await;
        let catalog = catalogs::create(&db.pool, "pg", "", at(0)).await.unwrap();
        // Hold the catalog's lock, as a delete in progress would.
        let mut holder = db.pool.begin().await.unwrap();
        catalogs::lock(&mut holder, catalog.id).await.unwrap();
        let pool = db.pool.clone();
        // `tokio::spawn` runs the import concurrently; it blocks on the lock.
        let waiting = tokio::spawn(async move {
            import(
                &pool,
                "pg",
                files(&[("a.md", b"# A\n", false)]),
                &options(false, at(1)),
            )
            .await
        });
        catalogs::tests::until_a_lock_is_awaited(&db.pool).await;

        sqlx::query("DELETE FROM catalogs WHERE id = $1")
            .bind(catalog.id)
            .execute(&mut *holder)
            .await
            .unwrap();
        holder.commit().await.unwrap();
        let result = waiting.await.unwrap();

        assert!(matches!(result, Err(OkfError::CatalogNotSelected { .. })));
    }
}

//! Import (design "Import"): load a catalog's files into the database.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde::Serialize;
use sqlx::{PgConnection, PgPool};

use crate::archive::Files;
use crate::error::OkfError;
use crate::okf::analysis::lint::{lint, LintReport};
use crate::okf::rules::is_machinery;
use crate::okf::snapshot::Snapshot;
use crate::okf::workspace::{MemWorkspace, StoredFile};
use crate::store::catalogs;
use crate::store::documents::{self, Derived, DocumentWrite, RevisionRecord};

/// Which of a catalog's paths an import replaces.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum ImportScope {
    /// Every path: the catalog becomes exactly the imported files.
    #[default]
    Everything,
    /// Only repository machinery ([`is_machinery`]): the incoming files'
    /// machinery replaces the catalog's, the incoming concepts and other
    /// non-machinery files are ignored, and the catalog's own stay as they
    /// are. This is how machinery, read-only to every write tool, is updated
    /// without reverting a concept published since the files were exported.
    Machinery,
}

/// How to import.
// `<'a>` is a lifetime parameter: the options borrow `actor` from the caller
// instead of copying it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImportOptions<'a> {
    /// Replace a non-empty catalog's contents (write the differences).
    pub replace: bool,
    /// The paths replaced.
    pub scope: ImportScope,
    /// Refuse unless the catalog is at this generation (the one its export
    /// reported), so a publish since the export is not silently reverted.
    pub expect_generation: Option<i64>,
    /// Recorded as `updated_by` and the revisions' `actor`.
    pub actor: &'a str,
    /// The time of every row written, and lint's "now" (server clock).
    pub now: DateTime<Utc>,
}

/// What an import did. The path lists are sorted.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ImportReport {
    /// New paths (or paths whose document had been deleted).
    pub added: Vec<String>,
    /// Live documents whose content or execute bit changed.
    pub updated: Vec<String>,
    /// Live documents missing from the import (in its scope), now deleted.
    pub deleted: Vec<String>,
    /// How many live documents were imported unchanged (with
    /// [`ImportScope::Machinery`], every concept counts here).
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

/// The files to import with their snapshot and lint, from [`analyze`].
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
/// catalog's `generation` is bumped when anything was written. With
/// [`ImportScope::Machinery`] only machinery paths are compared and written
/// (the files imported are the catalog's live non-machinery documents plus
/// the incoming machinery), so a concept published meanwhile is kept.
///
/// The files are checked, snapshotted and linted under the catalog's lock,
/// because the machinery scope's snapshot includes the catalog's documents.
///
/// # Arguments
/// * `pool` - the database.
/// * `catalog` - the catalog's name (it must exist: `okf-mcp catalog create`).
/// * `files` - the files, from [`crate::archive`].
/// * `options` - replace, scope, expected generation, actor and time.
///
/// # Returns
/// The counts and the lint report.
///
/// # Errors
/// [`OkfError::ImportRefused`] if a markdown file is not UTF-8 text;
/// [`OkfError::InvalidPath`] if `files` holds paths no workspace can (see
/// [`MemWorkspace::from_stored`]); [`OkfError::CatalogNotSelected`] for an
/// unknown catalog, or one deleted while the import waited for its lock;
/// [`OkfError::Refused`] for a non-empty catalog without `replace`, or one
/// not at `expect_generation`; [`OkfError::Database`].
pub async fn import(
    pool: &PgPool,
    catalog: &str,
    files: Files,
    options: &ImportOptions<'_>,
) -> Result<ImportReport, OkfError> {
    let now = options.now;
    // Dropping `tx` without `commit()` (any `?` or early `return`) rolls back.
    let mut tx = pool.begin().await?;
    let found = catalogs::find(&mut *tx, catalog).await?;
    catalogs::lock(&mut tx, found.id).await?;
    // A delete holding the lock may have removed (or replaced) the catalog
    // while this import waited; look again now that nothing else can. The
    // second look is also the generation no publish can move any more.
    let locked = catalogs::find(&mut *tx, catalog).await?;
    if locked.id != found.id {
        return Err(OkfError::CatalogNotSelected {
            catalog: catalog.to_string(),
        });
    }
    if let Some(expected) = options.expect_generation {
        if locked.generation != expected {
            return Err(OkfError::Refused {
                reason: format!(
                    "catalog {catalog:?} is at generation {}, not {expected}: it changed \
                     since the export; export it again",
                    locked.generation
                ),
            });
        }
    }
    let existing = existing_rows(&mut tx, found.id).await?;
    if !options.replace && existing.values().any(ExistingRow::is_live) {
        return Err(OkfError::Refused {
            reason: format!(
                "catalog {catalog:?} is not empty; pass --replace to replace its contents"
            ),
        });
    }
    let files = match options.scope {
        ImportScope::Everything => files,
        ImportScope::Machinery => with_catalog_documents(files, &existing),
    };
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
    // `Files` is a `BTreeMap`, so its paths, and the lists built from them,
    // come out sorted.
    for (path, file) in &files {
        let previous = existing.get(path);
        if previous.is_some_and(|row| row.is_live_with(file)) {
            report.unchanged += 1;
            continue;
        }
        // A tombstone keeps its revision, so a recreated path continues it.
        let revision = previous.map_or(1, |row| row.revision + 1);
        let derived = Derived::of(snapshot.get(path));
        let write = DocumentWrite {
            catalog_id: found.id,
            path,
            revision,
            actor: options.actor,
            now,
        };
        documents::write_document(&mut tx, &write, file, &derived).await?;
        documents::record_revision(&mut tx, &write, &RevisionRecord::import(Some(file))).await?;
        if previous.is_some_and(ExistingRow::is_live) {
            report.updated.push(path.clone());
        } else {
            report.added.push(path.clone());
        }
    }
    // With the machinery scope, every live non-machinery row is in `files`,
    // so only machinery can be missing.
    let mut missing: Vec<&ExistingRow> = existing
        .values()
        .filter(|row| row.is_live() && !files.contains_key(&row.path))
        .collect();
    // `existing` is a `HashMap`, in no particular order.
    missing.sort_unstable_by(|a, b| a.path.cmp(&b.path));
    for row in missing {
        let write = DocumentWrite {
            catalog_id: found.id,
            path: &row.path,
            revision: row.revision + 1,
            actor: options.actor,
            now,
        };
        documents::delete_document(&mut tx, &write).await?;
        documents::record_revision(&mut tx, &write, &RevisionRecord::import(None)).await?;
        report.deleted.push(row.path.clone());
    }
    if !(report.added.is_empty() && report.updated.is_empty() && report.deleted.is_empty()) {
        sqlx::query("UPDATE catalogs SET generation = generation + 1 WHERE id = $1")
            .bind(found.id)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(report)
}

/// The incoming machinery plus the catalog's live non-machinery documents:
/// what the catalog holds after a [`ImportScope::Machinery`] import.
///
/// # Arguments
/// * `incoming` - the imported files; their non-machinery paths are dropped.
/// * `existing` - the catalog's rows, from [`existing_rows`].
///
/// # Returns
/// The files to import.
fn with_catalog_documents(mut incoming: Files, existing: &HashMap<String, ExistingRow>) -> Files {
    // `retain` is an in-place filter, like
    // `{k: v for k, v in d.items() if is_machinery(k)}` without a new dict.
    incoming.retain(|path, _| is_machinery(path));
    let kept = existing
        .values()
        .filter(|row| row.is_live() && !is_machinery(&row.path))
        .map(|row| {
            let file = StoredFile {
                content: row.content.clone(),
                executable: row.executable,
            };
            (row.path.clone(), file)
        });
    incoming.extend(kept);
    incoming
}

/// Refuses markdown that is not text, then builds and lints the snapshot of
/// the incoming files (runs on a blocking thread).
fn analyze(files: Files, now: DateTime<Utc>) -> Result<Analyzed, OkfError> {
    refuse_markdown_that_is_not_text(&files)?;
    let workspace = MemWorkspace::from_stored(files)?;
    let snapshot = Snapshot::build(&workspace)?;
    // `None`: commit times come from stored revisions, and these incoming
    // files are not stored yet, so `generated-after-commit` is skipped here.
    // Linting the loaded catalog (`crate::catalog::Loaded::lint`) runs it.
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
            scope: ImportScope::Everything,
            expect_generation: None,
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
            report.added,
            ["a.md", "img/logo.png", "index.md", "tools/run.sh"]
        );
        assert!(report.updated.is_empty() && report.deleted.is_empty());
        assert_eq!(report.unchanged, 0);
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
            (
                vec!["new.md".to_string()],
                vec!["change.md".to_string()],
                vec!["drop.md".to_string()],
                1
            )
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
            (vec!["a.md".to_string()], 1),
            "a revived tombstone counts as added"
        );
        assert_eq!(
            rows(&db.pool).await[0],
            ("a.md".to_string(), 3, false, b"3".to_vec())
        );
        assert!(again.added.is_empty() && again.updated.is_empty() && again.deleted.is_empty());
        assert_eq!(again.unchanged, 2);
        assert_eq!(generation(&db.pool).await, 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_machinery_import_replaces_machinery_and_keeps_every_concept() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", at(0)).await.unwrap();
        let exported = files(&[
            ("a.md", CONCEPT, false),
            ("AGENTS.md", b"# Old rules\n", false),
            ("tools/old.sh", b"old\n", true),
        ]);
        import(&db.pool, "pg", exported.clone(), &options(false, at(1)))
            .await
            .unwrap();
        // Published after the export: `b.md` is new and `a.md` changed.
        let published = files(&[
            ("a.md", b"# A, edited\nSee [b](b.md).\n", false),
            ("AGENTS.md", b"# Old rules\n", false),
            ("b.md", b"# B\n", false),
            ("tools/old.sh", b"old\n", true),
        ]);
        import(&db.pool, "pg", published, &options(true, at(2)))
            .await
            .unwrap();
        // The export with new machinery overlaid: its `a.md` is stale and it
        // has no `b.md`.
        let mut overlaid = exported;
        overlaid.remove("tools/old.sh");
        overlaid.extend(files(&[
            ("AGENTS.md", b"# New rules\n", false),
            ("tools/new.sh", b"new\n", true),
        ]));
        let machinery = ImportOptions {
            scope: ImportScope::Machinery,
            ..options(true, at(3))
        };

        let report = import(&db.pool, "pg", overlaid, &machinery).await.unwrap();

        assert_eq!(report.added, ["tools/new.sh"]);
        assert_eq!(report.updated, ["AGENTS.md"]);
        assert_eq!(report.deleted, ["tools/old.sh"]);
        assert_eq!(report.unchanged, 2, "both concepts are kept as published");
        // The snapshot linted holds the catalog's concepts: `a.md`'s link to
        // `b.md` resolves.
        assert!(!report.lint.errors.iter().any(|f| f.rule == "broken-link"));
        let live: Vec<(String, Vec<u8>)> = rows(&db.pool)
            .await
            .into_iter()
            .filter(|(_, _, deleted, _)| !deleted)
            .map(|(path, _, _, content)| (path, content))
            .collect();
        assert_eq!(
            live,
            [
                ("AGENTS.md".to_string(), b"# New rules\n".to_vec()),
                (
                    "a.md".to_string(),
                    b"# A, edited\nSee [b](b.md).\n".to_vec()
                ),
                ("b.md".to_string(), b"# B\n".to_vec()),
                ("tools/new.sh".to_string(), b"new\n".to_vec()),
            ]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_import_expecting_another_generation_is_refused() {
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
        let guarded = |generation| ImportOptions {
            expect_generation: Some(generation),
            ..options(true, at(2))
        };

        let stale = import(
            &db.pool,
            "pg",
            files(&[("a.md", b"stale", false)]),
            &guarded(0),
        )
        .await;
        let current = import(&db.pool, "pg", files(&[("a.md", b"2", false)]), &guarded(1))
            .await
            .unwrap();

        assert!(matches!(
            stale,
            Err(OkfError::Refused { ref reason })
                if reason.contains("generation 1, not 0")
        ));
        assert_eq!(current.updated, ["a.md"]);
        assert_eq!(
            rows(&db.pool).await,
            [("a.md".to_string(), 2, false, b"2".to_vec())]
        );
        assert_eq!(generation(&db.pool).await, 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_import_that_waited_for_a_publish_checks_the_generation_it_left() {
        let db = test_db().await;
        let catalog = catalogs::create(&db.pool, "pg", "", at(0)).await.unwrap();
        import(
            &db.pool,
            "pg",
            files(&[("a.md", b"1", false)]),
            &options(false, at(1)),
        )
        .await
        .unwrap();
        // Hold the catalog's lock, as a publish in progress would.
        let mut holder = db.pool.begin().await.unwrap();
        catalogs::lock(&mut holder, catalog.id).await.unwrap();
        let pool = db.pool.clone();
        // Expects generation 1, the one the catalog is at when it starts.
        let waiting = tokio::spawn(async move {
            let guarded = ImportOptions {
                expect_generation: Some(1),
                ..options(true, at(3))
            };
            import(&pool, "pg", files(&[("a.md", b"stale", false)]), &guarded).await
        });
        catalogs::tests::until_a_lock_is_awaited(&db.pool).await;

        // The publish the import waited behind: a new revision of `a.md` and
        // generation 2, as `store::publish` writes them.
        sqlx::query(
            "UPDATE documents SET content = 'published', revision = 2 \
             WHERE catalog_id = $1 AND path = 'a.md'",
        )
        .bind(catalog.id)
        .execute(&mut *holder)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO revisions (catalog_id, path, revision, op, content, executable, actor, \
             message, created_at) \
             VALUES ($1, 'a.md', 2, 'update', 'published', false, 'publisher', 'edit', $2)",
        )
        .bind(catalog.id)
        .bind(at(2))
        .execute(&mut *holder)
        .await
        .unwrap();
        sqlx::query("UPDATE catalogs SET generation = generation + 1 WHERE id = $1")
            .bind(catalog.id)
            .execute(&mut *holder)
            .await
            .unwrap();
        holder.commit().await.unwrap();
        let result = waiting.await.unwrap();

        assert!(matches!(
            result,
            Err(OkfError::Refused { ref reason })
                if reason.contains("generation 2, not 1")
        ));
        assert_eq!(
            rows(&db.pool).await,
            [("a.md".to_string(), 2, false, b"published".to_vec())]
        );
        let history: Vec<(i32, String)> =
            sqlx::query_as("SELECT revision, actor FROM revisions ORDER BY revision")
                .fetch_all(&db.pool)
                .await
                .unwrap();
        assert_eq!(
            history,
            [(1, "test/import".to_string()), (2, "publisher".to_string())]
        );
        assert_eq!(generation(&db.pool).await, 2);
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

        assert_eq!(
            (report.updated, report.unchanged),
            (vec!["run.sh".to_string()], 0)
        );
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

//! Per-catalog state held in memory (design "In-memory snapshot"): the
//! snapshot of the catalog's live documents, rebuilt and swapped when the
//! catalog's `generation` moves.
//!
//! Readers call [`CatalogState::current`] and keep the `Arc` for as long as
//! they need: a swap never changes a snapshot someone is reading, and readers
//! never wait on the database.

use std::sync::{Arc, RwLock};

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::error::OkfError;
use crate::okf::analysis::lint::{lint, CommitTimes, LintReport};
use crate::okf::snapshot::Snapshot;
use crate::okf::workspace::{MemWorkspace, StoredFile};
use crate::store::{catalogs, documents};

/// The catalog's current generation, or `None` if the row is gone.
const GENERATION: &str = "SELECT generation FROM catalogs WHERE id = $1";

/// One loaded state of a catalog. Immutable.
#[derive(Debug)]
pub struct Loaded {
    generation: i64,
    snapshot: Snapshot,
    commit_times: CommitTimes,
    files: usize,
    last_change: Option<DateTime<Utc>>,
}

impl Loaded {
    /// The catalog generation this was built from.
    pub fn generation(&self) -> i64 {
        self.generation
    }

    /// The snapshot of the live documents.
    pub fn snapshot(&self) -> &Snapshot {
        &self.snapshot
    }

    /// Lint's commit times: each live document's last revision time (after a
    /// first import, the import time).
    pub fn commit_times(&self) -> &CommitTimes {
        &self.commit_times
    }

    /// How many files the catalog holds (every stored file, markdown or not).
    pub fn files(&self) -> usize {
        self.files
    }

    /// The newest document change (`updated_at`), or `None` for an empty catalog.
    pub fn last_change(&self) -> Option<DateTime<Utc>> {
        self.last_change
    }

    /// Lints the snapshot with its commit times, so `generated-after-commit`
    /// runs.
    ///
    /// # Arguments
    /// * `now` - the current time (server clock).
    ///
    /// # Returns
    /// The findings, as [`lint`] returns them.
    pub fn lint(&self, now: DateTime<Utc>) -> LintReport {
        lint(&self.snapshot, now, Some(&self.commit_times))
    }
}

/// A catalog held in memory.
#[derive(Debug)]
pub struct CatalogState {
    id: i64,
    name: String,
    description: String,
    // `RwLock<Arc<_>>`: the lock is held only to clone or replace the `Arc`
    // (a pointer copy plus a reference count), never while a snapshot is read
    // or built. It is `std`'s lock, not tokio's: it is never held across an
    // `.await`, so it never blocks an async thread for longer than that copy.
    current: RwLock<Arc<Loaded>>,
    // Held for the length of one rebuild, so refreshes of this catalog build
    // one at a time instead of each building the same snapshot. tokio's
    // `Mutex`, unlike `std`'s, may be held across an `.await`: waiting for it
    // suspends the task instead of blocking the thread.
    rebuilding: tokio::sync::Mutex<()>,
}

impl CatalogState {
    /// Loads a catalog: its current generation and snapshot.
    ///
    /// # Arguments
    /// * `pool` - the database.
    /// * `name` - the catalog's name.
    ///
    /// # Returns
    /// The state, holding the snapshot of the catalog's live documents.
    ///
    /// # Errors
    /// [`OkfError::CatalogNotSelected`] if there is no such catalog;
    /// [`OkfError::InvalidPath`] or another workspace error if the stored
    /// rows cannot form a workspace; [`OkfError::Database`].
    pub async fn load(pool: &PgPool, name: &str) -> Result<Self, OkfError> {
        let catalog = catalogs::find(pool, name).await?;
        let loaded = build(pool, catalog.id, &catalog.name).await?;
        Ok(Self {
            id: catalog.id,
            name: catalog.name,
            description: catalog.description,
            current: RwLock::new(Arc::new(loaded)),
            rebuilding: tokio::sync::Mutex::new(()),
        })
    }

    /// The catalog's database id.
    pub fn id(&self) -> i64 {
        self.id
    }

    /// The catalog's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The catalog's description (`catalog create --description`).
    pub fn description(&self) -> &str {
        &self.description
    }

    /// The current loaded state. Cheap: it clones an `Arc` (bumps a
    /// reference count), and the caller may keep it as long as it likes.
    pub fn current(&self) -> Arc<Loaded> {
        // `read()` fails only if a thread panicked while holding the lock
        // ("poisoned"); nothing here can panic with it held.
        Arc::clone(&self.current.read().expect("catalog state lock poisoned"))
    }

    /// Rebuilds and swaps in the snapshot if the catalog changed since it was
    /// loaded.
    ///
    /// Concurrent refreshes of one catalog are safe and build at most once
    /// per change: a refresh that waited for another's rebuild returns `false`
    /// if that rebuild already reached the generation it saw.
    ///
    /// # Arguments
    /// * `pool` - the database.
    ///
    /// # Returns
    /// Whether a new snapshot was swapped in.
    ///
    /// # Errors
    /// [`OkfError::CatalogNotSelected`] if the catalog was deleted;
    /// [`OkfError::Database`]; a workspace error as for [`CatalogState::load`].
    pub async fn refresh(&self, pool: &PgPool) -> Result<bool, OkfError> {
        let generation: Option<i64> = sqlx::query_scalar(GENERATION)
            .bind(self.id)
            .fetch_optional(pool)
            .await?;
        let Some(generation) = generation else {
            return Err(OkfError::CatalogNotSelected {
                catalog: self.name.clone(),
            });
        };
        // Generations only grow, so `<=` means "already this new or newer".
        if generation <= self.current().generation {
            return Ok(false);
        }
        // The guard (`_` prefix: kept only for its lifetime) releases the
        // mutex when it goes out of scope at the end of the function.
        let _rebuilding = self.rebuilding.lock().await;
        // Another refresh may have swapped in this generation while we waited.
        if generation <= self.current().generation {
            return Ok(false);
        }
        // Swaps happen only under `rebuilding`, so this build, which read a
        // generation at least as new as `generation`, is newer than `current`.
        let loaded = build(pool, self.id, &self.name).await?;
        // A statement-long write lock, taken after the last `.await`: the
        // `std` lock is never held across one.
        *self.current.write().expect("catalog state lock poisoned") = Arc::new(loaded);
        Ok(true)
    }
}

/// Reads a catalog's generation and live documents consistently, then builds
/// the snapshot on a blocking thread.
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] (naming `name`) if the row is gone;
/// [`OkfError::Database`]; a workspace or snapshot error.
async fn build(pool: &PgPool, catalog_id: i64, name: &str) -> Result<Loaded, OkfError> {
    // A repeatable-read transaction sees one moment of the database, so the
    // generation and the rows agree even if an import commits meanwhile.
    // `begin_with` sets the isolation level in the `BEGIN` itself.
    let mut tx = pool
        .begin_with("BEGIN ISOLATION LEVEL REPEATABLE READ, READ ONLY")
        .await?;
    // `&mut *tx` reborrows the transaction as a connection for one query.
    let generation: Option<i64> = sqlx::query_scalar(GENERATION)
        .bind(catalog_id)
        .fetch_optional(&mut *tx)
        .await?;
    // A catalog deleted between `find` (or `refresh`'s check) and here.
    let Some(generation) = generation else {
        return Err(OkfError::CatalogNotSelected {
            catalog: name.to_string(),
        });
    };
    let rows = documents::published(&mut *tx, catalog_id).await?;
    tx.commit().await?;
    // Before `rows` moves into the closure below: it borrows the rows.
    let commit_times = documents::commit_times(&rows);
    let files = rows.len();
    // `max()` over an empty iterator is `None`, so an empty catalog has no
    // last change.
    let last_change = rows.iter().map(|row| row.updated_at).max();
    // Building a snapshot is CPU work (rayon across documents): run it on a
    // blocking thread so the async runtime's threads stay free. `move` hands
    // `rows` to the closure.
    let snapshot = tokio::task::spawn_blocking(move || {
        let files = rows.into_iter().map(|row| {
            let file = StoredFile {
                content: row.content,
                executable: row.executable,
            };
            (row.path, file)
        });
        let workspace = MemWorkspace::from_stored(files)?;
        Snapshot::build(&workspace)
    })
    .await
    // A panic in the blocking task is re-raised here, as if it ran inline.
    .unwrap_or_else(|error| std::panic::resume_unwind(error.into_panic()))?;
    Ok(Loaded {
        generation,
        snapshot,
        commit_times,
        files,
        last_change,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::Files;
    use crate::db::test_db;
    use crate::store::catalogs;
    use crate::store::import::{import, ImportOptions};
    use chrono::{TimeDelta, TimeZone};

    fn files(entries: &[(&str, &[u8])]) -> Files {
        entries
            .iter()
            .map(|(path, content)| {
                let file = StoredFile {
                    content: content.to_vec(),
                    executable: false,
                };
                (path.to_string(), file)
            })
            .collect()
    }

    async fn import_into(pool: &PgPool, entries: &[(&str, &[u8])], replace: bool) {
        let options = ImportOptions {
            replace,
            actor: "t",
            now: Utc::now(),
        };
        import(pool, "pg", files(entries), &options).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn load_builds_the_snapshot_of_the_live_documents() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", Utc::now())
            .await
            .unwrap();
        // A plain link: `okf.py` does not count an image embed (`![..](..)`) as a link.
        let entries: &[(&str, &[u8])] = &[
            ("a.md", b"---\ntype: Note\n---\n[logo](logo.png)\n"),
            ("logo.png", b"\x89PNG\0"),
        ];
        import_into(&db.pool, entries, false).await;

        let state = CatalogState::load(&db.pool, "pg").await.unwrap();

        let loaded = state.current();
        assert_eq!((state.name(), loaded.generation()), ("pg", 1));
        assert_eq!(loaded.snapshot().docs().len(), 1);
        // The image is in the workspace, so the link to it resolves.
        assert!(loaded.snapshot().get("a.md").unwrap().links()[0].target_exists);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn load_records_the_description_file_count_and_last_change() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "pgrace's world", Utc::now())
            .await
            .unwrap();
        let empty = CatalogState::load(&db.pool, "pg").await.unwrap();
        assert_eq!(empty.description(), "pgrace's world");
        assert_eq!(empty.current().files(), 0);
        assert_eq!(empty.current().last_change(), None);
        let imported = Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap();
        let options = ImportOptions {
            replace: false,
            actor: "t",
            now: imported,
        };
        let entries = files(&[("a.md", b"# A\n"), ("b.txt", b"b\n")]);
        import(&db.pool, "pg", entries, &options).await.unwrap();

        empty.refresh(&db.pool).await.unwrap();

        let loaded = empty.current();
        assert_eq!(loaded.files(), 2);
        assert_eq!(loaded.last_change(), Some(imported));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn load_refuses_an_unknown_catalog() {
        let db = test_db().await;

        let result = CatalogState::load(&db.pool, "nope").await;

        assert!(matches!(result, Err(OkfError::CatalogNotSelected { .. })));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn refresh_swaps_in_a_new_snapshot_only_when_the_catalog_changed() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", Utc::now())
            .await
            .unwrap();
        import_into(&db.pool, &[("a.md", b"# Old\n")], false).await;
        let state = CatalogState::load(&db.pool, "pg").await.unwrap();
        let before = state.current();

        assert!(
            !state.refresh(&db.pool).await.unwrap(),
            "nothing changed yet"
        );
        import_into(&db.pool, &[("a.md", b"# New\n")], true).await;
        assert!(state.refresh(&db.pool).await.unwrap());

        let after = state.current();
        assert_eq!(after.generation(), 2);
        assert_eq!(after.snapshot().get("a.md").unwrap().text(), "# New\n");
        // A reader that took the old snapshot still sees it, whole.
        assert_eq!(before.snapshot().get("a.md").unwrap().text(), "# Old\n");
        assert!(!state.refresh(&db.pool).await.unwrap());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_refreshes_build_and_swap_once() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", Utc::now())
            .await
            .unwrap();
        let state = CatalogState::load(&db.pool, "pg").await.unwrap();
        import_into(&db.pool, &[("a.md", b"# New\n")], false).await;

        // `tokio::join!` runs both futures at once on this task.
        let (first, second) = tokio::join!(state.refresh(&db.pool), state.refresh(&db.pool));

        assert_eq!(
            [first.unwrap(), second.unwrap()]
                .iter()
                .filter(|&&swapped| swapped)
                .count(),
            1
        );
        assert_eq!(state.current().generation(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_deleted_catalog_cannot_be_refreshed() {
        let db = test_db().await;
        let catalog = catalogs::create(&db.pool, "pg", "", Utc::now())
            .await
            .unwrap();
        let state = CatalogState::load(&db.pool, "pg").await.unwrap();
        catalogs::delete(&db.pool, "pg", false).await.unwrap();

        let result = state.refresh(&db.pool).await;

        assert_eq!(state.id(), catalog.id);
        assert!(matches!(result, Err(OkfError::CatalogNotSelected { .. })));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn build_names_a_catalog_deleted_before_it_read() {
        let db = test_db().await;
        let catalog = catalogs::create(&db.pool, "pg", "", Utc::now())
            .await
            .unwrap();
        catalogs::delete(&db.pool, "pg", false).await.unwrap();

        let result = build(&db.pool, catalog.id, "pg").await;

        assert!(matches!(
            result,
            Err(OkfError::CatalogNotSelected { catalog }) if catalog == "pg"
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lint_flags_a_stamp_later_than_the_documents_last_revision() {
        let db = test_db().await;
        let imported = Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap();
        catalogs::create(&db.pool, "pg", "", imported)
            .await
            .unwrap();
        let later = "---\ntype: Note\ngenerated: { by: t, at: 2026-11-02T12:10:00Z }\n---\nBody\n";
        let earlier =
            "---\ntype: Note\ngenerated: { by: t, at: 2026-11-02T11:00:00Z }\n---\nBody\n";
        let options = ImportOptions {
            replace: false,
            actor: "t",
            now: imported,
        };
        let entries = files(&[
            ("later.md", later.as_bytes()),
            ("earlier.md", earlier.as_bytes()),
        ]);
        import(&db.pool, "pg", entries, &options).await.unwrap();
        let state = CatalogState::load(&db.pool, "pg").await.unwrap();

        let report = state.current().lint(imported + TimeDelta::minutes(30));

        let flagged: Vec<&str> = report
            .warnings
            .iter()
            .filter(|finding| finding.rule == "generated-after-commit")
            .map(|finding| finding.file.as_str())
            .collect();
        assert_eq!(flagged, ["later.md"]);
    }
}

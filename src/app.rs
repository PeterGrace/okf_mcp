//! The server's shared state (design "Architecture"): the database, the
//! configuration, the clock, the key cache and every catalog held in
//! memory, and the poll loop that notices changes made outside the server
//! (design "Noticing changes made outside the server").

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, RwLock};

use sqlx::PgPool;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::auth::Keys;
use crate::catalog::CatalogState;
use crate::clock::Clock;
use crate::config::Config;
use crate::error::OkfError;
use crate::store::catalogs;

/// Every loaded catalog, by name (a `BTreeMap`, so iteration is in name
/// order, like `catalog list`).
pub type Catalogs = BTreeMap<String, Arc<CatalogState>>;

/// Catalogs whose stored data failed to build into a snapshot: each one's id,
/// and the generation that failed. Building the same generation again would
/// fail the same way, so it is retried only once the generation moves.
type Broken = HashMap<i64, i64>;

/// The state every route and tool shares. Built once by [`App::load`] and
/// shared as an `Arc<App>`.
pub struct App {
    pool: PgPool,
    config: Config,
    // `Arc<dyn Clock>`: any clock (the real one, or a fixed one in tests),
    // shared between threads. `dyn Clock` is a trait object: Python's duck
    // typing, with the method looked up at run time.
    clock: Arc<dyn Clock>,
    keys: Arc<Keys>,
    // As in `CatalogState`: the lock is held only to clone or swap the `Arc`.
    catalogs: RwLock<Arc<Catalogs>>,
    // The `admin_generation` the key cache was loaded at. Written only under
    // `polling`, so `Relaxed` ordering is enough.
    admin_generation: AtomicI64,
    // Held for a whole `poll` pass. Without it, two overlapping passes (the
    // loop and a test's or a later milestone's direct call) could swap an
    // older key list in after a newer one while storing the newer counter,
    // and a revoked key would keep working until the next admin change.
    // tokio's `Mutex`: it is held across `.await`s. It also guards the
    // pass's own memory, the broken catalogs: a Rust mutex owns the data it
    // protects, which can only be reached through the lock.
    polling: tokio::sync::Mutex<Broken>,
}

impl App {
    /// Loads the key cache and every catalog's snapshot.
    ///
    /// A catalog whose snapshot fails to build is logged and left out, as
    /// [`App::poll`] does, and a later poll tries it again (see there for
    /// when): one bad catalog never takes the others offline.
    ///
    /// # Arguments
    /// * `pool` - the database (migrated).
    /// * `config` - the server's settings.
    /// * `clock` - the source of every timestamp.
    ///
    /// # Returns
    /// The shared state.
    ///
    /// # Errors
    /// [`OkfError::Database`] reading the counter, the keys or the list.
    pub async fn load(
        pool: PgPool,
        config: Config,
        clock: Arc<dyn Clock>,
    ) -> Result<Arc<Self>, OkfError> {
        // Read the counter before the keys: a change committed between the
        // two moves it again, so the next poll reloads.
        let admin_generation = catalogs::admin_generation(&pool).await?;
        let keys = Arc::new(Keys::load(&pool).await?);
        let mut loaded = Catalogs::new();
        let mut broken = Broken::new();
        for row in catalogs::list(&pool).await? {
            match CatalogState::load(&pool, &row.name).await {
                Ok(state) => {
                    loaded.insert(row.name, Arc::new(state));
                }
                // Deleted between the list and the load.
                Err(OkfError::CatalogNotSelected { .. }) => {}
                // Not in the map, so a later poll tries to load it again.
                Err(error) => note_failure(&mut broken, &row, "load", &error),
            }
        }
        // `if let Some(name) = ...`: Python's `if (name := ...) is not None`.
        if let Some(name) = &config.default_catalog {
            if !loaded.contains_key(name) {
                tracing::warn!(catalog = %name, "default_catalog is not loaded (yet)");
            }
        }
        Ok(Arc::new(Self {
            pool,
            config,
            clock,
            keys,
            catalogs: RwLock::new(Arc::new(loaded)),
            admin_generation: AtomicI64::new(admin_generation),
            polling: tokio::sync::Mutex::new(broken),
        }))
    }

    /// The database.
    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// The server's settings.
    pub fn config(&self) -> &Config {
        &self.config
    }

    /// The source of every timestamp.
    pub fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }

    /// The key cache (shared with the auth layer).
    pub fn keys(&self) -> &Arc<Keys> {
        &self.keys
    }

    /// Every loaded catalog, as of now. Cheap (an `Arc` clone); one call
    /// gives a consistent view to check names against and take states from.
    pub fn catalogs(&self) -> Arc<Catalogs> {
        Arc::clone(&self.catalogs.read().expect("catalog map lock poisoned"))
    }

    /// One catalog's state.
    ///
    /// # Arguments
    /// * `name` - the catalog's name.
    ///
    /// # Returns
    /// Its state, or `None` if no such catalog is loaded.
    pub fn catalog(&self, name: &str) -> Option<Arc<CatalogState>> {
        self.catalogs().get(name).cloned()
    }

    /// One pass of the poll loop: reloads the key cache if `admin_generation`
    /// moved, and brings the catalogs in line with the catalog list.
    ///
    /// A catalog that fails to load or refresh, or a key reload that fails,
    /// is logged and the rest of the pass goes ahead. A key reload, or a
    /// catalog that failed on a database error, is retried at the next pass.
    /// A catalog whose stored data would not build is retried only once its
    /// `generation` moves (an import or publish), since the same data would
    /// fail the same way: until then a refresh keeps the old snapshot, and a
    /// catalog never loaded stays out. Passes never overlap: a call made while another runs waits for
    /// it, then makes its own pass.
    ///
    /// # Returns
    /// `Ok(())` once the pass is done.
    ///
    /// # Errors
    /// [`OkfError::Database`] reading the counter or the catalog list.
    pub async fn poll(&self) -> Result<(), OkfError> {
        // The guard releases the mutex when the function returns; through
        // it (`*broken`, like a reference) the pass reads and replaces the
        // broken catalogs.
        let mut broken = self.polling.lock().await;
        self.reload_keys_if_changed().await?;
        let current = self.catalogs();
        let mut next = Catalogs::new();
        // Rebuilt each pass, so a deleted catalog's entry goes with it.
        let mut still_broken = Broken::new();
        for row in catalogs::list(&self.pool).await? {
            // This very generation already failed to build: don't try again.
            let failed_before = broken.get(&row.id) == Some(&row.generation);
            if failed_before {
                still_broken.insert(row.id, row.generation);
            }
            // The same id: the same catalog. A different id under the same
            // name: deleted and created again, so load it from scratch.
            let existing = current.get(&row.name).filter(|state| state.id() == row.id);
            let state = match existing {
                Some(state) => {
                    if row.generation > state.current().generation() && !failed_before {
                        if let Err(error) = state.refresh(&self.pool).await {
                            // The old snapshot stays in place.
                            note_failure(&mut still_broken, &row, "refresh", &error);
                        }
                    }
                    Arc::clone(state)
                }
                None if failed_before => continue,
                None => match CatalogState::load(&self.pool, &row.name).await {
                    Ok(state) => {
                        tracing::info!(catalog = %row.name, "catalog loaded");
                        Arc::new(state)
                    }
                    Err(error) => {
                        note_failure(&mut still_broken, &row, "load", &error);
                        continue;
                    }
                },
            };
            next.insert(row.name, state);
        }
        *self.catalogs.write().expect("catalog map lock poisoned") = Arc::new(next);
        *broken = still_broken;
        Ok(())
    }

    /// Reloads the key cache if `admin_generation` moved since it was loaded.
    /// Called only under `polling`.
    ///
    /// # Errors
    /// [`OkfError::Database`] reading the counter. A failed reload is logged,
    /// not returned: it leaves the stored counter alone, so the next pass
    /// retries it, and the caller's catalog changes go ahead regardless.
    async fn reload_keys_if_changed(&self) -> Result<(), OkfError> {
        let admin_generation = catalogs::admin_generation(&self.pool).await?;
        if admin_generation == self.admin_generation.load(Ordering::Relaxed) {
            return Ok(());
        }
        match self.keys.reload(&self.pool).await {
            Ok(()) => self
                .admin_generation
                .store(admin_generation, Ordering::Relaxed),
            Err(error) => {
                tracing::error!(%error, "key reload failed; retrying at the next poll");
            }
        }
        Ok(())
    }

    /// Runs [`App::poll`] every `poll_interval` until `shutdown` is cancelled.
    /// A failed pass is logged and the loop goes on.
    ///
    /// # Arguments
    /// * `shutdown` - cancelled when the server stops.
    // `self: Arc<Self>`: the method takes a shared handle by value, so the
    // spawned task owns one and the `App` lives as long as the loop.
    pub async fn poll_until(self: Arc<Self>, shutdown: CancellationToken) {
        let mut ticks = tokio::time::interval(self.config.poll_interval);
        // After a slow pass, wait a full interval instead of catching up.
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        // The first tick completes at once, and the state was just loaded.
        ticks.tick().await;
        loop {
            // `select!` waits for whichever branch is ready first and drops
            // the other. A pass in progress is not interrupted: cancellation
            // is only noticed between passes.
            tokio::select! {
                () = shutdown.cancelled() => return,
                _ = ticks.tick() => {
                    if let Err(error) = self.poll().await {
                        tracing::error!(%error, "poll failed");
                    }
                }
            }
        }
    }
}

/// Logs a catalog that failed to load or refresh and, unless the failure was
/// the database's (worth retrying at once), remembers its generation as
/// broken.
///
/// # Arguments
/// * `broken` - the broken catalogs, to add to.
/// * `row` - the catalog's row from this pass's list.
/// * `action` - `"load"` or `"refresh"`, for the log line.
/// * `error` - what went wrong.
fn note_failure(broken: &mut Broken, row: &catalogs::Catalog, action: &str, error: &OkfError) {
    // `matches!` is a one-pattern `match` that yields a `bool`.
    let retry = if matches!(error, OkfError::Database { .. }) {
        "at the next poll"
    } else {
        broken.insert(row.id, row.generation);
        "when the catalog changes"
    };
    tracing::error!(
        catalog = %row.name,
        generation = row.generation,
        %error,
        "{action} failed; retrying {retry}"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::Files;
    use crate::clock::SystemClock;
    use crate::db::test_db;
    use crate::okf::workspace::StoredFile;
    use crate::store::import::{import, ImportOptions};
    use crate::store::keys::{self as key_rows, Access, Grants, NewKey};
    use chrono::Utc;
    use std::time::{Duration, Instant};

    async fn import_files(pool: &PgPool, catalog: &str, files: &[(&str, &str)], replace: bool) {
        let files: Files = files
            .iter()
            .map(|(path, text)| {
                let file = StoredFile {
                    content: text.as_bytes().to_vec(),
                    executable: false,
                };
                (path.to_string(), file)
            })
            .collect();
        let options = ImportOptions {
            replace,
            actor: "tester",
            now: Utc::now(),
        };
        import(pool, catalog, files, &options).await.unwrap();
    }

    async fn new_key(pool: &PgPool, id: &str) -> String {
        let new = NewKey {
            id,
            access: Access::Read,
            grants: Grants::All,
            default_catalog: None,
            actor: "tester",
        };
        key_rows::create(pool, &new, Utc::now()).await.unwrap()
    }

    async fn app(pool: &PgPool) -> Arc<App> {
        App::load(pool.clone(), Config::default(), Arc::new(SystemClock))
            .await
            .unwrap()
    }

    fn names(app: &App) -> Vec<String> {
        app.catalogs().keys().cloned().collect()
    }

    /// Stores a row no workspace accepts (a path escaping the catalog), so
    /// the catalog's snapshot fails to build.
    async fn break_catalog(pool: &PgPool, catalog_id: i64) {
        sqlx::query(
            "INSERT INTO documents (catalog_id, path, content, executable, revision, \
                                    updated_at, updated_by) \
             VALUES ($1, '../escape.md', '', false, 1, now(), 'test')",
        )
        .bind(catalog_id)
        .execute(pool)
        .await
        .unwrap();
    }

    /// Removes the row [`break_catalog`] stored, without moving the
    /// catalog's generation (as a hand repair in SQL would).
    async fn repair_catalog(pool: &PgPool, catalog_id: i64) {
        sqlx::query("DELETE FROM documents WHERE catalog_id = $1 AND path = '../escape.md'")
            .bind(catalog_id)
            .execute(pool)
            .await
            .unwrap();
    }

    /// Moves a catalog's generation, as an import or publish does.
    async fn bump_generation(pool: &PgPool, catalog_id: i64) {
        sqlx::query("UPDATE catalogs SET generation = generation + 1 WHERE id = $1")
            .bind(catalog_id)
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn load_holds_every_catalog_and_live_key() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", Utc::now())
            .await
            .unwrap();
        catalogs::create(&db.pool, "work", "", Utc::now())
            .await
            .unwrap();
        import_files(&db.pool, "pg", &[("a.md", "# A\n")], false).await;
        let key = new_key(&db.pool, "reader").await;

        let app = app(&db.pool).await;

        assert_eq!(names(&app), ["pg", "work"]);
        assert_eq!(app.catalog("pg").unwrap().current().files(), 1);
        assert_eq!(app.keys().find(&key).unwrap().key_id, "reader");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_notices_imports_new_and_deleted_catalogs_and_revoked_keys() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", Utc::now())
            .await
            .unwrap();
        catalogs::create(&db.pool, "gone", "", Utc::now())
            .await
            .unwrap();
        let key = new_key(&db.pool, "reader").await;
        let app = app(&db.pool).await;
        let pg_before = app.catalog("pg").unwrap();

        // Changes made by the subcommands, beside the running server.
        import_files(&db.pool, "pg", &[("a.md", "# A\n")], false).await;
        catalogs::create(&db.pool, "work", "", Utc::now())
            .await
            .unwrap();
        catalogs::delete(&db.pool, "gone", false).await.unwrap();
        key_rows::revoke(&db.pool, "reader", Utc::now())
            .await
            .unwrap();
        app.poll().await.unwrap();

        assert_eq!(names(&app), ["pg", "work"]);
        // The same state, refreshed in place: readers holding it see the new snapshot.
        let pg_after = app.catalog("pg").unwrap();
        assert!(Arc::ptr_eq(&pg_before, &pg_after));
        assert_eq!(pg_after.current().files(), 1);
        assert!(app.keys().find(&key).is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn overlapping_polls_leave_the_newest_key_list() {
        let db = test_db().await;
        let key = new_key(&db.pool, "reader").await;
        let app = app(&db.pool).await;

        key_rows::revoke(&db.pool, "reader", Utc::now())
            .await
            .unwrap();
        // `join!` runs both passes concurrently; `polling` serializes them.
        let (first, second) = tokio::join!(app.poll(), app.poll());

        first.unwrap();
        second.unwrap();
        assert!(app.keys().find(&key).is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_reloads_a_catalog_created_again_under_the_same_name() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "first", Utc::now())
            .await
            .unwrap();
        let app = app(&db.pool).await;
        let first_id = app.catalog("pg").unwrap().id();

        catalogs::delete(&db.pool, "pg", false).await.unwrap();
        catalogs::create(&db.pool, "pg", "second", Utc::now())
            .await
            .unwrap();
        app.poll().await.unwrap();

        let pg = app.catalog("pg").unwrap();
        assert_ne!(pg.id(), first_id);
        assert_eq!(pg.description(), "second");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_catalog_that_fails_to_load_is_skipped_until_it_changes() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", Utc::now())
            .await
            .unwrap();
        let bad = catalogs::create(&db.pool, "bad", "", Utc::now())
            .await
            .unwrap();
        break_catalog(&db.pool, bad.id).await;

        // At startup and at a poll alike, the other catalogs go ahead.
        let app = app(&db.pool).await;
        assert_eq!(names(&app), ["pg"]);
        app.poll().await.unwrap();
        assert_eq!(names(&app), ["pg"]);
        // Repaired behind the server's back: the generation it failed at is
        // not built again, so the repair goes unnoticed...
        repair_catalog(&db.pool, bad.id).await;
        app.poll().await.unwrap();
        assert_eq!(names(&app), ["pg"]);
        // ...until the catalog changes, as an import would change it.
        bump_generation(&db.pool, bad.id).await;
        app.poll().await.unwrap();

        assert_eq!(names(&app), ["bad", "pg"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_refresh_keeps_the_old_snapshot_until_the_catalog_changes() {
        let db = test_db().await;
        let pg = catalogs::create(&db.pool, "pg", "", Utc::now())
            .await
            .unwrap();
        import_files(&db.pool, "pg", &[("a.md", "# A\n")], false).await;
        let app = app(&db.pool).await;
        break_catalog(&db.pool, pg.id).await;
        bump_generation(&db.pool, pg.id).await;

        app.poll().await.unwrap();
        let loaded = app.catalog("pg").unwrap().current();
        assert_eq!((loaded.generation(), loaded.files()), (1, 1));
        // Not rebuilt at the generation that failed, even once it would build.
        repair_catalog(&db.pool, pg.id).await;
        app.poll().await.unwrap();
        assert_eq!(app.catalog("pg").unwrap().current().generation(), 1);
        bump_generation(&db.pool, pg.id).await;
        app.poll().await.unwrap();

        let loaded = app.catalog("pg").unwrap().current();
        assert_eq!((loaded.generation(), loaded.files()), (3, 1));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_key_reload_does_not_stop_the_pass_and_is_retried() {
        let db = test_db().await;
        let key = new_key(&db.pool, "reader").await;
        let app = app(&db.pool).await;
        key_rows::revoke(&db.pool, "reader", Utc::now())
            .await
            .unwrap();
        catalogs::create(&db.pool, "work", "", Utc::now())
            .await
            .unwrap();
        // With the keys table out of the way, the reload fails.
        sqlx::query("ALTER TABLE api_keys RENAME TO api_keys_away")
            .execute(&db.pool)
            .await
            .unwrap();

        app.poll().await.unwrap();
        assert_eq!(names(&app), ["work"], "the catalog part of the pass ran");
        assert!(app.keys().find(&key).is_some(), "the old cache stays");
        sqlx::query("ALTER TABLE api_keys_away RENAME TO api_keys")
            .execute(&db.pool)
            .await
            .unwrap();
        // No admin change since: only the unstored counter makes this retry.
        app.poll().await.unwrap();

        assert!(app.keys().find(&key).is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_until_stops_when_cancelled() {
        // Generous bounds: the tests share a loaded machine, and each pass
        // talks to Postgres. The loop needs only one pass to succeed.
        const PATIENCE: Duration = Duration::from_secs(30);
        let db = test_db().await;
        let config = Config {
            poll_interval: Duration::from_millis(20),
            ..Config::default()
        };
        let app = App::load(db.pool.clone(), config, Arc::new(SystemClock))
            .await
            .unwrap();
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(Arc::clone(&app).poll_until(shutdown.clone()));
        catalogs::create(&db.pool, "pg", "", Utc::now())
            .await
            .unwrap();

        // Within a few intervals the loop has picked the new catalog up.
        let started = Instant::now();
        while app.catalog("pg").is_none() && started.elapsed() < PATIENCE {
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        let seen = app.catalog("pg").is_some();
        shutdown.cancel();

        assert!(seen, "the poll loop never loaded the new catalog");
        tokio::time::timeout(PATIENCE, task)
            .await
            .expect("poll_until returns after cancel")
            .unwrap();
    }
}

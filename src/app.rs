//! The server's shared state (design "Architecture"): the database, the
//! configuration, the clock, the key cache and every catalog held in
//! memory, the poll loop that notices changes made outside the server
//! (design "Noticing changes made outside the server"), and the clock check
//! loop and its last result (design "Time").

use std::collections::{BTreeMap, HashMap};
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, RwLock};

use chrono::TimeDelta;
use sqlx::PgPool;
use tokio::time::MissedTickBehavior;
use tokio_util::sync::CancellationToken;

use crate::auth::Keys;
use crate::catalog::CatalogState;
use crate::clock::Clock;
use crate::clock_check::{self, ClockStatus};
use crate::config::Config;
use crate::error::OkfError;
use crate::http::sessions::SessionKeys;
use crate::keyed_lock::KeyedLocks;
use crate::oauth::OAuth;
use crate::overlay::Overlays;
use crate::store::oauth::{
    clients as oauth_clients, codes as sign_in_codes, tokens as oauth_tokens,
};
use crate::store::{catalogs, changesets, email_tokens, sign_in_events};

/// Every loaded catalog, by name (a `BTreeMap`, so iteration is in name
/// order, like `catalog list`).
pub type Catalogs = BTreeMap<String, Arc<CatalogState>>;

/// Catalogs whose stored data failed to build into a snapshot: each one's id,
/// and the generation that failed. Building the same generation again would
/// fail the same way, so it is retried only once the generation moves.
type Broken = HashMap<i64, i64>;

/// How long an empty open changeset may go unchanged before the poll loop
/// discards it (design "Session lifecycle").
const EMPTY_CHANGESET_HOURS: i64 = 24;

/// How often the poll loop prunes the OAuth tables (they change slowly; a
/// pass every `poll_interval` would be three needless deletes).
const OAUTH_PRUNE_EVERY: std::time::Duration = std::time::Duration::from_secs(60 * 60);

/// How long expired tokens and unredeemed codes are kept (for diagnosis).
const OAUTH_RETENTION_DAYS: i64 = 7;

/// How long a registered client that no grant names is kept.
const UNBOUND_CLIENT_HOURS: i64 = 24;

/// How long sign-in events are kept (spec "Logging and audit").
const SIGN_IN_EVENT_RETENTION_DAYS: i64 = 90;

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
    // Which key opened each MCP session (M5 Decision 2).
    sessions: SessionKeys,
    // Each open changeset's overlay, cached (M5 Decision 6).
    overlays: Overlays,
    // Each changeset's writers, queued in this process before they take a
    // pooled connection (`write::apply`).
    write_queue: KeyedLocks,
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
    // The last clock check (design "Time"); `None` until the first one, and
    // always without a `time_server`. Written by the check loop only.
    clock_status: RwLock<Option<ClockStatus>>,
    // The OAuth authorization server's state; `None` without `public_url`.
    oauth: Option<OAuth>,
    // When the OAuth tables were last pruned; `None` until the first pass.
    // A `std` mutex: held only to read or set the instant.
    oauth_pruned: std::sync::Mutex<Option<std::time::Instant>>,
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
    /// [`OkfError::Database`] reading the counter, the keys or the list;
    /// [`OkfError::Config`] from [`OAuth::new`].
    pub async fn load(
        pool: PgPool,
        config: Config,
        clock: Arc<dyn Clock>,
    ) -> Result<Arc<Self>, OkfError> {
        // Read the counter before the keys: a change committed between the
        // two moves it again, so the next poll reloads.
        let admin_generation = catalogs::admin_generation(&pool).await?;
        // `clock` moves into `Self` below, so the key cache gets its own handle.
        let keys = Arc::new(Keys::load(&pool, Arc::clone(&clock)).await?);
        let mut loaded = Catalogs::new();
        let mut broken = Broken::new();
        for row in catalogs::list(&pool).await? {
            match CatalogState::load(&pool, &row.name).await {
                Ok(state) => {
                    loaded.insert(row.name, Arc::new(state));
                }
                // Not in the map, so a later poll tries to load it again
                // (unless it was deleted meanwhile).
                Err(error) => note_failure(&mut broken, &row, "load", &error),
            }
        }
        // `if let Some(name) = ...`: Python's `if (name := ...) is not None`.
        if let Some(name) = &config.default_catalog {
            if !loaded.contains_key(name) {
                tracing::warn!(catalog = %name, "default_catalog is not loaded (yet)");
            }
        }
        let oauth = OAuth::new(&config)?;
        let sessions = SessionKeys::new(config.session_idle_timeout);
        let overlays = Overlays::new(config.session_idle_timeout, config.overlay_cache_size);
        Ok(Arc::new(Self {
            pool,
            config,
            clock,
            keys,
            sessions,
            overlays,
            write_queue: KeyedLocks::new(),
            catalogs: RwLock::new(Arc::new(loaded)),
            admin_generation: AtomicI64::new(admin_generation),
            polling: tokio::sync::Mutex::new(broken),
            clock_status: RwLock::new(None),
            oauth,
            oauth_pruned: std::sync::Mutex::new(None),
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

    /// Which key opened each MCP session (the `/mcp` session binding).
    ///
    /// # Returns
    /// The bindings, shared by every `/mcp` request.
    pub fn sessions(&self) -> &SessionKeys {
        &self.sessions
    }

    /// The OAuth state.
    ///
    /// # Returns
    /// `None` when OAuth is off (no `public_url`).
    pub fn oauth(&self) -> Option<&OAuth> {
        self.oauth.as_ref()
    }

    /// The overlay cache (M5 Decision 6).
    ///
    /// # Returns
    /// Every open changeset's cached overlay, shared by every tool call.
    pub fn overlays(&self) -> &Overlays {
        &self.overlays
    }

    /// The per-changeset write queue: `write::apply` holds a changeset's
    /// lock here before it begins its transaction, so writers waiting their
    /// turn on one changeset hold no pooled connection.
    ///
    /// # Returns
    /// The locks, keyed by changeset id.
    pub fn write_queue(&self) -> &KeyedLocks {
        &self.write_queue
    }

    /// The last clock check, if any (`/healthz` shows it).
    ///
    /// # Returns
    /// A copy of the status, or `None` without a `time_server` or before the
    /// first check.
    pub fn clock_status(&self) -> Option<ClockStatus> {
        // A std lock is "poisoned" when a thread panicked while holding it;
        // nothing here can panic under the lock, so that is an invariant.
        self.clock_status
            .read()
            .expect("clock status lock poisoned")
            .clone()
    }

    /// Records a clock check.
    ///
    /// # Arguments
    /// * `status` - the check's outcome.
    pub fn set_clock_status(&self, status: ClockStatus) {
        *self
            .clock_status
            .write()
            .expect("clock status lock poisoned") = Some(status);
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
    /// moved, and brings the catalogs in line with the catalog list. Between
    /// the two, it discards empty changesets unchanged for 24 hours (design
    /// "Session lifecycle"), so that runs on every pass, even one whose
    /// catalog list then fails; a prune failure is logged and retried at the
    /// next pass.
    ///
    /// A catalog that fails to load or refresh, or a key reload (or the
    /// `admin_generation` read before it) that fails, is logged and the rest
    /// of the pass goes ahead. A key reload, or a
    /// catalog that failed on a database error, is retried at the next pass.
    /// A catalog whose stored data would not build is retried only once its
    /// `generation` moves (an import or publish), since the same data would
    /// fail the same way: until then a refresh keeps the old snapshot, and a
    /// catalog never loaded stays out. A catalog deleted and created again
    /// under the same name counts as never loaded: if the new one fails to
    /// load, the name is missing until it does, since the old content no
    /// longer exists to serve. Passes never overlap: a call made while another runs waits for
    /// it, then makes its own pass.
    ///
    /// # Returns
    /// `Ok(())` once the pass is done.
    ///
    /// # Errors
    /// [`OkfError::Database`] reading the catalog list.
    pub async fn poll(&self) -> Result<(), OkfError> {
        // The guard releases the mutex when the function returns; through
        // it (`*broken`, like a reference) the pass reads and replaces the
        // broken catalogs.
        let mut broken = self.polling.lock().await;
        self.reload_keys_if_changed().await;
        // Before the catalog list, whose `?` would skip it on a failure.
        self.prune_empty_changesets().await;
        self.prune_oauth().await;
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
                    // Outside the snapshot, so a `catalog update` or
                    // owner change shows without a rebuild.
                    state.set_description(&row.description);
                    state.set_owner_team(row.owner_team.as_deref());
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
    /// Failing to read the counter or to reload is logged, not returned: the
    /// stored counter stays as it was, so the next pass tries again, and the
    /// caller's catalog changes go ahead regardless.
    async fn reload_keys_if_changed(&self) {
        let admin_generation = match catalogs::admin_generation(&self.pool).await {
            Ok(admin_generation) => admin_generation,
            Err(error) => {
                tracing::error!(%error, "reading admin_generation failed; retrying at the next poll");
                return;
            }
        };
        if admin_generation == self.admin_generation.load(Ordering::Relaxed) {
            return;
        }
        match self.keys.reload(&self.pool).await {
            Ok(()) => self
                .admin_generation
                .store(admin_generation, Ordering::Relaxed),
            Err(error) => {
                tracing::error!(%error, "key reload failed; retrying at the next poll");
            }
        }
    }

    /// Discards empty changesets unchanged for [`EMPTY_CHANGESET_HOURS`].
    /// Called only under `polling`.
    ///
    /// A failure is logged, not returned: the next pass tries again, and
    /// the rest of the pass is not held up by it.
    async fn prune_empty_changesets(&self) {
        let now = self.clock.now();
        let older_than = now - TimeDelta::hours(EMPTY_CHANGESET_HOURS);
        match changesets::prune_empty(&self.pool, older_than, now).await {
            Ok(0) => {}
            Ok(pruned) => tracing::info!(pruned, "discarded empty changesets a day old"),
            Err(error) => {
                tracing::error!(%error, "pruning empty changesets failed; retrying at the next poll");
            }
        }
    }

    /// At most once per [`OAUTH_PRUNE_EVERY`], deletes expired OAuth tokens
    /// and unredeemed codes kept [`OAUTH_RETENTION_DAYS`] past their expiry,
    /// registered clients no grant names after [`UNBOUND_CLIENT_HOURS`],
    /// sign-in events older than [`SIGN_IN_EVENT_RETENTION_DAYS`] and emailed
    /// links [`OAUTH_RETENTION_DAYS`] past their expiry. Runs whether or not OAuth is on, so rows left from when it was are
    /// cleared too. Called only under `polling`.
    ///
    /// A failure is logged, not returned, and waits for the next hourly pass.
    async fn prune_oauth(&self) {
        let now_instant = std::time::Instant::now();
        {
            // A block, so the guard is released before the `.await`s below.
            let mut pruned = self.oauth_pruned.lock().expect("oauth prune lock poisoned");
            if pruned.is_some_and(|at| now_instant.duration_since(at) < OAUTH_PRUNE_EVERY) {
                return;
            }
            *pruned = Some(now_instant);
        }
        let now = self.clock.now();
        let retained = now - TimeDelta::days(OAUTH_RETENTION_DAYS);
        log_pruned(
            "expired OAuth tokens",
            oauth_tokens::prune(&self.pool, retained).await,
        );
        log_pruned(
            "expired sign-in codes",
            sign_in_codes::prune(&self.pool, retained).await,
        );
        log_pruned(
            "unused registered OAuth clients",
            oauth_clients::prune_unbound_dcr(
                &self.pool,
                now - TimeDelta::hours(UNBOUND_CLIENT_HOURS),
            )
            .await,
        );
        log_pruned(
            "old sign-in events",
            sign_in_events::prune(
                &self.pool,
                now - TimeDelta::days(SIGN_IN_EVENT_RETENTION_DAYS),
            )
            .await,
        );
        log_pruned(
            "expired emailed links",
            email_tokens::prune(&self.pool, retained).await,
        );
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

    /// Checks the clock at once and then every [`clock_check::CHECK_INTERVAL`]
    /// until `shutdown` is cancelled. Returns at once without a `time_server`.
    ///
    /// # Arguments
    /// * `shutdown` - cancelled when the server stops.
    pub async fn check_clock_until(self: Arc<Self>, shutdown: CancellationToken) {
        // `let ... else`: return when there is no server to ask.
        let Some(server) = self.config.time_server.clone() else {
            return;
        };
        let mut ticks = tokio::time::interval(clock_check::CHECK_INTERVAL);
        ticks.set_missed_tick_behavior(MissedTickBehavior::Delay);
        loop {
            // The first tick completes at once: the startup check.
            tokio::select! {
                () = shutdown.cancelled() => return,
                _ = ticks.tick() => {}
            }
            // An async call does nothing until awaited: `check` is the
            // query, not yet started, for the `select!` below to drive.
            let check = clock_check::check_once(&server, self.clock(), clock_check::QUERY_TIMEOUT);
            // A stop during a query ends it at once instead of waiting up to
            // `QUERY_TIMEOUT` (Brooke's review of PR #138).
            tokio::select! {
                () = shutdown.cancelled() => return,
                status = check => self.set_clock_status(status),
            }
        }
    }
}

/// Logs one OAuth prune's outcome: silent for nothing deleted, `info` for
/// some, `error` for a failure.
///
/// # Arguments
/// * `what` - the rows, for the log line.
/// * `result` - how many were deleted, or the failure.
fn log_pruned(what: &'static str, result: Result<u64, OkfError>) {
    match result {
        Ok(0) => {}
        Ok(pruned) => tracing::info!(pruned, "deleted {what}"),
        Err(error) => tracing::error!(%error, "deleting {what} failed; retrying in an hour"),
    }
}

/// Logs a catalog that failed to load or refresh and, if its stored data is
/// to blame, remembers its generation as broken.
///
/// A catalog deleted since this pass listed it is not a failure: it is gone
/// from the next list, so it is only noted at debug level. A database error
/// is logged but not remembered, so the next pass retries at once.
///
/// # Arguments
/// * `broken` - the broken catalogs, to add to.
/// * `row` - the catalog's row from this pass's list.
/// * `action` - `"load"` or `"refresh"`, for the log line.
/// * `error` - what went wrong.
fn note_failure(broken: &mut Broken, row: &catalogs::Catalog, action: &str, error: &OkfError) {
    let retry = match error {
        OkfError::CatalogNotSelected { .. } => {
            tracing::debug!(catalog = %row.name, "{action} skipped: the catalog was deleted");
            return;
        }
        // Transient: the next pass tries again, as for a database error.
        OkfError::Database { .. } | OkfError::Busy { .. } => "at the next poll",
        // Every other error comes from the stored data, so the same
        // generation would fail the same way. (`_` matches any variant.)
        _ => {
            broken.insert(row.id, row.generation);
            "when the catalog changes"
        }
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
    use crate::mcp::testing::app_with;
    use crate::okf::workspace::StoredFile;
    use crate::store::changesets::{ChangesetState, EntryRow};
    use crate::store::import::{import, ImportOptions, ImportScope};
    use crate::store::keys as key_rows;
    use chrono::{DateTime, TimeDelta, Utc};
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
            scope: ImportScope::Everything,
            expect_generation: None,
            actor: "tester",
            now: Utc::now(),
        };
        import(pool, catalog, files, &options).await.unwrap();
    }

    async fn new_key(pool: &PgPool, id: &str) -> String {
        key_rows::insert_legacy_test_key(pool, id, key_rows::Access::Read, "tester", Utc::now())
            .await
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

    #[test]
    fn only_data_failures_are_remembered_as_broken() {
        let row = catalogs::Catalog {
            id: 7,
            name: "pg".to_string(),
            description: String::new(),
            generation: 3,
            created_at: Utc::now(),
            owner_team: None,
        };
        let deleted = OkfError::CatalogNotSelected {
            catalog: "pg".to_string(),
        };
        let database = OkfError::Database {
            source: sqlx::Error::PoolTimedOut,
        };
        let bad_data = OkfError::InvalidPath {
            path: "../escape.md".to_string(),
            reason: "escapes the catalog".to_string(),
        };
        let mut broken = Broken::new();

        note_failure(&mut broken, &row, "load", &deleted);
        note_failure(&mut broken, &row, "load", &database);
        assert!(broken.is_empty());
        note_failure(&mut broken, &row, "refresh", &bad_data);

        assert_eq!(broken, Broken::from([(7, 3)]));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn load_holds_every_catalog_and_live_key() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", crate::db::TEST_TEAM, Utc::now())
            .await
            .unwrap();
        catalogs::create(&db.pool, "work", "", crate::db::TEST_TEAM, Utc::now())
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
        catalogs::create(&db.pool, "pg", "", crate::db::TEST_TEAM, Utc::now())
            .await
            .unwrap();
        catalogs::create(&db.pool, "gone", "", crate::db::TEST_TEAM, Utc::now())
            .await
            .unwrap();
        let key = new_key(&db.pool, "reader").await;
        let app = app(&db.pool).await;
        let pg_before = app.catalog("pg").unwrap();

        // Changes made by the subcommands, beside the running server.
        import_files(&db.pool, "pg", &[("a.md", "# A\n")], false).await;
        catalogs::create(&db.pool, "work", "", crate::db::TEST_TEAM, Utc::now())
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
    async fn poll_picks_up_a_new_description_without_a_rebuild() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "copy", crate::db::TEST_TEAM, Utc::now())
            .await
            .unwrap();
        let app = app(&db.pool).await;
        let pg = app.catalog("pg").unwrap();
        let before = pg.current();

        catalogs::update_description(&db.pool, "pg", "source of truth")
            .await
            .unwrap();
        app.poll().await.unwrap();

        assert_eq!(&*pg.description(), "source of truth");
        // `Arc::ptr_eq`: the very same snapshot object, so nothing was rebuilt.
        assert!(Arc::ptr_eq(&before, &app.catalog("pg").unwrap().current()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_picks_up_a_new_owner_without_a_rebuild() {
        let db = test_db().await;
        let alice = crate::store::users::NewUser {
            name: "alice",
            display_name: "",
        };
        crate::store::users::create(&db.pool, &alice, Utc::now())
            .await
            .unwrap();
        catalogs::create(&db.pool, "pg", "", crate::db::TEST_TEAM, Utc::now())
            .await
            .unwrap();
        let app = app(&db.pool).await;
        let pg = app.catalog("pg").unwrap();
        let before = pg.current();
        assert_eq!(pg.owner_team().as_deref(), Some(crate::db::TEST_TEAM));

        catalogs::set_owner(&db.pool, "pg", "alice").await.unwrap();
        app.poll().await.unwrap();

        assert_eq!(pg.owner_team().as_deref(), Some("alice"));
        assert!(Arc::ptr_eq(&before, &app.catalog("pg").unwrap().current()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_reloads_a_catalog_created_again_under_the_same_name() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "first", crate::db::TEST_TEAM, Utc::now())
            .await
            .unwrap();
        let app = app(&db.pool).await;
        let first_id = app.catalog("pg").unwrap().id();

        catalogs::delete(&db.pool, "pg", false).await.unwrap();
        catalogs::create(&db.pool, "pg", "second", crate::db::TEST_TEAM, Utc::now())
            .await
            .unwrap();
        app.poll().await.unwrap();

        let pg = app.catalog("pg").unwrap();
        assert_ne!(pg.id(), first_id);
        assert_eq!(&*pg.description(), "second");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_catalog_that_fails_to_load_is_skipped_until_it_changes() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", crate::db::TEST_TEAM, Utc::now())
            .await
            .unwrap();
        let bad = catalogs::create(&db.pool, "bad", "", crate::db::TEST_TEAM, Utc::now())
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
        let pg = catalogs::create(&db.pool, "pg", "", crate::db::TEST_TEAM, Utc::now())
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
    async fn a_re_created_catalog_that_fails_to_load_drops_the_old_one() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", crate::db::TEST_TEAM, Utc::now())
            .await
            .unwrap();
        import_files(&db.pool, "pg", &[("a.md", "# A\n")], false).await;
        let app = app(&db.pool).await;
        catalogs::delete(&db.pool, "pg", false).await.unwrap();
        let again = catalogs::create(&db.pool, "pg", "", crate::db::TEST_TEAM, Utc::now())
            .await
            .unwrap();
        break_catalog(&db.pool, again.id).await;

        app.poll().await.unwrap();

        assert!(
            app.catalog("pg").is_none(),
            "the deleted catalog is not served"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_key_reload_does_not_stop_the_pass_and_is_retried() {
        let db = test_db().await;
        let key = new_key(&db.pool, "reader").await;
        let app = app(&db.pool).await;
        key_rows::revoke(&db.pool, "reader", Utc::now())
            .await
            .unwrap();
        catalogs::create(&db.pool, "work", "", crate::db::TEST_TEAM, Utc::now())
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
    async fn a_failed_admin_generation_read_does_not_stop_the_pass() {
        let db = test_db().await;
        let app = app(&db.pool).await;
        catalogs::create(&db.pool, "work", "", crate::db::TEST_TEAM, Utc::now())
            .await
            .unwrap();
        // With the counter's table out of the way, reading it fails.
        sqlx::query("ALTER TABLE server_state RENAME TO server_state_away")
            .execute(&db.pool)
            .await
            .unwrap();

        app.poll().await.unwrap();

        assert_eq!(names(&app), ["work"]);
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
        catalogs::create(&db.pool, "pg", "", crate::db::TEST_TEAM, Utc::now())
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

    #[tokio::test(flavor = "multi_thread")]
    async fn the_clock_check_runs_at_startup_and_stops_with_the_server() {
        // `None`: the server stamps its reply when the request arrives, after
        // `app_with` has started its database (several seconds).
        let server = crate::ntp::testing::fake_server(None, 1).await;
        let config = Config {
            time_server: Some(server.to_string()),
            ..Config::default()
        };
        let test = app_with(&[("pg", &[])], config).await;
        let shutdown = CancellationToken::new();
        let task = tokio::spawn(Arc::clone(&test.app).check_clock_until(shutdown.clone()));

        // Wait for the startup check, but not forever.
        let status = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                if let Some(status) = test.app.clock_status() {
                    break status;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("a clock check within 10 s");
        shutdown.cancel();
        tokio::time::timeout(Duration::from_secs(5), task)
            .await
            .expect("the loop stops when cancelled")
            .expect("the loop does not panic");

        assert!(status.ok, "{status:?}");
        // Both ends read the system clock: the offset is a few ms at most.
        assert!(
            status.offset_ms.is_some_and(|ms| ms.abs() < 1_000),
            "{status:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn without_a_time_server_there_is_no_check() {
        let test = app_with(&[("pg", &[])], Config::default()).await;

        // Returns at once, without a cancelled token.
        tokio::time::timeout(
            Duration::from_secs(1),
            Arc::clone(&test.app).check_clock_until(CancellationToken::new()),
        )
        .await
        .expect("returns at once without a time_server");

        assert_eq!(test.app.clock_status(), None);
    }

    /// Opens a changeset for key `writer` at `at`, with an entry for `path`
    /// if given.
    async fn changeset_at(
        pool: &PgPool,
        catalog_id: i64,
        at: DateTime<Utc>,
        path: Option<&str>,
    ) -> i64 {
        let mut tx = pool.begin().await.unwrap();
        changesets::lock_catalog(&mut tx, catalog_id).await.unwrap();
        let changeset = changesets::open(&mut tx, catalog_id, "writer", "writer", at)
            .await
            .unwrap();
        if let Some(path) = path {
            let entry = EntryRow {
                path: path.to_string(),
                base_revision: None,
                content: Some(b"x\n".to_vec()),
                executable: false,
                replay: None,
            };
            changesets::upsert_entry(&mut tx, changeset.id, &entry, at)
                .await
                .unwrap();
        }
        tx.commit().await.unwrap();
        changeset.id
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_discards_empty_changesets_a_day_old_and_keeps_the_rest() {
        let db = test_db().await;
        let pg = catalogs::create(&db.pool, "pg", "", crate::db::TEST_TEAM, Utc::now())
            .await
            .unwrap();
        new_key(&db.pool, "writer").await;
        let app = app(&db.pool).await;
        let day_old = Utc::now() - TimeDelta::hours(25);
        let empty_old = changeset_at(&db.pool, pg.id, day_old, None).await;
        let empty_new = changeset_at(&db.pool, pg.id, Utc::now(), None).await;
        let full_old = changeset_at(&db.pool, pg.id, day_old, Some("a.md")).await;

        app.poll().await.unwrap();

        let state = |id| {
            let pool = db.pool.clone();
            async move { changesets::by_id(&pool, id).await.unwrap().unwrap().state }
        };
        assert_eq!(state(empty_old).await, ChangesetState::Discarded);
        assert_eq!(state(empty_new).await, ChangesetState::Open);
        assert_eq!(state(full_old).await, ChangesetState::Open);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_prunes_oauth_rows_at_most_hourly() {
        use crate::store::keys::Access;
        use crate::store::oauth::testing::{insert_client, insert_grant};
        let test = app_with(&[("main", &[])], Config::default()).await;
        let pool = test.app.pool().clone();
        let now = Utc::now();
        let client = insert_client(
            &pool,
            "https://client.example.com/client.json",
            "Example",
            now,
        )
        .await;
        let grant = insert_grant(&pool, &client, Access::Read, &["main"], now).await;
        let stale_token = |byte: u8| {
            let pool = pool.clone();
            let grant = grant.clone();
            async move {
                sqlx::query(
                    "INSERT INTO oauth_tokens \
                     (sha256, grant_id, kind, family_id, resource, created_at, expires_at) \
                     VALUES ($1, $2, 'access', 'f', 'https://okf.example.com/mcp', $3, $4)",
                )
                .bind([byte; 32].as_slice())
                .bind(&grant)
                .bind(now - TimeDelta::days(9))
                .bind(now - TimeDelta::days(8))
                .execute(&pool)
                .await
                .unwrap();
            }
        };
        let count = |table: &'static str| {
            let pool = pool.clone();
            async move {
                // `AssertSqlSafe`: `table` is one of this test's literals.
                sqlx::query_scalar::<_, i64>(sqlx::AssertSqlSafe(format!(
                    "SELECT count(*) FROM {table}"
                )))
                .fetch_one(&pool)
                .await
                .unwrap()
            }
        };
        stale_token(1).await;
        sqlx::query(
            "INSERT INTO oauth_clients (client_id, kind, redirect_uris, metadata, created_at) \
             VALUES ('unbound', 'dcr', ARRAY['https://client.example.com/callback'], '{}', $1)",
        )
        .bind(now - TimeDelta::days(2))
        .execute(&pool)
        .await
        .unwrap();

        // The first pass prunes.
        test.app.poll().await.unwrap();
        assert_eq!(count("oauth_tokens").await, 0);
        assert_eq!(count("oauth_clients").await, 1, "the bound client stays");

        // Within the hour, a new stale row waits for the next hourly pass.
        stale_token(2).await;
        test.app.poll().await.unwrap();
        assert_eq!(count("oauth_tokens").await, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn poll_prunes_old_sign_in_events_and_long_expired_emailed_links() {
        let test = app_with(&[("main", &[])], Config::default()).await;
        let pool = test.app.pool().clone();
        let now = Utc::now();
        let user: String = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(crate::db::TEST_USER)
            .fetch_one(&pool)
            .await
            .unwrap();
        for (byte, expired_days_ago) in [(1u8, 8), (2u8, 1)] {
            sqlx::query(
                "INSERT INTO email_tokens (hash, user_id, purpose, created_at, expires_at) \
                 VALUES ($1, $2, 'setup', $3, $4)",
            )
            .bind([byte; 32].as_slice())
            .bind(&user)
            .bind(now - TimeDelta::days(expired_days_ago + 1))
            .bind(now - TimeDelta::days(expired_days_ago))
            .execute(&pool)
            .await
            .unwrap();
        }
        for days_ago in [91, 1] {
            sqlx::query(
                "INSERT INTO sign_in_events (user_id, method, outcome, at) \
                 VALUES ($1, 'password', 'failed', $2)",
            )
            .bind(&user)
            .bind(now - TimeDelta::days(days_ago))
            .execute(&pool)
            .await
            .unwrap();
        }

        test.app.poll().await.unwrap();

        let tokens: i64 = sqlx::query_scalar("SELECT count(*) FROM email_tokens")
            .fetch_one(&pool)
            .await
            .unwrap();
        let events: i64 = sqlx::query_scalar("SELECT count(*) FROM sign_in_events")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!((tokens, events), (1, 1));
    }
}

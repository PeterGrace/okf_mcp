//! Test support for the tools: an [`App`] over a throwaway database holding
//! catalogs, and principals and contexts to call tools with.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::app::App;
// Re-exported so tool tests keep calling `testing::files`; it lives in
// `archive`, beside `Files`, so store tests need not reach up into `mcp`.
pub(crate) use crate::archive::files;
use crate::auth::{CredentialKind, Principal, Reach};
use crate::clock::{Clock, SystemClock};
use crate::config::Config;
use crate::db::pg_support::TestDb;
use crate::mcp::context::ToolContext;
use crate::store::catalogs;
use crate::store::changesets::{self, ChangesetRow, EntryRow};
use crate::store::import::{import, ImportOptions, ImportScope};
use crate::store::keys::Access;

/// An app and the database under it. Fields drop in declaration order: the
/// app (and its pool) before the container.
pub(crate) struct TestApp {
    /// The shared state, as a server would hold it.
    pub(crate) app: Arc<App>,
    _db: TestDb,
}

/// Creates each catalog, imports its files now, and loads the app with the
/// system clock.
///
/// # Arguments
/// * `catalogs_and_files` - `(catalog name, its files)` pairs, in creation order.
/// * `config` - the server's settings.
///
/// # Returns
/// The loaded app and its database.
///
/// # Panics
/// As [`app_at`].
pub(crate) async fn app_with(
    catalogs_and_files: &[(&str, &[(&str, &str)])],
    config: Config,
) -> TestApp {
    app_at(
        catalogs_and_files,
        config,
        Utc::now(),
        Arc::new(SystemClock),
    )
    .await
}

/// Creates each catalog, imports its files at `imported`, and loads the app
/// with `clock`.
///
/// # Arguments
/// * `catalogs_and_files` - `(catalog name, its files)` pairs, in creation order.
/// * `config` - the server's settings.
/// * `imported` - the creation and import time (each document's revision time).
/// * `clock` - the server's clock.
///
/// # Returns
/// The loaded app and its database.
///
/// # Panics
/// If the database, a create or an import fails: a broken test setup.
pub(crate) async fn app_at(
    catalogs_and_files: &[(&str, &[(&str, &str)])],
    config: Config,
    imported: DateTime<Utc>,
    clock: Arc<dyn Clock>,
) -> TestApp {
    let db = crate::db::test_db().await;
    for (name, entries) in catalogs_and_files {
        catalogs::create(&db.pool, name, "", crate::db::TEST_TEAM, imported)
            .await
            .expect("create the test catalog");
        let options = ImportOptions {
            replace: false,
            scope: ImportScope::Everything,
            expect_generation: None,
            actor: "tester",
            now: imported,
        };
        import(&db.pool, name, files(entries), &options)
            .await
            .expect("import the test files");
    }
    // Test apps only enqueue (Decision 19); the investigator test serves one.
    let mut config = config;
    config.mail.worker = false;
    let app = App::load(db.pool.clone(), config, clock)
        .await
        .expect("load the app");
    TestApp { app, _db: db }
}

/// A key with this id and access, reaching `reach` (every catalog when
/// empty), with a default.
///
/// # Arguments
/// * `key_id` - its id (also its actor).
/// * `access` - its access level.
/// * `reach` - the catalogs it is granted; empty for every catalog.
/// * `default` - its default catalog.
///
/// # Returns
/// The principal, as the auth layer would put it in a request.
pub(crate) fn principal_as(
    key_id: &str,
    access: Access,
    reach: &[&str],
    default: Option<&str>,
) -> Arc<Principal> {
    principal_with_reporting(key_id, access, reach, default, false)
}

/// Builds a test principal, explicitly opting into maintenance reporting.
pub(crate) fn principal_with_reporting(
    key_id: &str,
    access: Access,
    reach: &[&str],
    default: Option<&str>,
    maintenance_report: bool,
) -> Arc<Principal> {
    Arc::new(Principal {
        key_id: key_id.to_string(),
        access,
        maintenance_report,
        reach: if reach.is_empty() {
            Reach::All
        } else {
            // Each catalog at the principal's own `access`, as before levels.
            Reach::Catalogs(
                reach
                    .iter()
                    .map(|name| (name.to_string(), access))
                    .collect(),
            )
        },
        default_catalog: default.map(str::to_string),
        actor: key_id.to_string(),
        kind: CredentialKind::ApiKey,
        user: None,
        suspended: Default::default(),
    })
}

/// A `read` key `tester` reaching `reach` (every catalog when empty), with
/// a default.
///
/// # Arguments
/// * `reach` - the catalogs it is granted; empty for every catalog.
/// * `default` - its default catalog.
///
/// # Returns
/// The principal, as the auth layer would put it in a request.
pub(crate) fn principal(reach: &[&str], default: Option<&str>) -> Arc<Principal> {
    principal_as("tester", Access::Read, reach, default)
}

/// Stores a key row, so changesets can name it (`changesets.key_id` is a
/// foreign key). Its stored access does not matter: tests build the
/// principal with [`principal_as`].
///
/// # Arguments
/// * `test` - the app.
/// * `key_id` - the key's id.
///
/// # Panics
/// If the store refuses it.
pub(crate) async fn store_key(test: &TestApp, key_id: &str) {
    crate::store::keys::insert_legacy_test_key(
        test.app.pool(),
        key_id,
        Access::Admin,
        key_id,
        Utc::now(),
    )
    .await;
}

/// A tool context for a `write` key reaching every catalog: the key's row
/// is stored first (a changeset's `key_id` is a foreign key).
///
/// # Arguments
/// * `test` - the app.
/// * `key_id` - the key's id, also its actor (call once per key and test).
/// * `default` - its default catalog.
///
/// # Returns
/// The context a write tool would build for that key. A second connection
/// of the same key is `context(test, principal_as(...), None)`.
///
/// # Panics
/// If the key row cannot be stored (already stored in this test, say).
pub(crate) async fn writer(test: &TestApp, key_id: &str, default: &str) -> ToolContext {
    store_key(test, key_id).await;
    context(
        test,
        principal_as(key_id, Access::Write, &[], Some(default)),
        None,
    )
}

/// Opens a changeset for `ctx`'s key in its write target (the first
/// selected catalog), as `open_changeset` would.
///
/// # Arguments
/// * `ctx` - the connection.
///
/// # Returns
/// The new changeset's handle.
///
/// # Panics
/// If the tool refuses (a key below `write`, say).
pub(crate) async fn open(ctx: &ToolContext) -> String {
    let params = crate::mcp::changesets::OpenParams { catalog: None };
    crate::mcp::changesets::open_changeset(ctx, params)
        .await
        .expect("open a changeset")
        .changeset
}

/// Opens a changeset for `ctx`'s key in `catalog` at `at` (its creation and
/// last-change time), in the lock order.
///
/// # Arguments
/// * `test` - the app.
/// * `ctx` - the connection: its key and actor own the changeset.
/// * `catalog` - the catalog's name.
/// * `at` - the time to record.
///
/// # Returns
/// The new changeset.
///
/// # Panics
/// If the catalog is not loaded or the store fails.
pub(crate) async fn open_at(
    test: &TestApp,
    ctx: &ToolContext,
    catalog: &str,
    at: DateTime<Utc>,
) -> ChangesetRow {
    let state = test.app.catalog(catalog).expect("a loaded catalog");
    let principal = ctx.principal();
    let mut tx = test.app.pool().begin().await.expect("begin");
    changesets::lock_catalog(&mut tx, state.id())
        .await
        .expect("lock the catalog");
    let changeset = changesets::open(&mut tx, state.id(), &principal.key_id, &principal.actor, at)
        .await
        .expect("open the changeset");
    tx.commit().await.expect("commit");
    changeset
}

/// A changeset by its handle.
///
/// # Arguments
/// * `test` - the app.
/// * `handle` - the handle.
///
/// # Returns
/// The row, in whatever state it is.
///
/// # Panics
/// If there is no such changeset or the store fails.
pub(crate) async fn changeset_row(test: &TestApp, handle: &str) -> ChangesetRow {
    changesets::by_handle(test.app.pool(), handle)
        .await
        .expect("read the changeset")
        .expect("the changeset exists")
}

/// One pending edit, for [`pending_entry`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct PendingEdit<'a> {
    /// Catalog-relative path.
    pub(crate) path: &'a str,
    /// The new text; `None` deletes the path.
    pub(crate) text: Option<&'a str>,
    /// The time of the edit.
    pub(crate) at: DateTime<Utc>,
}

/// Stores an edit in the changeset `handle` names, in the lock order, as a
/// write tool would (without the write rules).
///
/// # Arguments
/// * `test` - the app.
/// * `handle` - the changeset.
/// * `edit` - the path, text and time.
///
/// # Returns
/// The changeset after the edit.
///
/// # Panics
/// If there is no such changeset or the store fails.
pub(crate) async fn pending_entry(
    test: &TestApp,
    handle: &str,
    edit: &PendingEdit<'_>,
) -> ChangesetRow {
    let changeset = changeset_row(test, handle).await;
    let entry = EntryRow {
        path: edit.path.to_string(),
        base_revision: None,
        content: edit.text.map(|text| text.as_bytes().to_vec()),
        executable: false,
        replay: None,
    };
    let mut tx = test.app.pool().begin().await.expect("begin");
    changesets::lock_catalog(&mut tx, changeset.catalog_id)
        .await
        .expect("lock the catalog");
    changesets::upsert_entry(&mut tx, changeset.id, &entry, edit.at)
        .await
        .expect("store the entry");
    tx.commit().await.expect("commit");
    changeset_row(test, handle).await
}

/// A tool context for `principal` on a connection naming `connection`.
///
/// # Arguments
/// * `test` - the app.
/// * `principal` - the calling key.
/// * `connection` - the connection's catalogs (`X-OKF-Catalogs`), if any.
///
/// # Returns
/// The context a tool would build from such a request.
pub(crate) fn context(
    test: &TestApp,
    principal: Arc<Principal>,
    connection: Option<&[&str]>,
) -> ToolContext {
    let connection = connection.map(|names| names.iter().map(|name| name.to_string()).collect());
    ToolContext::new(Arc::clone(&test.app), principal, connection)
}

/// The team [`suspend_catalogs`] moves catalogs to and suspends.
pub(crate) const SUSPENDED_TEAM: &str = "family";

/// The notice [`suspend_catalogs`] gives the suspension.
pub(crate) const SUSPENSION_NOTICE: &str = "Paused for billing.";

/// Moves `catalogs` to a new team, [`SUSPENDED_TEAM`] (owned by
/// [`crate::db::TEST_USER`]), suspends it with [`SUSPENSION_NOTICE`], and
/// polls, so the key cache sees the suspension. Every other catalog stays
/// with [`crate::db::TEST_TEAM`], active.
///
/// # Arguments
/// * `test` - the app.
/// * `catalogs` - the catalogs to suspend (call once per test).
///
/// # Panics
/// If the team exists already or a store call fails.
pub(crate) async fn suspend_catalogs(test: &TestApp, catalogs: &[&str]) {
    let pool = test.app.pool();
    let now = Utc::now();
    let team = crate::store::teams::NewTeam {
        name: SUSPENDED_TEAM,
        display_name: "Family",
        owner: crate::db::TEST_USER,
    };
    crate::store::teams::create(pool, &team, now)
        .await
        .expect("create the team to suspend");
    for catalog in catalogs {
        catalogs::set_owner(pool, catalog, SUSPENDED_TEAM)
            .await
            .expect("move the catalog to the team");
    }
    crate::store::teams::suspend(pool, SUSPENDED_TEAM, Some(SUSPENSION_NOTICE), now)
        .await
        .expect("suspend the team");
    test.app.poll().await.expect("poll the suspension in");
}

/// Lifts [`suspend_catalogs`]'s suspension and polls it in.
///
/// # Panics
/// If the store or the poll fails.
pub(crate) async fn lift_suspension(test: &TestApp) {
    crate::store::teams::unsuspend(test.app.pool(), SUSPENDED_TEAM)
        .await
        .expect("lift the suspension");
    test.app.poll().await.expect("poll the lift in");
}

/// Stores a live, admin-issued key and polls it in.
///
/// # Arguments
/// * `test` - the app.
/// * `name` - its name, also its actor (once per test).
/// * `access` - its level.
/// * `reach` - its catalogs; empty for every catalog.
/// * `maintenance_report` - whether it may submit maintenance reports.
///
/// # Returns
/// Its secret: look its principal up with [`cached_principal`] after each
/// poll, as a request would.
///
/// # Panics
/// If the store refuses the key or the poll fails.
pub(crate) async fn stored_secret(
    test: &TestApp,
    name: &str,
    access: Access,
    reach: &[&str],
    maintenance_report: bool,
) -> String {
    let names: Vec<String> = reach.iter().map(|catalog| catalog.to_string()).collect();
    let new = crate::store::keys::NewKey {
        name,
        access,
        maintenance_report,
        grants: if names.is_empty() {
            crate::store::keys::Grants::All
        } else {
            crate::store::keys::Grants::Catalogs(&names)
        },
        default_catalog: reach.first().copied(),
        actor: name,
        user: crate::db::TEST_USER,
        self_issued: false,
        expires_at: None,
        levels: &[],
    };
    let secret = crate::store::keys::create_with_id(test.app.pool(), &new, Utc::now())
        .await
        .expect("create the key")
        .key;
    test.app.poll().await.expect("poll the key in");
    secret
}

/// The principal the key cache holds now for `secret`, as the auth layer
/// would put it in a request.
///
/// # Panics
/// If the cache does not know the key.
pub(crate) fn cached_principal(test: &TestApp, secret: &str) -> Arc<Principal> {
    test.app.keys().find(secret).expect("the key is cached")
}

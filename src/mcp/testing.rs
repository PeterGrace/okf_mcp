//! Test support for the tools: an [`App`] over a throwaway database holding
//! catalogs, and principals and contexts to call tools with.

use std::sync::Arc;

use chrono::{DateTime, Utc};

use crate::app::App;
// Re-exported so tool tests keep calling `testing::files`; it lives in
// `archive`, beside `Files`, so store tests need not reach up into `mcp`.
pub(crate) use crate::archive::files;
use crate::auth::{Principal, Reach};
use crate::clock::SystemClock;
use crate::config::Config;
use crate::db::pg_support::TestDb;
use crate::mcp::context::ToolContext;
use crate::store::catalogs;
use crate::store::changesets::{self, ChangesetKey, ChangesetRow, EntryRow};
use crate::store::import::{import, ImportOptions};
use crate::store::keys::{self as key_rows, Access, Grants, NewKey};

/// An app and the database under it. Fields drop in declaration order: the
/// app (and its pool) before the container.
pub(crate) struct TestApp {
    /// The shared state, as a server would hold it.
    pub(crate) app: Arc<App>,
    _db: TestDb,
}

/// Creates each catalog, imports its files, and loads the app.
///
/// # Arguments
/// * `catalogs_and_files` - `(catalog name, its files)` pairs, in creation order.
/// * `config` - the server's settings.
///
/// # Returns
/// The loaded app and its database.
///
/// # Panics
/// If the database, a create or an import fails: a broken test setup.
pub(crate) async fn app_with(
    catalogs_and_files: &[(&str, &[(&str, &str)])],
    config: Config,
) -> TestApp {
    let db = crate::db::test_db().await;
    for (name, entries) in catalogs_and_files {
        catalogs::create(&db.pool, name, "", Utc::now())
            .await
            .expect("create the test catalog");
        let options = ImportOptions {
            replace: false,
            actor: "tester",
            now: Utc::now(),
        };
        import(&db.pool, name, files(entries), &options)
            .await
            .expect("import the test files");
    }
    let app = App::load(db.pool.clone(), config, Arc::new(SystemClock))
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
    Arc::new(Principal {
        key_id: key_id.to_string(),
        access,
        reach: if reach.is_empty() {
            Reach::All
        } else {
            Reach::Catalogs(reach.iter().map(|name| name.to_string()).collect())
        },
        default_catalog: default.map(str::to_string),
        actor: key_id.to_string(),
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
    let new = NewKey {
        id: key_id,
        access: Access::Admin,
        grants: Grants::All,
        default_catalog: None,
        actor: key_id,
    };
    key_rows::create(test.app.pool(), &new, Utc::now())
        .await
        .expect("store the test key");
}

/// One pending edit, for [`pending_entry`].
#[derive(Debug, Clone, Copy)]
pub(crate) struct PendingEdit<'a> {
    /// The catalog's name.
    pub(crate) catalog: &'a str,
    /// Catalog-relative path.
    pub(crate) path: &'a str,
    /// The new text; `None` deletes the path.
    pub(crate) text: Option<&'a str>,
    /// The time of the edit.
    pub(crate) at: DateTime<Utc>,
}

/// Stores an edit in the changeset of `ctx`'s key and label, in the lock
/// order (opening the changeset if need be), as a write tool would.
///
/// # Arguments
/// * `test` - the app.
/// * `ctx` - the connection: its key and label pick the changeset.
/// * `edit` - the catalog, path, text and time.
///
/// # Returns
/// The changeset after the edit.
///
/// # Panics
/// If the catalog is not loaded or the store fails.
pub(crate) async fn pending_entry(
    test: &TestApp,
    ctx: &ToolContext,
    edit: &PendingEdit<'_>,
) -> ChangesetRow {
    let state = test.app.catalog(edit.catalog).expect("a loaded catalog");
    let key = ChangesetKey {
        catalog_id: state.id(),
        key_id: &ctx.principal().key_id,
        label: ctx.label(),
    };
    let entry = EntryRow {
        path: edit.path.to_string(),
        base_revision: None,
        content: edit.text.map(|text| text.as_bytes().to_vec()),
        executable: false,
        replay: None,
    };
    let mut tx = test.app.pool().begin().await.expect("begin");
    changesets::lock_catalog(&mut tx, state.id())
        .await
        .expect("lock the catalog");
    let changeset = changesets::get_or_create(&mut tx, &key, &ctx.principal().actor, edit.at)
        .await
        .expect("open the changeset");
    changesets::upsert_entry(&mut tx, changeset.id, &entry, edit.at)
        .await
        .expect("store the entry");
    tx.commit().await.expect("commit");
    changesets::by_id(test.app.pool(), changeset.id)
        .await
        .expect("read the changeset back")
        .expect("the changeset exists")
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

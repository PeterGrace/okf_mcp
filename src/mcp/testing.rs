//! Test support for the tools: an [`App`] over a throwaway database holding
//! catalogs, and principals and contexts to call tools with.

use std::sync::Arc;

use chrono::Utc;

use crate::app::App;
use crate::archive::Files;
use crate::auth::{Principal, Reach};
use crate::clock::SystemClock;
use crate::config::Config;
use crate::db::pg_support::TestDb;
use crate::mcp::context::ToolContext;
use crate::okf::workspace::StoredFile;
use crate::store::catalogs;
use crate::store::import::{import, ImportOptions};
use crate::store::keys::Access;

/// An app and the database under it. Fields drop in declaration order: the
/// app (and its pool) before the container.
pub(crate) struct TestApp {
    /// The shared state, as a server would hold it.
    pub(crate) app: Arc<App>,
    _db: TestDb,
}

/// Literal files, as an import takes them.
///
/// # Arguments
/// * `entries` - `(path, text)` pairs.
///
/// # Returns
/// The files, none executable.
pub(crate) fn files(entries: &[(&str, &str)]) -> Files {
    entries
        .iter()
        .map(|(path, text)| {
            let file = StoredFile {
                content: text.as_bytes().to_vec(),
                executable: false,
            };
            (path.to_string(), file)
        })
        .collect()
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

/// A `read` key reaching `reach` (every catalog when empty), with a default.
///
/// # Arguments
/// * `reach` - the catalogs it is granted; empty for every catalog.
/// * `default` - its default catalog.
///
/// # Returns
/// The principal, as the auth layer would put it in a request.
pub(crate) fn principal(reach: &[&str], default: Option<&str>) -> Arc<Principal> {
    Arc::new(Principal {
        key_id: "tester".to_string(),
        access: Access::Read,
        reach: if reach.is_empty() {
            Reach::All
        } else {
            Reach::Catalogs(reach.iter().map(|name| name.to_string()).collect())
        },
        default_catalog: default.map(str::to_string),
        actor: "tester".to_string(),
    })
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

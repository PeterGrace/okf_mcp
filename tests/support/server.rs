//! An okf-mcp server for end-to-end tests: a throwaway Postgres, catalogs
//! imported from literal files, the real router on a free port, and rmcp
//! clients that talk to it over HTTP.

// Each test crate that includes `support` uses a different part of it.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use axum::http::{HeaderName, HeaderValue};
use chrono::Utc;
use okf_mcp::app::App;
use okf_mcp::archive::Files;
use okf_mcp::clock::SystemClock;
use okf_mcp::config::Config;
use okf_mcp::okf::workspace::StoredFile;
use okf_mcp::store::import::{import, ImportOptions, ImportScope};
use okf_mcp::store::keys::{self, Access, Grants, NewKey};
use okf_mcp::store::{catalogs, users};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::{RoleClient, ServiceExt};
use sqlx::PgPool;
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::pg::TestDb;

/// The task running `http::serve`.
type Served = JoinHandle<std::io::Result<()>>;

/// How long `restart` waits for the old server to stop.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// A connected MCP client.
pub type Client = RunningService<RoleClient, ()>;

/// The user every test database is given; its personal team, of the same
/// name, owns the catalogs the tests create.
pub const OWNER: &str = "tester";

/// Migrates a fresh database and creates [`OWNER`], so catalogs have a
/// team to belong to.
///
/// # Arguments
/// * `pool` - the unmigrated database.
///
/// # Panics
/// If the migration or the create fails: a broken test setup.
pub async fn migrate_with_owner(pool: &PgPool) {
    okf_mcp::db::migrate(pool).await.expect("migrations");
    let owner = users::NewUser {
        name: OWNER,
        display_name: "",
    };
    users::create(pool, &owner, Utc::now())
        .await
        .expect("create the owner");
}

/// Literal files, as an import takes them.
///
/// # Arguments
/// * `entries` - `(path, text)` pairs.
///
/// # Returns
/// The files, none executable.
pub fn files(entries: &[(&str, &str)]) -> Files {
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

/// A running server. Dropping it stops the server, then the database.
pub struct TestServer {
    /// `http://127.0.0.1:<port>`.
    pub base: String,
    /// The server's state, to `poll` after changing the database.
    pub app: Arc<App>,
    shutdown: CancellationToken,
    /// The task running `http::serve`, awaited by `restart`.
    served: Served,
    /// The database, for changes made "by the subcommands".
    pub db: TestDb,
}

// `Drop` runs when the value goes out of scope (like `__del__`, but
// deterministic): the server stops before the fields, and so the database,
// are dropped.
impl Drop for TestServer {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

impl TestServer {
    /// Creates and imports each catalog, then starts the server.
    ///
    /// # Arguments
    /// * `catalogs_and_files` - `(catalog name, its files)` pairs, in
    ///   creation order.
    /// * `config` - the server's settings.
    ///
    /// # Returns
    /// The running server.
    ///
    /// # Panics
    /// If the database, a create, an import or the bind fails: a broken
    /// test setup.
    pub async fn start(catalogs_and_files: Vec<(&str, Files)>, config: Config) -> Self {
        Self::start_with(catalogs_and_files, move |_| config).await
    }

    /// As [`TestServer::start`], with settings computed from the server's
    /// own base URL, which is known only once its port is bound: OAuth's
    /// `public_url` must name it.
    ///
    /// # Arguments
    /// * `catalogs_and_files` - as for `start`.
    /// * `make_config` - builds the settings from `http://127.0.0.1:<port>`.
    ///
    /// # Returns
    /// The running server.
    ///
    /// # Panics
    /// As [`TestServer::start`].
    // `impl FnOnce(&str) -> Config`: any closure called once with the base
    // URL, like a Python callable argument.
    pub async fn start_with(
        catalogs_and_files: Vec<(&str, Files)>,
        make_config: impl FnOnce(&str) -> Config,
    ) -> Self {
        let db = TestDb::start_migrated().await;
        migrate_with_owner(&db.pool).await;
        for (name, files) in catalogs_and_files {
            catalogs::create(&db.pool, name, "", OWNER, Utc::now())
                .await
                .expect("create catalog");
            let options = ImportOptions {
                replace: false,
                scope: ImportScope::Everything,
                expect_generation: None,
                actor: "tester",
                now: Utc::now(),
            };
            import(&db.pool, name, files, &options)
                .await
                .expect("import");
        }
        let (base, app, shutdown, served) = serve_app(&db, make_config).await;
        Self {
            base,
            app,
            shutdown,
            served,
            db,
        }
    }

    /// Stops the server and starts a new one on the same database, as a
    /// pod restart would: nothing in memory survives, everything stored does.
    ///
    /// # Arguments
    /// * `config` - the new server's settings.
    ///
    /// The old server has stopped (its poll loop ended, its connections
    /// closed) before the new one starts, so the two never share the database.
    /// Close every client first: an open connection holds the old server up.
    ///
    /// # Panics
    /// As [`TestServer::start`], or if the old server does not stop within
    /// [`STOP_TIMEOUT`] or its task fails.
    pub async fn restart(&mut self, config: Config) {
        self.shutdown.cancel();
        // A `JoinHandle` is a future of the task's result (like awaiting an
        // `asyncio.Task`); `&mut` awaits it in place, since `Drop` forbids
        // moving fields out of `self`. The outer `expect` is the timeout, the
        // middle one a panic in the task, the inner one `serve`'s I/O error.
        tokio::time::timeout(STOP_TIMEOUT, &mut self.served)
            .await
            .expect("the old server stops")
            .expect("the old server's task")
            .expect("the old server serves without an I/O error");
        let (base, app, shutdown, served) = serve_app(&self.db, move |_| config).await;
        self.base = base;
        self.app = app;
        self.shutdown = shutdown;
        self.served = served;
    }

    /// The MCP endpoint.
    ///
    /// # Returns
    /// `<base>/mcp`.
    pub fn mcp_url(&self) -> String {
        format!("{}/mcp", self.base)
    }

    /// Creates a key and reloads the server's key cache.
    ///
    /// # Arguments
    /// * `id` - the key's id (also its actor).
    /// * `access` - its access level.
    /// * `catalogs` - its grants; empty for every catalog.
    /// * `default` - its default catalog.
    ///
    /// # Returns
    /// The key, to send as a bearer token.
    ///
    /// # Panics
    /// If the store refuses the key or the poll fails.
    pub async fn key(
        &self,
        id: &str,
        access: Access,
        catalogs: &[&str],
        default: Option<&str>,
    ) -> String {
        let names: Vec<String> = catalogs.iter().map(|name| name.to_string()).collect();
        let grants = if names.is_empty() {
            Grants::All
        } else {
            Grants::Catalogs(&names)
        };
        let new = NewKey {
            name: id,
            access,
            maintenance_report: false,
            grants,
            default_catalog: default,
            actor: id,
            user: OWNER,
            self_issued: false,
            expires_at: None,
            levels: &[],
        };
        let key = keys::create(&self.db.pool, &new, Utc::now())
            .await
            .expect("create key");
        self.app.poll().await.expect("poll");
        key
    }

    /// Inserts a key called `name`, granted every catalog, with no user, as
    /// a key made before users existed is, then reloads the key cache. No
    /// store function makes one any more, so this writes the row itself,
    /// and bumps the administrative generation so the poll sees it.
    ///
    /// # Arguments
    /// * `name` - its id, name and actor.
    /// * `access` - its access level.
    ///
    /// # Returns
    /// The key, to send as a bearer token.
    ///
    /// # Panics
    /// If the insert or the poll fails.
    pub async fn legacy_key(&self, name: &str, access: Access) -> String {
        let secret = okf_mcp::keys::generate().expect("generate a key");
        sqlx::query(
            "INSERT INTO api_keys (id, name, sha256, access, all_catalogs, actor, created_at) \
             VALUES ($1, $1, $2, $3, true, $1, now())",
        )
        .bind(name)
        .bind(okf_mcp::keys::hash(&secret).as_slice())
        .bind(access.as_str())
        .execute(&self.db.pool)
        .await
        .expect("insert a legacy key");
        sqlx::query("UPDATE server_state SET admin_generation = admin_generation + 1")
            .execute(&self.db.pool)
            .await
            .expect("bump the admin generation");
        self.app.poll().await.expect("poll");
        secret
    }

    /// Creates a user with an email and a password, and polls it into the
    /// key cache.
    ///
    /// # Arguments
    /// * `name` - the user's name.
    /// * `email` - their email, which signs them in.
    /// * `password` - their password (within the policy).
    ///
    /// # Returns
    /// The user's id.
    ///
    /// # Panics
    /// If the store refuses the user, the email or the password.
    pub async fn user_with_password(&self, name: &str, email: &str, password: &str) -> String {
        let pool = &self.db.pool;
        users::create(
            pool,
            &users::NewUser {
                name,
                display_name: "",
            },
            Utc::now(),
        )
        .await
        .expect("create the user");
        users::set_email(pool, name, Some(email))
            .await
            .expect("set the email");
        let user = users::find_by_email(pool, email)
            .await
            .expect("find the user")
            .expect("the user exists");
        okf_mcp::store::passwords::set(pool, &user.id, password, Utc::now())
            .await
            .expect("set the password");
        self.app.poll().await.expect("poll the user in");
        user.id
    }

    /// Creates a read-only maintenance reporter and refreshes the key cache.
    pub async fn reporter_key(&self, name: &str, catalogs: &[&str]) -> String {
        let key = self.key(name, Access::Read, catalogs, None).await;
        let id = self
            .app
            .keys()
            .find(&key)
            .expect("created principal")
            .key_id
            .clone();
        keys::set_maintenance_report_target(&self.db.pool, &id, None, true)
            .await
            .expect("enable reporting");
        self.app.poll().await.expect("poll");
        key
    }

    /// An MCP client with `key`, on a connection naming `catalogs` (the
    /// `X-OKF-Catalogs` header), if any.
    ///
    /// # Arguments
    /// * `key` - the bearer key.
    /// * `catalogs` - the header's value (`"pg,work"`), if any.
    ///
    /// # Returns
    /// The connected client.
    ///
    /// # Errors
    /// The client's failure to initialize (a refused key, say), as text.
    pub async fn client(&self, key: &str, catalogs: Option<&str>) -> Result<Client, String> {
        // `Option::map` then `into_iter` yields zero or one pair: no header, or one.
        let headers: Vec<(&'static str, &str)> = catalogs
            .map(|list| ("x-okf-catalogs", list))
            .into_iter()
            .collect();
        self.client_with(key, &headers).await
    }

    /// An MCP client with `key` and extra request headers (`X-OKF-Catalogs`,
    /// say) on every request.
    ///
    /// # Arguments
    /// * `key` - the bearer key.
    /// * `headers` - `(lowercase name, value)` pairs.
    ///
    /// # Returns
    /// The connected client.
    ///
    /// # Errors
    /// As [`TestServer::client`].
    ///
    /// # Panics
    /// If a name is not lowercase or a value is not a valid header value.
    pub async fn client_with(
        &self,
        key: &str,
        headers: &[(&'static str, &str)],
    ) -> Result<Client, String> {
        self.client_at(key, &self.mcp_url(), headers).await
    }

    /// An MCP client with `key` and extra request headers on `url` (the
    /// MCP URL with a query, say).
    ///
    /// # Arguments
    /// * `key` - the bearer key.
    /// * `url` - the MCP endpoint to connect to.
    /// * `headers` - `(lowercase name, value)` pairs.
    ///
    /// # Returns
    /// The connected client.
    ///
    /// # Errors
    /// As [`TestServer::client`].
    ///
    /// # Panics
    /// If a name is not lowercase or a value is not a valid header value.
    pub async fn client_at(
        &self,
        key: &str,
        url: &str,
        headers: &[(&'static str, &str)],
    ) -> Result<Client, String> {
        let headers: HashMap<HeaderName, HeaderValue> = headers
            .iter()
            // `&(name, value)` copies the two references out of each pair.
            .map(|&(name, value)| {
                let value = HeaderValue::from_str(value).expect("header value");
                (HeaderName::from_static(name), value)
            })
            .collect();
        let config = StreamableHttpClientTransportConfig::with_uri(url.to_string())
            .auth_header(key)
            .custom_headers(headers);
        // `()` is a client handler that does nothing: tests only call tools.
        ().serve(StreamableHttpClientTransport::from_config(config))
            .await
            .map_err(|error| error.to_string())
    }
}

/// Binds a free port, loads the app on `db` with settings built from the
/// port's URL, and serves it.
///
/// # Arguments
/// * `db` - the database, already migrated and imported.
/// * `make_config` - builds the settings from the base URL.
///
/// # Returns
/// The base URL, the app, the token that stops the server, and the task
/// serving it.
///
/// # Panics
/// If the bind fails or the app does not load.
async fn serve_app(
    db: &TestDb,
    make_config: impl FnOnce(&str) -> Config,
) -> (String, Arc<App>, CancellationToken, Served) {
    // Port 0: the OS picks a free one. Bound first, so the settings can
    // name the server's own URL.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let mut config = make_config(&base);
    // Test servers only enqueue; the mail tests run their own worker.
    config.mail.worker = false;
    let app = App::load(db.pool.clone(), config, Arc::new(SystemClock))
        .await
        .expect("app loads");
    let shutdown = CancellationToken::new();
    let served = tokio::spawn(okf_mcp::http::serve(
        Arc::clone(&app),
        listener,
        shutdown.clone(),
    ));
    (base, app, shutdown, served)
}

/// Calls a tool.
///
/// # Arguments
/// * `client` - a connected client.
/// * `tool` - the tool's name.
/// * `arguments` - a JSON object.
///
/// # Returns
/// The tool's result (an error result included).
///
/// # Panics
/// If `arguments` is not an object or the call itself fails.
pub async fn call(
    client: &Client,
    tool: &'static str,
    arguments: serde_json::Value,
) -> CallToolResult {
    let arguments = arguments.as_object().expect("arguments object").clone();
    client
        .call_tool(CallToolRequestParams::new(tool).with_arguments(arguments))
        .await
        .expect("tool call")
}

/// Opens a changeset through the `open_changeset` tool (M8 Decision 1).
///
/// # Arguments
/// * `client` - a connected client with a `write` key.
/// * `catalog` - the catalog; `None` for the connection's write target.
///
/// # Returns
/// The handle to pass as `changeset`.
///
/// # Panics
/// If the tool fails or its result has no string `changeset`.
pub async fn open_changeset(client: &Client, catalog: Option<&str>) -> String {
    // `json!` writes `null` for `None`, which the tool reads as no catalog.
    let opened = call(
        client,
        "open_changeset",
        serde_json::json!({"catalog": catalog}),
    )
    .await;
    structured(&opened)["changeset"]
        .as_str()
        .expect("a string handle")
        .to_string()
}

/// A tool's structured result, asserting it is not an error.
///
/// # Arguments
/// * `result` - what [`call`] returned.
///
/// # Returns
/// Its `structuredContent`.
///
/// # Panics
/// If the result is an error or has no structured content.
pub fn structured(result: &CallToolResult) -> &serde_json::Value {
    assert_ne!(result.is_error, Some(true), "tool failed: {result:?}");
    result
        .structured_content
        .as_ref()
        .expect("structured content")
}

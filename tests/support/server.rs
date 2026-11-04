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
use okf_mcp::store::catalogs;
use okf_mcp::store::import::{import, ImportOptions};
use okf_mcp::store::keys::{self, Access, Grants, NewKey};
use rmcp::model::{CallToolRequestParams, CallToolResult};
use rmcp::service::RunningService;
use rmcp::transport::streamable_http_client::StreamableHttpClientTransportConfig;
use rmcp::transport::StreamableHttpClientTransport;
use rmcp::{RoleClient, ServiceExt};
use tokio::task::JoinHandle;
use tokio_util::sync::CancellationToken;

use super::pg::TestDb;

/// The task running `http::serve`.
type Served = JoinHandle<std::io::Result<()>>;

/// How long `restart` waits for the old server to stop.
pub const STOP_TIMEOUT: Duration = Duration::from_secs(10);

/// A connected MCP client.
pub type Client = RunningService<RoleClient, ()>;

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
        let db = TestDb::start().await;
        okf_mcp::db::migrate(&db.pool).await.expect("migrations");
        for (name, files) in catalogs_and_files {
            catalogs::create(&db.pool, name, "", Utc::now())
                .await
                .expect("create catalog");
            let options = ImportOptions {
                replace: false,
                actor: "tester",
                now: Utc::now(),
            };
            import(&db.pool, name, files, &options)
                .await
                .expect("import");
        }
        let (base, app, shutdown, served) = serve_app(&db, config).await;
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
        let (base, app, shutdown, served) = serve_app(&self.db, config).await;
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
            id,
            access,
            grants,
            default_catalog: default,
            actor: id,
        };
        let key = keys::create(&self.db.pool, &new, Utc::now())
            .await
            .expect("create key");
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
        let headers: HashMap<HeaderName, HeaderValue> = headers
            .iter()
            // `&(name, value)` copies the two references out of each pair.
            .map(|&(name, value)| {
                let value = HeaderValue::from_str(value).expect("header value");
                (HeaderName::from_static(name), value)
            })
            .collect();
        let config = StreamableHttpClientTransportConfig::with_uri(self.mcp_url())
            .auth_header(key)
            .custom_headers(headers);
        // `()` is a client handler that does nothing: tests only call tools.
        ().serve(StreamableHttpClientTransport::from_config(config))
            .await
            .map_err(|error| error.to_string())
    }
}

/// Loads the app on `db` and serves it on a free port.
///
/// # Arguments
/// * `db` - the database, already migrated and imported.
/// * `config` - the server's settings.
///
/// # Returns
/// The base URL, the app, the token that stops the server, and the task
/// serving it.
///
/// # Panics
/// If the app does not load or the bind fails.
async fn serve_app(db: &TestDb, config: Config) -> (String, Arc<App>, CancellationToken, Served) {
    let app = App::load(db.pool.clone(), config, Arc::new(SystemClock))
        .await
        .expect("app loads");
    // Port 0: the OS picks a free one.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
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

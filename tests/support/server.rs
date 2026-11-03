//! An okf-mcp server for end-to-end tests: a throwaway Postgres, catalogs
//! imported from literal files, the real router on a free port, and rmcp
//! clients that talk to it over HTTP.

// Each test crate that includes `support` uses a different part of it.
#![allow(dead_code)]

use std::collections::HashMap;
use std::sync::Arc;

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
use tokio_util::sync::CancellationToken;

use super::pg::TestDb;

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
        let app = App::load(db.pool.clone(), config, Arc::new(SystemClock))
            .await
            .expect("app loads");
        // Port 0: the OS picks a free one.
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let shutdown = CancellationToken::new();
        tokio::spawn(okf_mcp::http::serve(
            Arc::clone(&app),
            listener,
            shutdown.clone(),
        ));
        Self {
            base,
            app,
            shutdown,
            db,
        }
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
        let mut headers = HashMap::new();
        if let Some(list) = catalogs {
            headers.insert(
                HeaderName::from_static("x-okf-catalogs"),
                HeaderValue::from_str(list).expect("header value"),
            );
        }
        let config = StreamableHttpClientTransportConfig::with_uri(self.mcp_url())
            .auth_header(key)
            .custom_headers(headers);
        // `()` is a client handler that does nothing: tests only call tools.
        ().serve(StreamableHttpClientTransport::from_config(config))
            .await
            .map_err(|error| error.to_string())
    }
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

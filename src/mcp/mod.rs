//! The MCP server (design "Tools"): the rmcp handler and the read tools,
//! one module per group. Each `#[tool]` method builds a
//! [`context::ToolContext`] from the HTTP request rmcp hands it and calls a
//! plain function, which the module's tests call directly.

mod catalogs;
// `pub` (not `pub(crate)`): the context's helpers, like `ToolResult`,
// `tool_error` and `reply` below, are used by later tool modules, and a
// public item is never flagged as dead code meanwhile.
pub mod context;
#[cfg(test)]
pub(crate) mod testing;

use std::sync::Arc;

use axum::http::request::Parts;
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::model::{CallToolResult, Implementation, ServerCapabilities, ServerConfig};
use rmcp::{tool_handler, Json, ServerHandler};

use crate::app::App;
use crate::error::OkfError;
use crate::mcp::context::ToolContext;

/// What the server tells a client it is for.
pub const INSTRUCTIONS: &str = "Open Knowledge Format catalogs: concepts (markdown with YAML \
    frontmatter) about systems, projects, people and decisions. Start with `search`, then \
    `get_concept`. Paths look like /infra/time-server.md (the leading slash and .md are \
    optional). Without a `catalog` argument, tools use this connection's catalogs \
    (`list_catalogs` shows them).";

/// The MCP handler: one per session (rmcp's factory makes it).
pub struct OkfServer {
    app: Arc<App>,
    tool_router: ToolRouter<OkfServer>,
}

impl OkfServer {
    /// A handler over the shared state.
    ///
    /// # Arguments
    /// * `app` - the shared state.
    ///
    /// # Returns
    /// The handler, with every tool registered.
    pub fn new(app: Arc<App>) -> Self {
        Self {
            app,
            tool_router: Self::tools(),
        }
    }

    /// Every tool: each module's router, joined with `+`.
    ///
    /// # Returns
    /// The router `#[tool_handler]` dispatches calls through.
    pub fn tools() -> ToolRouter<Self> {
        Self::catalogs_router()
    }

    /// The context of the request rmcp handed a tool. The error is already a
    /// tool result, so a tool writes `let ctx = self.context(&parts)?;`.
    ///
    /// # Arguments
    /// * `parts` - the HTTP request.
    ///
    /// # Returns
    /// The call's context.
    ///
    /// # Errors
    /// As [`ToolContext::from_parts`], as an `isError` result.
    // `CallToolResult` is a large error type, but rmcp fixes it: a tool's
    // error must be one, and `?` here passes it straight through.
    #[allow(clippy::result_large_err)]
    pub(crate) fn context(&self, parts: &Parts) -> Result<ToolContext, CallToolResult> {
        ToolContext::from_parts(&self.app, parts).map_err(|error| tool_error(&error))
    }
}

// `#[tool_handler]` writes `ServerHandler`'s `list_tools` and `call_tool`
// over the stored router; `get_info` is ours.
#[tool_handler(router = self.tool_router)]
impl ServerHandler for OkfServer {
    fn get_info(&self) -> ServerConfig {
        // `env!` reads this crate's name and version at compile time.
        ServerConfig::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new(
                env!("CARGO_PKG_NAME"),
                env!("CARGO_PKG_VERSION"),
            ))
            .with_instructions(INSTRUCTIONS)
    }
}

/// What every tool returns: a structured result, or an `isError` result.
///
/// A `#[tool]` method spells this type out (`Result<Json<T>, CallToolResult>`)
/// rather than naming the alias: rmcp's macro builds the tool's output schema
/// only when it sees `Json<...>` in the written return type.
pub type ToolResult<T> = Result<Json<T>, CallToolResult>;

/// An error as a tool result: `isError: true`, its JSON in the content.
/// Internal errors are logged here, with their details; the others at debug
/// level, by kind only (their messages can name paths from the catalog).
///
/// # Arguments
/// * `error` - what went wrong.
///
/// # Returns
/// The result to send the client.
pub fn tool_error(error: &OkfError) -> CallToolResult {
    if error.is_internal() {
        // `%error` logs it with `Display`; it never holds a key or a document.
        tracing::error!(%error, "tool failed");
    } else {
        tracing::debug!(kind = error.kind(), "tool error");
    }
    CallToolResult::structured_error(error.to_json())
}

/// A tool function's outcome as a tool result.
///
/// # Arguments
/// * `result` - the plain function's value or error.
///
/// # Returns
/// The value as structured content, or the error as an `isError` result.
// A large error type, fixed by rmcp, as for `OkfServer::context`.
#[allow(clippy::result_large_err)]
pub fn reply<T>(result: Result<T, OkfError>) -> ToolResult<T> {
    result.map(Json).map_err(|error| tool_error(&error))
}

/// Runs CPU work (an analysis, a search) on a blocking thread, so the async
/// threads stay free; a panic in it is re-raised here.
///
/// # Arguments
/// * `work` - the closure to run; `'static` because it may outlive the
///   caller's borrows, so it must own (or `Arc`) what it reads.
///
/// # Returns
/// What `work` returns.
pub async fn blocking<T, F>(work: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    tokio::task::spawn_blocking(work)
        .await
        .unwrap_or_else(|error| std::panic::resume_unwind(error.into_panic()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::Request;

    use crate::config::Config;

    #[test]
    fn reply_passes_values_and_turns_errors_into_error_results() {
        let found: Result<u32, OkfError> = Ok(7);
        let missing: Result<u32, OkfError> = Err(OkfError::NotFound {
            path: "/x.md".to_string(),
        });

        let found = reply(found);
        let missing = reply(missing);

        // `Json<T>` is a one-field tuple struct; `.0` is the value inside.
        assert_eq!(found.ok().map(|json| json.0), Some(7));
        let error = missing.err().expect("an error result");
        assert_eq!(error.is_error, Some(true));
        assert_eq!(
            error.structured_content.as_ref().unwrap()["error"],
            "NotFound"
        );
        assert_eq!(error.structured_content.unwrap()["path"], "/x.md");
    }

    #[tokio::test]
    async fn blocking_returns_the_work_and_re_raises_its_panic() {
        let value = blocking(|| 2 + 2).await;
        // `tokio::spawn` catches the re-raised panic as a `JoinError`.
        let panicked = tokio::spawn(blocking(|| panic!("boom"))).await;

        assert_eq!(value, 4);
        let error = panicked.expect_err("the panic is re-raised");
        assert!(error.is_panic());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_server_names_itself_and_needs_a_principal_on_the_request() {
        let test = testing::app_with(&[("pg", &[])], Config::default()).await;
        let server = OkfServer::new(Arc::clone(&test.app));
        let (mut with_key, _) = Request::new(()).into_parts();
        with_key.extensions.insert(testing::principal(&[], None));
        let (without_key, _) = Request::new(()).into_parts();

        let info = server.get_info();
        let admitted = server.context(&with_key);
        let refused = server.context(&without_key);

        assert_eq!(info.server_info.name, env!("CARGO_PKG_NAME"));
        assert!(info.capabilities.tools.is_some());
        assert_eq!(
            admitted
                .ok()
                .map(|ctx| ctx.principal().key_id.clone())
                .as_deref(),
            Some("tester")
        );
        let error = refused.err().expect("no principal, no context");
        assert_eq!(error.is_error, Some(true));
        assert_eq!(error.structured_content.unwrap()["error"], "Forbidden");
    }
}

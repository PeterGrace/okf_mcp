//! Authenticated, report-only MCP transport. Retry scheduling belongs to the supervisor.
//!
//! Raw SSE events are bounded by rmcp. JSON bodies can be buffered/decoded by
//! reqwest before our serialized-size check; that check is not a memory bound.
//!
//! Integration requirement: keep rmcp diagnostics disabled for live runner use.
//! The SDK itself can trace raw wire messages; redacting HTTP errors here does
//! not sanitize SDK event tracing. This module does not configure global logging.

use std::{
    collections::{HashMap, HashSet},
    future::Future,
    sync::{
        atomic::{AtomicU8, Ordering},
        Arc,
    },
    time::Duration,
};

use reqwest::header::{HeaderName, HeaderValue};
use rmcp::{
    model::{CallToolRequestParams, ClientJsonRpcMessage, PaginatedRequestParams, Tool},
    service::{ClientInitializeError, RunningService},
    transport::{
        common::client_side_sse::{BoxedSseResponse, NeverRetry},
        streamable_http_client::{
            StreamableHttpClient, StreamableHttpClientTransportConfig, StreamableHttpError,
            StreamableHttpPostResponse,
        },
        StreamableHttpClientTransport,
    },
    RoleClient, ServiceExt,
};
use serde_json::Value;

use super::{
    config::{RunnerConfig, Secrets},
    contract::{self, PAYLOAD_CAP},
    error::RunnerError,
    types::ToolRequest,
};

const RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// Controller/fake interchange; only the fixed AllowedTool enum can be dispatched.
pub trait CatalogClient: Send {
    fn list_tools(&mut self) -> impl Future<Output = Result<Vec<Tool>, RunnerError>> + Send;
    fn call(
        &mut self,
        request: ToolRequest,
    ) -> impl Future<Output = Result<Value, RunnerError>> + Send;
}

/// A preflighted connection bound to one catalog. No raw peer or HTTP client escapes.
///
/// The HTTP-only test gate is not a production API:
/// ```compile_fail
/// use okf_mcp::investigator::client::McpClient;
/// let _ = McpClient::loopback("http://127.0.0.1/mcp", "pg", "token");
/// ```
pub struct McpClient {
    service: RunningService<RoleClient, ()>,
    catalog: String,
    fatal: Arc<AtomicU8>,
}

/// Connect using explicit credentials and preflight all nine tool contracts.
/// Both live-consent flags are required before network access. Successful offline
/// configuration checks alone never imply network readiness.
pub async fn connect(config: &RunnerConfig, secrets: &Secrets) -> Result<McpClient, RunnerError> {
    config.validate_consent()?;
    McpClient::open(&config.endpoint, &config.catalog, secrets.token()).await
}

fn http_client() -> Result<reqwest::Client, RunnerError> {
    // reqwest 0.13's rustls backend uses builder_with_provider and explicitly
    // selects aws-lc when no process provider exists. Unlike ClientConfig::builder,
    // it does not panic when both sqlx's ring and reqwest's aws-lc are enabled.
    // Do not install a process-global provider and change the server's TLS policy.
    reqwest::Client::builder()
        .tls_backend_rustls()
        .redirect(reqwest::redirect::Policy::none())
        .no_proxy()
        .retry(reqwest::retry::never())
        .connect_timeout(RPC_TIMEOUT)
        .timeout(RPC_TIMEOUT)
        .build()
        .map_err(|_| RunnerError::Config)
}

impl McpClient {
    // Only connect and the cfg(test) loopback gate call this private constructor.
    async fn open(endpoint: &str, catalog: &str, token: &str) -> Result<Self, RunnerError> {
        if catalog.trim().is_empty() || token.trim().is_empty() {
            return Err(RunnerError::Config);
        }
        HeaderValue::from_str(&format!("Bearer {token}")).map_err(|_| RunnerError::Config)?;
        let fatal = Arc::new(AtomicU8::new(0));
        let client = RedactedHttp {
            client: http_client()?,
            fatal: fatal.clone(),
        };
        let mut transport_config =
            StreamableHttpClientTransportConfig::with_uri(endpoint.to_owned())
                .auth_header(token)
                .max_sse_event_size(PAYLOAD_CAP)
                .max_concurrent_requests(1)
                .reinit_on_expired_session(false);
        transport_config.retry_config = Arc::new(NeverRetry::default());
        transport_config.control_request_timeout = RPC_TIMEOUT;
        let transport = StreamableHttpClientTransport::with_client(client, transport_config);
        let service = tokio::time::timeout(RPC_TIMEOUT, ().serve(transport))
            .await
            .map_err(|_| RunnerError::Transport)?
            .map_err(|error| {
                fatal_error(&fatal).unwrap_or_else(|| initialization_failure(&error))
            })?;
        let mut client = Self {
            service,
            catalog: catalog.into(),
            fatal,
        };
        client.list_tools().await?;
        Ok(client)
    }

    fn ready(&self) -> Result<(), RunnerError> {
        fatal_error(&self.fatal).map_or(Ok(()), Err)
    }

    fn failure(&self, error: rmcp::ServiceError) -> RunnerError {
        fatal_error(&self.fatal).unwrap_or(match error {
            // rmcp collapses an oversized/invalid SSE response into TransportClosed.
            // It cannot safely be classified as a transient network failure.
            rmcp::ServiceError::TransportSend(_) | rmcp::ServiceError::Timeout { .. } => {
                RunnerError::Transport
            }
            _ => RunnerError::Contract,
        })
    }

    fn remember(&self, error: RunnerError) -> RunnerError {
        let code = match error {
            RunnerError::Permission => 1,
            RunnerError::Contract => 2,
            _ => 0,
        };
        if code != 0 {
            let _ = self
                .fatal
                .compare_exchange(0, code, Ordering::SeqCst, Ordering::SeqCst);
        }
        error
    }
}

impl CatalogClient for McpClient {
    async fn list_tools(&mut self) -> Result<Vec<Tool>, RunnerError> {
        self.ready()?;
        let mut tools = Vec::new();
        let mut cursor = None;
        let mut seen = HashSet::new();
        for _ in 0..10 {
            let page = tokio::time::timeout(
                RPC_TIMEOUT,
                self.service.list_tools(
                    cursor
                        .map(|cursor| PaginatedRequestParams::default().with_cursor(Some(cursor))),
                ),
            )
            .await
            .map_err(|_| RunnerError::Transport)?
            .map_err(|e| self.remember(self.failure(e)))?;
            self.ready()?;
            contract::bounded(&page).map_err(|e| self.remember(e))?;
            tools.extend(page.tools);
            contract::bounded(&tools).map_err(|e| self.remember(e))?;
            match page.next_cursor {
                None => {
                    contract::validate_tools(&tools).map_err(|e| self.remember(e))?;
                    return Ok(tools);
                }
                Some(next) if seen.insert(next.clone()) => cursor = Some(next),
                Some(_) => return Err(self.remember(RunnerError::Contract)),
            }
        }
        Err(self.remember(RunnerError::Contract))
    }

    async fn call(&mut self, request: ToolRequest) -> Result<Value, RunnerError> {
        self.ready()?;
        let tool = request.tool;
        let arguments = contract::arguments(request, &self.catalog)?;
        let response = tokio::time::timeout(
            RPC_TIMEOUT,
            self.service
                .call_tool(CallToolRequestParams::new(tool.name()).with_arguments(arguments)),
        )
        .await
        .map_err(|_| RunnerError::Transport)?
        .map_err(|e| self.remember(self.failure(e)))?;
        self.ready()?;
        contract::result(tool, response, &self.catalog).map_err(|e| self.remember(e))
    }
}

fn initialization_failure(error: &ClientInitializeError) -> RunnerError {
    match error {
        ClientInitializeError::TransportError { error, .. } => {
            match error
                .error
                .downcast_ref::<StreamableHttpError<RunnerError>>()
            {
                Some(StreamableHttpError::Client(kind)) => *kind,
                _ => RunnerError::Contract,
            }
        }
        // Only the failed final phase is causal; an earlier discovery failure
        // must not override a later protocol error or authentication refusal.
        ClientInitializeError::LegacyFallbackFailed { fallback, .. } => {
            initialization_failure(fallback)
        }
        _ => RunnerError::Contract,
    }
}

fn fatal_error(fatal: &AtomicU8) -> Option<RunnerError> {
    match fatal.load(Ordering::SeqCst) {
        1 => Some(RunnerError::Permission),
        2 => Some(RunnerError::Contract),
        _ => None,
    }
}

// rmcp's reqwest adapter retains remote bodies in UnexpectedServerResponse and
// its worker logs transport failures. Strip these before they reach that worker.
// The adapter exposes no final response URL; redirects are disabled in reqwest.
#[derive(Clone)]
struct RedactedHttp {
    client: reqwest::Client,
    fatal: Arc<AtomicU8>,
}
impl RedactedHttp {
    fn redact(
        &self,
        error: StreamableHttpError<reqwest::Error>,
    ) -> StreamableHttpError<RunnerError> {
        let kind = match error {
            StreamableHttpError::ServerDoesNotSupportSse => {
                return StreamableHttpError::ServerDoesNotSupportSse
            }
            StreamableHttpError::AuthRequired(_) | StreamableHttpError::InsufficientScope(_) => {
                RunnerError::Permission
            }
            // rmcp 3.8 preserves bare 401/403 here, not as reqwest status errors.
            StreamableHttpError::UnexpectedServerResponse(ref message)
                if message.starts_with("HTTP 401 ") || message.starts_with("HTTP 403 ") =>
            {
                RunnerError::Permission
            }
            StreamableHttpError::Client(ref error)
                if error
                    .status()
                    .is_some_and(|s| s.as_u16() == 401 || s.as_u16() == 403) =>
            {
                RunnerError::Permission
            }
            StreamableHttpError::Client(ref error)
                if error.is_connect() || error.is_timeout() || error.is_body() =>
            {
                RunnerError::Transport
            }
            StreamableHttpError::HttpStatus(_) => RunnerError::Transport,
            _ => RunnerError::Contract,
        };
        let code = match kind {
            RunnerError::Permission => 1,
            RunnerError::Contract => 2,
            _ => 0,
        };
        if code != 0 {
            let _ = self
                .fatal
                .compare_exchange(0, code, Ordering::SeqCst, Ordering::SeqCst);
        }
        StreamableHttpError::Client(kind)
    }
}
impl StreamableHttpClient for RedactedHttp {
    type Error = RunnerError;

    async fn post_message(
        &self,
        uri: Arc<str>,
        message: ClientJsonRpcMessage,
        session_id: Option<Arc<str>>,
        auth_header: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<StreamableHttpPostResponse, StreamableHttpError<Self::Error>> {
        if let Some(error) = fatal_error(&self.fatal) {
            return Err(StreamableHttpError::Client(error));
        }
        self.client
            .post_message_with_max_sse_event_size(
                uri,
                message,
                session_id,
                auth_header,
                headers,
                PAYLOAD_CAP,
            )
            .await
            .map_err(|e| self.redact(e))
    }

    async fn delete_session(
        &self,
        uri: Arc<str>,
        session_id: Arc<str>,
        auth_header: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<(), StreamableHttpError<Self::Error>> {
        self.client
            .delete_session(uri, session_id, auth_header, headers)
            .await
            .map_err(|e| self.redact(e))
    }

    async fn get_stream(
        &self,
        uri: Arc<str>,
        session_id: Option<Arc<str>>,
        last_event_id: Option<String>,
        auth_header: Option<String>,
        headers: HashMap<HeaderName, HeaderValue>,
    ) -> Result<BoxedSseResponse, StreamableHttpError<Self::Error>> {
        self.client
            .get_stream_with_max_sse_event_size(
                uri,
                session_id,
                last_event_id,
                auth_header,
                headers,
                PAYLOAD_CAP,
            )
            .await
            .map_err(|e| self.redact(e))
    }
}

#[cfg(test)]
impl McpClient {
    async fn loopback(endpoint: &str, catalog: &str, token: &str) -> Result<Self, RunnerError> {
        let url = reqwest::Url::parse(endpoint).map_err(|_| RunnerError::Config)?;
        let loopback = url
            .host_str()
            .and_then(|h| h.parse::<std::net::IpAddr>().ok())
            .is_some_and(|ip| ip.is_loopback());
        if url.scheme() != "http"
            || !loopback
            || !url.username().is_empty()
            || url.password().is_some()
            || url.fragment().is_some()
        {
            return Err(RunnerError::Config);
        }
        Self::open(endpoint, catalog, token).await
    }
}

#[cfg(test)]
#[path = "client_tests.rs"]
mod tests;

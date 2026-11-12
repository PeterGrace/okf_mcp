//! The HTTP server (design "Process and routes"): `/healthz`, `/mcp`, the
//! archive routes (`archives`), the JSON API under `/api/v1` (`api`) and the
//! web console under `/ui/` (`ui`), the OAuth routes (`crate::oauth`) when
//! OAuth is on, with request tracing, timeouts and the key layer, on
//! `/mcp` the session binding (`sessions`), and console sign-in sessions
//! on `/api/v1` (`console_session`).

pub(crate) mod api;
mod archives;
pub mod console_session;
pub mod limits;
pub(crate) mod pages;
pub mod sessions;
mod ui;

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use serde::Serialize;
use tokio::net::TcpListener;
use tokio_util::sync::CancellationToken;
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::{DefaultMakeSpan, DefaultOnResponse, TraceLayer};
use tracing::Level;

use crate::app::App;
use crate::auth::require_key;
use crate::clock_check::ClockStatus;
use crate::error::OkfError;
use crate::http::sessions::bind_session;
use crate::mcp::OkfServer;
use crate::oauth;

/// How long a request on any route but `/mcp` may take; for an import, how
/// long reading and checking its upload may take (the import itself has no
/// deadline, so a committed import is never answered with a timeout).
/// `/mcp`'s `GET` is a long-lived event stream, which rmcp's keep-alive and
/// the session idle timeout bound instead.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

/// The timeout layer for `/healthz` and the export route:
/// [`REQUEST_TIMEOUT`], then 503. Not 408, which says the *client* was slow
/// to send its request (and which some clients and proxies retry on their
/// own).
///
/// # Returns
/// The layer, for `Router::layer`.
pub fn timeout_layer() -> TimeoutLayer {
    TimeoutLayer::with_status_code(StatusCode::SERVICE_UNAVAILABLE, REQUEST_TIMEOUT)
}

/// How long `/healthz` waits for the database (a pool connection included)
/// before it answers "unreachable": well inside a probe's own timeout, and a
/// bound on how long an anonymous caller can hold a pool slot.
pub const HEALTH_TIMEOUT: Duration = Duration::from_secs(2);

/// An [`OkfError`] as an HTTP response: a status and the error's JSON.
// A newtype (a one-field tuple struct) so this crate can implement axum's
// `IntoResponse` for it: Rust only lets a crate implement a foreign trait
// (`IntoResponse`) for a type it defines itself.
#[derive(Debug)]
pub struct AppError(pub OkfError);

// `From` lets `?` turn an `OkfError` into an `AppError` in handlers.
impl From<OkfError> for AppError {
    fn from(error: OkfError) -> Self {
        Self(error)
    }
}

impl AppError {
    /// The HTTP status of the wrapped error.
    ///
    /// # Returns
    /// 404 for a path, catalog or changeset that is not there (or not reachable), 401
    /// for a request that proves no identity, 403 for an access level that is
    /// not enough, 409 for a refused request or a publish conflict, 410 for a
    /// used or expired link, 422 for a publish whose result fails lint or a
    /// self-service request past a limit, 400 for a bad input, 500 for the
    /// server's own failures, 503 for a request the database aborted (retry
    /// it).
    pub fn status(&self) -> StatusCode {
        // No `_` arm: a new variant fails to compile here until it has a status.
        match &self.0 {
            OkfError::NotFound { .. }
            | OkfError::CatalogNotSelected { .. }
            | OkfError::UnknownChangeset { .. }
            | OkfError::UnknownName { .. } => StatusCode::NOT_FOUND,
            OkfError::Forbidden { .. } => StatusCode::FORBIDDEN,
            OkfError::Unauthenticated { .. } => StatusCode::UNAUTHORIZED,
            OkfError::Gone { .. } => StatusCode::GONE,
            OkfError::Refused { .. } | OkfError::Conflict { .. } | OkfError::StillLinked { .. } => {
                StatusCode::CONFLICT
            }
            OkfError::LintFailed { .. } | OkfError::LimitExceeded { .. } => {
                StatusCode::UNPROCESSABLE_ENTITY
            }
            OkfError::Busy { .. } => StatusCode::SERVICE_UNAVAILABLE,
            OkfError::InvalidPath { .. }
            | OkfError::FrontmatterParse { .. }
            | OkfError::SecretDetected { .. }
            | OkfError::Shape { .. }
            | OkfError::EditMismatch { .. }
            | OkfError::ImportRefused { .. } => StatusCode::BAD_REQUEST,
            OkfError::Io { .. }
            | OkfError::Database { .. }
            | OkfError::Migrate { .. }
            | OkfError::Config { .. } => StatusCode::INTERNAL_SERVER_ERROR,
        }
    }
}

impl IntoResponse for AppError {
    fn into_response(self) -> Response {
        // As for tool errors: internal ones (every 500) are logged with their
        // details; the others at debug level, by kind only (a message can
        // name a path).
        if self.0.is_internal() {
            tracing::error!(error = %self.0, "request failed");
        } else {
            tracing::debug!(kind = self.0.kind(), "request refused");
        }
        (self.status(), Json(self.0.to_json())).into_response()
    }
}

/// `/healthz`'s body.
#[derive(Debug, Serialize)]
struct Health {
    /// `"ok"` or `"unreachable"`.
    database: &'static str,
    /// How many catalogs are loaded (no names: the route needs no key).
    catalogs: usize,
    /// The last clock check (design "Time"); `null` without a `time_server`
    /// or before the first check. It never changes the status.
    clock: Option<ClockStatus>,
}

/// `GET /healthz`: 200 while the database answers within
/// [`HEALTH_TIMEOUT`], else 503, with the last clock check, which never
/// changes the status.
///
/// # Arguments
/// * `app` - the shared state (axum's `State` extractor hands it over).
///
/// # Returns
/// The status and a [`Health`] body.
async fn healthz(State(app): State<Arc<App>>) -> Response {
    let probe = sqlx::query("SELECT 1").execute(app.pool());
    // `timeout` gives `Err` if the deadline passes first (waiting for a pool
    // connection included), else the query's own `Result`; only `Ok(Ok(_))`
    // means the database answered in time.
    let reachable = matches!(tokio::time::timeout(HEALTH_TIMEOUT, probe).await, Ok(Ok(_)));
    let health = Health {
        database: if reachable { "ok" } else { "unreachable" },
        catalogs: app.catalogs().len(),
        clock: app.clock_status(),
    };
    let status = if reachable {
        StatusCode::OK
    } else {
        StatusCode::SERVICE_UNAVAILABLE
    };
    (status, Json(health)).into_response()
}

/// rmcp's streamable HTTP service over the app: one [`OkfServer`] per
/// session, the configured `Host` names, sessions closed after
/// `session_idle_timeout`, and every session ended when `shutdown` is.
///
/// # Arguments
/// * `app` - the shared state.
/// * `shutdown` - cancelling it ends every session.
///
/// # Returns
/// The service, for `Router::route_service`.
fn mcp_service(
    app: &Arc<App>,
    shutdown: &CancellationToken,
) -> StreamableHttpService<OkfServer, LocalSessionManager> {
    let mut sessions = LocalSessionManager::default();
    sessions.session_config.keep_alive = Some(app.config().session_idle_timeout);
    let config = StreamableHttpServerConfig::default()
        .with_allowed_hosts(app.config().allowed_hosts.iter().cloned())
        // A child token: cancelling `shutdown` cancels it too, and rmcp
        // cancelling its own copy never reaches `shutdown`.
        .with_cancellation_token(shutdown.child_token());
    let app = Arc::clone(app);
    // The factory runs once per session; `move` gives the closure its own
    // `Arc` (like a Python closure capturing `app`, but by explicit transfer).
    StreamableHttpService::new(
        move || Ok(OkfServer::new(Arc::clone(&app))),
        Arc::new(sessions),
        config,
    )
}

/// The whole router.
///
/// # Arguments
/// * `app` - the shared state.
/// * `shutdown` - cancelling it ends every MCP session.
///
/// # Returns
/// The router, ready for [`axum::serve()`] (which [`serve`] calls).
pub fn router(app: Arc<App>, shutdown: &CancellationToken) -> Router {
    // Layers wrap what is added before them, so a layer added later runs
    // first on the way in: `require_key` (on `keyed`) runs before
    // `bind_session` (on `mcp`), which can then read the principal.
    let mcp = Router::new()
        .route_service("/mcp", mcp_service(&app, shutdown))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&app),
            bind_session,
        ));
    // `route_layer` applies to the routes added before it, merged ones
    // included, so every keyed route goes above it.
    let keyed = Router::new()
        .merge(mcp)
        .merge(archives::routes(&app))
        .route_layer(middleware::from_fn_with_state(
            Arc::clone(&app),
            require_key,
        ));
    let health = Router::new()
        .route("/healthz", get(healthz))
        .layer(timeout_layer());
    // The OAuth routes exist only with OAuth on; without it `/oauth/*` and
    // `/.well-known/*` stay 404, as before M10. They take no bearer token.
    let open = if app.oauth().is_some() {
        Router::new()
            .merge(health)
            .merge(oauth::routes().layer(timeout_layer()))
    } else {
        Router::new().merge(health)
    };
    open.merge(ui::routes())
        // The API applies `require_principal` (a bearer credential or a
        // console session cookie) itself, inside its header layer, so even
        // its 401 carries `no-store` and `nosniff`. MCP and the archive
        // routes (`keyed`) take only a bearer credential.
        .nest("/api/v1", api::routes(&app))
        .merge(keyed)
        // One `info` line per response (status, latency) in a span naming the
        // method and URI. The default span records no headers, so a bearer key
        // never reaches the log.
        .layer(
            TraceLayer::new_for_http()
                .make_span_with(DefaultMakeSpan::new().level(Level::INFO))
                .on_response(DefaultOnResponse::new().level(Level::INFO)),
        )
        // Hands `app` to every handler that asks for `State<Arc<App>>`.
        .with_state(app)
}

/// Serves until `shutdown` is cancelled, running the poll loop and the clock
/// check beside it.
///
/// # Arguments
/// * `app` - the shared state.
/// * `listener` - a bound socket.
/// * `shutdown` - cancelled to stop: it ends the MCP sessions, the poll loop,
///   the clock check and the listener, and `serve` returns once open
///   connections close.
///
/// # Errors
/// The listener's I/O error, after the loops and the sessions are
/// stopped (`shutdown` is cancelled on every return).
pub async fn serve(
    app: Arc<App>,
    listener: TcpListener,
    shutdown: CancellationToken,
) -> std::io::Result<()> {
    let router = router(Arc::clone(&app), &shutdown);
    let poller = tokio::spawn(Arc::clone(&app).poll_until(shutdown.clone()));
    let clock_checker = tokio::spawn(Arc::clone(&app).check_clock_until(shutdown.clone()));
    let stop = shutdown.clone();
    // `async move { ... }` is a future that resolves once the token is
    // cancelled; axum then stops accepting and waits for open connections.
    // `ConnectInfo<SocketAddr>` in every request's extensions: the peer
    // address `console_session::client_ip` falls back to (plan Decision 7).
    let served = axum::serve(
        listener,
        router.into_make_service_with_connect_info::<SocketAddr>(),
    )
    .with_graceful_shutdown(async move { stop.cancelled().await })
    .await;
    // On an error too, stop what this function started (the loops and
    // rmcp's sessions) before returning; cancelling twice is harmless.
    shutdown.cancel();
    // The loops return once `shutdown` is cancelled; a panic in one is logged.
    if let Err(error) = poller.await {
        tracing::error!(%error, "the poll loop panicked");
    }
    if let Err(error) = clock_checker.await {
        tracing::error!(%error, "the clock check panicked");
    }
    served
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request;
    // `oneshot` sends one request through a router without a socket.
    use tower::ServiceExt;

    use crate::config::Config;
    use crate::error::LinkSource;
    use crate::mcp::testing::app_with;

    /// A response's body as JSON.
    async fn json_body(response: Response) -> serde_json::Value {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn app_errors_have_a_status_and_the_error_json() {
        let missing = AppError(OkfError::CatalogNotSelected {
            catalog: "nope".to_string(),
        });
        let forbidden = AppError::from(OkfError::Forbidden {
            reason: "import needs admin".to_string(),
        });
        let refused = AppError(OkfError::Refused {
            reason: "not empty".to_string(),
        });
        let bad = AppError(OkfError::ImportRefused {
            problems: vec!["x".to_string()],
        });
        let io = AppError(OkfError::Io {
            path: "a.md".to_string(),
            source: std::io::Error::other("disk on fire"),
        });
        let internal = AppError(OkfError::Database {
            source: sqlx::Error::PoolClosed,
        });
        let unknown = AppError(OkfError::UnknownChangeset {
            changeset: "made-up".to_string(),
        });

        let errors = [
            &missing, &forbidden, &refused, &bad, &io, &internal, &unknown,
        ];
        let statuses = errors.map(AppError::status);
        // A 500 is exactly an internal error: logged in full, redacted here.
        let internal_flags = errors.map(|error| error.0.is_internal());
        let response = internal.into_response();

        assert_eq!(
            statuses,
            [
                StatusCode::NOT_FOUND,
                StatusCode::FORBIDDEN,
                StatusCode::CONFLICT,
                StatusCode::BAD_REQUEST,
                StatusCode::INTERNAL_SERVER_ERROR,
                StatusCode::INTERNAL_SERVER_ERROR,
                StatusCode::NOT_FOUND,
            ]
        );
        assert_eq!(
            internal_flags,
            [false, false, false, false, true, true, false]
        );
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        // The driver's message stays in the log.
        assert_eq!(
            json_body(response).await,
            serde_json::json!({
                "error": "Database",
                "message": "internal error; the server log has the details",
            })
        );
    }

    #[test]
    fn busy_is_503_and_says_to_retry() {
        let busy = AppError(OkfError::Busy {
            reason: "deadlock".to_string(),
        });

        let status = busy.status();
        let body = busy.0.to_json();

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(body["error"], "Busy");
        assert!(body["message"].as_str().unwrap().ends_with("retry"));
    }

    #[test]
    fn an_unknown_user_or_team_is_a_404() {
        let error = AppError(OkfError::UnknownName {
            noun: "user",
            name: "alice".into(),
        });

        let status = error.status();
        let body = error.0.to_json();

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(error.0.to_string(), "user \"alice\" does not exist");
        assert_eq!(
            body,
            serde_json::json!({
                "error": "UnknownName",
                "message": "user \"alice\" does not exist",
                "noun": "user",
                "name": "alice",
            })
        );
    }

    #[test]
    fn publish_refusals_have_their_statuses() {
        let conflict = AppError(OkfError::Conflict { files: Vec::new() });
        let lint = AppError(OkfError::LintFailed {
            errors: Vec::new(),
            warnings: Vec::new(),
        });

        assert_eq!(conflict.status(), StatusCode::CONFLICT);
        assert_eq!(lint.status(), StatusCode::UNPROCESSABLE_ENTITY);
    }

    #[test]
    fn a_delete_refused_while_linked_is_a_conflict() {
        let still_linked = AppError(OkfError::StillLinked {
            path: "/x.md".to_string(),
            links: vec![LinkSource {
                source: "/hub.md".to_string(),
                line: 5,
            }],
        });

        assert_eq!(still_linked.status(), StatusCode::CONFLICT);
    }

    #[test]
    fn an_edit_mismatch_is_a_bad_request() {
        let mismatch = AppError(OkfError::EditMismatch {
            path: "a.md".to_string(),
            matches: 0,
        });

        let status = mismatch.status();

        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test]
    async fn a_refused_secret_is_a_bad_request_with_its_rule_and_line() {
        let refused = AppError(OkfError::SecretDetected {
            path: "notes.md".to_string(),
            line: 6,
            rule: "secret:assignment".to_string(),
            message: "possible secret: 'password' followed by a literal value Ab1A...".to_string(),
        });

        let status = refused.status();
        let body = json_body(refused.into_response()).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["error"], "SecretDetected");
        assert_eq!(
            (&body["line"], &body["rule"]),
            (&6.into(), &"secret:assignment".into())
        );
    }

    // `start_paused`: tokio's clock jumps ahead whenever every task is
    // waiting on a timer, so the 120-second timeout passes at once.
    #[tokio::test(start_paused = true)]
    async fn a_request_past_the_timeout_gets_503() {
        let slow = Router::new()
            .route(
                "/slow",
                get(|| async {
                    tokio::time::sleep(REQUEST_TIMEOUT * 2).await;
                    "too late"
                }),
            )
            .layer(timeout_layer());

        let request = Request::get("/slow").body(Body::empty()).expect("request");
        let response = slow.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn healthz_gives_up_after_its_own_deadline() {
        let test = app_with(&[("pg", &[])], Config::default()).await;
        let router = router(Arc::clone(&test.app), &CancellationToken::new());
        // Hold every pool connection, so the probe waits for one (sqlx's own
        // acquire timeout is 30 s).
        let size = test.app.pool().options().get_max_connections();
        let mut held = Vec::new();
        for _ in 0..size {
            held.push(test.app.pool().acquire().await.unwrap());
        }

        let started = std::time::Instant::now();
        let request = Request::get("/healthz")
            .body(Body::empty())
            .expect("request");
        let response = router.oneshot(request).await.unwrap();
        let waited = started.elapsed();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert!(
            waited >= HEALTH_TIMEOUT,
            "answered before the deadline: {waited:?}"
        );
        assert!(
            waited < HEALTH_TIMEOUT * 5,
            "waited past the deadline: {waited:?}"
        );
        drop(held);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn healthz_reports_an_unreachable_database_as_503() {
        let test = app_with(&[("pg", &[])], Config::default()).await;
        let router = router(Arc::clone(&test.app), &CancellationToken::new());
        test.app.pool().close().await;

        let request = Request::get("/healthz")
            .body(Body::empty())
            .expect("request");
        let response = router.oneshot(request).await.unwrap();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(
            json_body(response).await,
            serde_json::json!({"database": "unreachable", "catalogs": 1, "clock": null})
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn healthz_reports_the_last_clock_check_without_changing_its_status() {
        let test = app_with(&[("pg", &[])], Config::default()).await;
        let get = || {
            Request::get("/healthz")
                .body(Body::empty())
                .expect("request")
        };
        let before = router(Arc::clone(&test.app), &CancellationToken::new())
            .oneshot(get())
            .await
            .unwrap();
        let before = json_body(before).await;
        test.app.set_clock_status(ClockStatus {
            ok: false,
            checked_at: "2026-11-03T12:00:00Z".to_string(),
            offset_ms: None,
            stratum: None,
        });

        let after = router(Arc::clone(&test.app), &CancellationToken::new())
            .oneshot(get())
            .await
            .unwrap();
        let status = after.status();
        let after = json_body(after).await;

        assert_eq!(before["clock"], serde_json::Value::Null);
        assert_eq!(status, StatusCode::OK);
        assert_eq!(after["database"], "ok");
        assert_eq!(
            after["clock"],
            serde_json::json!({"ok": false, "checked_at": "2026-11-03T12:00:00Z"})
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn oauth_routes_exist_only_with_oauth_on() {
        let off = app_with(&[("main", &[])], Config::default()).await;
        let on = app_with(
            &[("main", &[])],
            crate::store::oauth::testing::oauth_config("https://okf.example.com"),
        )
        .await;
        let get = |test: &crate::mcp::testing::TestApp| {
            router(Arc::clone(&test.app), &CancellationToken::new()).oneshot(
                Request::get("/mcp")
                    .header("host", "okf.example.com")
                    .body(Body::empty())
                    .expect("request"),
            )
        };

        let refused_off = get(&off).await.unwrap();
        let refused_on = get(&on).await.unwrap();

        assert_eq!(refused_off.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(refused_off.headers()["www-authenticate"], "Bearer");
        assert_eq!(refused_on.status(), StatusCode::UNAUTHORIZED);
        assert_eq!(
            refused_on.headers()["www-authenticate"],
            "Bearer resource_metadata=\"https://okf.example.com/.well-known/oauth-protected-resource\""
        );
    }

    #[test]
    fn sign_in_errors_have_their_statuses() {
        let statuses = [
            AppError(OkfError::Unauthenticated {
                reason: "no".into(),
            })
            .status(),
            AppError(OkfError::Gone {
                reason: "expired".into(),
            })
            .status(),
            AppError(OkfError::LimitExceeded {
                limit: "ceiling",
                reason: "above".into(),
            })
            .status(),
        ];

        assert_eq!(
            statuses,
            [
                StatusCode::UNAUTHORIZED,
                StatusCode::GONE,
                StatusCode::UNPROCESSABLE_ENTITY,
            ]
        );
    }
}

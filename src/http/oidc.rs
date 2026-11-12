//! OIDC sign-in and account linking (spec "OIDC", plan Decision 12).
//!
//! A sign-in starts at `GET /auth/oidc/{provider}/start` (the sign-in
//! page's button), which keeps the flow's `state`, `nonce` and PKCE
//! verifier on the server under a random id and puts that id in a
//! ten-minute `SameSite=Lax` cookie: the flow is bound to the browser that
//! started it, and the callback takes it out (single use). The callback
//! checks `state` itself (the crate does not), exchanges the code and
//! verifies the ID token, then finds the user by `(provider, sub)`, else by
//! a verified email (linking it), else refuses.
//!
//! Linking from the account page starts with `POST /api/v1/me/identities/
//! {provider}/link` (a console session and its CSRF header), which records
//! the session in the flow; the callback links only for a browser still
//! holding that same live session, so neither another site nor another
//! person's callback can link an identity to someone's account.
//!
//! No secret reaches a log line: the callback's query has no `Debug`, the
//! request span records the path only (`http::router`), and the flow's
//! secrets redact themselves.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::{Path, Query, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;
use subtle::ConstantTimeEq;

use crate::app::App;
use crate::auth::{CredentialKind, Principal};
use crate::error::OkfError;
use crate::http::console_session::{
    append_all, client_ip, cookie, secure_cookies, session_cookie, session_user, set_cookie,
    ConsoleSession, SameSite, COOKIE,
};
use crate::http::limits::ip_key;
use crate::http::pages::safe_return_to;
use crate::http::signin::{audit, message_page, notify_method_added, ProviderButton};
use crate::http::{timeout_layer, AppError};
use crate::keys;
use crate::oidc::{Flow, Identity, LinkRequest, OidcError, OidcProviders, FLOW_TTL};
use crate::store::identities::{self, OidcOutcome, OidcSignIn, SessionLink};
use crate::store::sign_in_events::{Method, Outcome};
use crate::store::user_sessions;
use crate::store::users::UserAuth;

/// The flow cookie: `SameSite=Lax`, so the provider's redirect carries it.
pub const FLOW_COOKIE: &str = "okf_oidc";
/// No user for this identity or verified email (403, spec).
pub const NO_ACCOUNT: &str = "No account for this email. Ask an admin to invite you.";
/// A bad state, nonce or token, or a replayed callback (400, spec).
pub const SIGN_IN_RETRY: &str = "Sign-in failed; try again";
/// A link callback without the session that started it (400).
pub const LINK_RETRY: &str = "Linking failed: sign in again and start it from your account page.";
/// Discovery failed with nothing cached (503).
const UNAVAILABLE: &str = "The sign-in provider cannot be reached right now; try again later.";
/// The identity cannot be linked (409): another user holds it, or the user
/// already links another account at this provider.
pub const LINKED_ELSEWHERE: &str = "This account cannot be linked: it is linked to another user, \
     or another account at this provider is already linked to yours.";
/// Over the per-address sign-in limit (429).
const TOO_MANY: &str = "Too many attempts from this address. Wait a few minutes and try again.";
/// Over the per-user link limit (429).
const TOO_MANY_LINKS: &str = "Too many links started; wait a few minutes and try again.";
/// Where linking returns to.
const ACCOUNT_PAGE: &str = "/ui/#/account";
/// The pages' title.
const TITLE: &str = "Sign in";

/// The open routes, merged only when providers are configured.
///
/// # Returns
/// The router, with the request timeout.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/auth/oidc/{provider}/start", get(start))
        .route("/auth/oidc/{provider}/callback", get(callback))
        .layer(timeout_layer())
}

/// The linking route, merged into `/api/v1` (behind `require_principal`,
/// which checks the CSRF header of a cookie write).
///
/// # Returns
/// The router.
pub(crate) fn api_routes() -> Router<Arc<App>> {
    // `{provider}`: Task 10b's `/me/identities/{provider}` must use the same
    // parameter name at this position, or the router refuses to build.
    Router::new().route("/me/identities/{provider}/link", post(link))
}

/// The sign-in page's provider buttons.
///
/// # Arguments
/// * `app` - the shared state.
///
/// # Returns
/// One button per configured provider, by id.
pub(crate) fn buttons(app: &App) -> Vec<ProviderButton> {
    app.oidc().map_or_else(Vec::new, |oidc| {
        oidc.labels()
            .map(|(id, label)| ProviderButton {
                id: id.to_string(),
                label: label.to_string(),
            })
            .collect()
    })
}

/// The `Set-Cookie` header that clears the flow cookie.
fn clear_flow(app: &App) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::SET_COOKIE,
        set_cookie(
            FLOW_COOKIE,
            "",
            std::time::Duration::ZERO,
            SameSite::Lax,
            secure_cookies(app),
        ),
    );
    headers
}

/// A flow to start.
struct NewFlow<'a> {
    /// The provider's id.
    provider: &'a str,
    /// Where to go afterwards.
    return_to: String,
    /// Set for a link from the account page.
    link: Option<LinkRequest>,
}

/// Starts a flow: the provider's authorize URL and the flow cookie. The
/// flow this browser held before is dropped, and a link replaces the one
/// its session started before ([`OidcProviders::store_flow`]).
///
/// # Errors
/// [`OkfError::NotFound`] for an unknown provider; [`OkfError::Busy`] when
/// discovery fails with nothing cached or too many flows are in progress;
/// [`OkfError::Io`] from the random source.
async fn begin(
    app: &App,
    oidc: &OidcProviders,
    headers: &HeaderMap,
    new: NewFlow<'_>,
) -> Result<(String, HeaderMap), OkfError> {
    let (url, secrets) = oidc.authorize_url(new.provider, Instant::now()).await?;
    let id = keys::generate_with("")?;
    let flow = Flow {
        provider: new.provider.to_string(),
        secrets,
        return_to: new.return_to,
        link: new.link,
    };
    let previous = cookie(headers, FLOW_COOKIE);
    if !oidc.store_flow(&id, flow, previous, Instant::now()) {
        return Err(OkfError::Busy {
            reason: "too many sign-ins in progress; try again shortly".to_string(),
        });
    }
    let mut headers = HeaderMap::new();
    headers.insert(
        header::SET_COOKIE,
        set_cookie(
            FLOW_COOKIE,
            &id,
            FLOW_TTL,
            SameSite::Lax,
            secure_cookies(app),
        ),
    );
    Ok((url, headers))
}

/// `GET /auth/oidc/{provider}/start`'s query.
#[derive(Debug, Default, Deserialize)]
struct StartQuery {
    return_to: Option<String>,
}

/// `GET /auth/oidc/{provider}/start`: 303 to the provider. Each start
/// takes from the per-address sign-in limit, so no one address can fill
/// the flow map.
async fn start(
    State(app): State<Arc<App>>,
    Path(provider): Path<String>,
    parts: Parts,
    Query(query): Query<StartQuery>,
) -> Response {
    let Some(oidc) = app.oidc() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    if oidc.label(&provider).is_none() {
        return message_page(StatusCode::NOT_FOUND, TITLE, "No such sign-in provider.");
    }
    let ip = client_ip(&parts, &app.config().auth);
    if !app
        .sign_in_limits()
        .sign_in_per_ip
        .try_take(&ip_key(ip), Instant::now())
    {
        return message_page(StatusCode::TOO_MANY_REQUESTS, TITLE, TOO_MANY);
    }
    let return_to = safe_return_to(query.return_to.as_deref());
    let new = NewFlow {
        provider: &provider,
        return_to,
        link: None,
    };
    match begin(&app, oidc, &parts.headers, new).await {
        Ok((url, headers)) => (headers, Redirect::to(&url)).into_response(),
        Err(OkfError::NotFound { .. }) => {
            message_page(StatusCode::NOT_FOUND, TITLE, "No such sign-in provider.")
        }
        Err(OkfError::Busy { .. }) => {
            message_page(StatusCode::SERVICE_UNAVAILABLE, TITLE, UNAVAILABLE)
        }
        Err(error) => AppError(error).into_response(),
    }
}

/// The provider's redirect back. No `Debug`: it holds the code.
#[derive(Deserialize)]
struct CallbackQuery {
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    state: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// How a callback ends.
enum Done {
    /// Go to `to`, with the session cookie (empty when linking).
    Redirect {
        session_cookie: HeaderMap,
        to: String,
    },
    /// A page with this status and message.
    Refused(StatusCode, &'static str),
}

/// A message page that also clears the flow cookie.
fn page(headers: &HeaderMap, status: StatusCode, message: &str) -> Response {
    let mut response = message_page(status, TITLE, message);
    append_all(response.headers_mut(), headers);
    response
}

/// `GET /auth/oidc/{provider}/callback`.
async fn callback(
    State(app): State<Arc<App>>,
    Path(provider): Path<String>,
    parts: Parts,
    Query(query): Query<CallbackQuery>,
) -> Response {
    let Some(oidc) = app.oidc() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // Every answer clears the flow cookie: the flow is single use.
    let mut headers = clear_flow(&app);
    let flow =
        cookie(&parts.headers, FLOW_COOKIE).and_then(|id| oidc.flows.take(id, Instant::now()));
    let Some(Flow {
        provider: started_with,
        secrets,
        return_to,
        link,
    }) = flow
    else {
        return page(&headers, StatusCode::BAD_REQUEST, SIGN_IN_RETRY);
    };
    // The `state` check is this redirect's CSRF guard; openidconnect leaves
    // it to the app. Constant time, like every other secret comparison.
    let state_ok = started_with == provider
        && query
            .state
            .as_deref()
            .is_some_and(|state| bool::from(state.as_bytes().ct_eq(secrets.state.as_bytes())));
    // A tuple pattern: the state matched, the provider sent no error, and a code.
    let (true, None, Some(code)) = (state_ok, query.error.as_deref(), query.code) else {
        return page(&headers, StatusCode::BAD_REQUEST, SIGN_IN_RETRY);
    };
    let ip = client_ip(&parts, &app.config().auth);
    let identity = match oidc
        .identify(&provider, code, secrets, Instant::now())
        .await
    {
        Ok(identity) => identity,
        Err(OidcError::Discovery { .. }) => {
            return page(&headers, StatusCode::SERVICE_UNAVAILABLE, UNAVAILABLE)
        }
        Err(error) => {
            tracing::info!(provider = %provider, %error, "OIDC sign-in refused");
            audit(&app, None, Method::Oidc, Outcome::Failed, ip).await;
            return page(&headers, StatusCode::BAD_REQUEST, SIGN_IN_RETRY);
        }
    };
    let done = match link {
        Some(link) => finish_link(&app, &parts, &provider, &identity, link).await,
        None => finish_sign_in(&app, &parts, &provider, &identity, return_to).await,
    };
    match done {
        Ok(Done::Redirect { session_cookie, to }) => {
            append_all(&mut headers, &session_cookie);
            (headers, Redirect::to(&to)).into_response()
        }
        Ok(Done::Refused(status, message)) => page(&headers, status, message),
        Err(error) => {
            let mut response = AppError(error).into_response();
            append_all(response.headers_mut(), &headers);
            response
        }
    }
}

/// What to call a provider in the method-added notice.
fn method_text(app: &App, provider: &str) -> String {
    let label = app
        .oidc()
        .and_then(|oidc| oidc.label(provider))
        .unwrap_or(provider);
    format!("your {label} account")
}

/// The identity's email, trimmed, when it may be used to find an account:
/// sent, and verified by the provider.
fn verified_email(identity: &Identity) -> Option<&str> {
    identity
        .email
        .as_deref()
        .map(str::trim)
        .filter(|_| identity.email_verified)
}

/// Queues the "a sign-in method was added" notice on a task of its own,
/// after the commit: it is best effort (`notify_method_added` logs a
/// failure), so a slow or failing queue can never hold back or fail the
/// response of a committed sign-in or link.
fn notify_detached(app: &Arc<App>, user: UserAuth, provider: &str) {
    let app = Arc::clone(app);
    let method = method_text(&app, provider);
    // `tokio::spawn` runs the future on its own task, like
    // `asyncio.create_task`; `move` hands it owned copies.
    tokio::spawn(async move { notify_method_added(&app, &user, &method).await });
}

/// Signs in by `(provider, sub)`, else links by verified email and signs
/// in, else refuses with 403 (spec, in that order), in one transaction
/// (`identities::sign_in`) that also ends the browser's old session and
/// records the event. An identity already linked always signs in as its
/// own user, whatever email it now carries, so a changed email at the
/// provider never reaches another account. After the commit only the
/// cookie is built and the notice detached: nothing can block or fail.
async fn finish_sign_in(
    app: &Arc<App>,
    parts: &Parts,
    provider: &str,
    identity: &Identity,
    return_to: String,
) -> Result<Done, OkfError> {
    let ip = client_ip(parts, &app.config().auth);
    let absolute = app.settings().console_session_absolute;
    let user_agent = parts
        .headers
        .get(header::USER_AGENT)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let request = OidcSignIn {
        provider,
        subject: &identity.subject,
        // Only a verified email may claim an account.
        verified_email: verified_email(identity),
        old_session: cookie(&parts.headers, COOKIE),
        ip,
        user_agent,
        absolute,
    };
    let outcome = identities::sign_in(app.pool(), &request, app.clock().now()).await;
    let (user, session, linked) = match outcome {
        Ok(OidcOutcome::SignedIn {
            user,
            session,
            linked,
        }) => (user, session, linked),
        // Nothing was committed on these paths, so the audit may wait.
        Ok(OidcOutcome::NoAccount) => {
            audit(app, None, Method::Oidc, Outcome::Failed, ip).await;
            return Ok(Done::Refused(StatusCode::FORBIDDEN, NO_ACCOUNT));
        }
        Ok(OidcOutcome::Unlinked) | Err(OkfError::Unauthenticated { .. }) => {
            audit(app, None, Method::Oidc, Outcome::Failed, ip).await;
            return Ok(Done::Refused(StatusCode::BAD_REQUEST, SIGN_IN_RETRY));
        }
        Err(OkfError::Refused { .. }) => {
            return Ok(Done::Refused(StatusCode::CONFLICT, LINKED_ELSEWHERE))
        }
        Err(error) => return Err(error),
    };
    let session_cookie = session_cookie(app, &user.id, &session, absolute);
    if linked {
        tracing::info!(user = %user.id, provider, "OIDC identity linked by verified email");
        notify_detached(app, user, provider);
    }
    Ok(Done::Redirect {
        session_cookie,
        to: return_to,
    })
}

/// The user of this browser's console session, when it is the live session
/// that started the link (same id, compared in constant time, and the same
/// user).
async fn linking_user(
    app: &App,
    parts: &Parts,
    link: &LinkRequest,
) -> Result<Option<UserAuth>, OkfError> {
    let Some(id) = cookie(&parts.headers, COOKIE) else {
        return Ok(None);
    };
    if !bool::from(id.as_bytes().ct_eq(link.session_id.as_bytes())) {
        return Ok(None);
    }
    let idle = app.settings().console_session_idle;
    match user_sessions::touch(app.pool(), id, idle, app.clock().now()).await? {
        Some(row) if row.user_id == link.user_id => session_user(app, &row.user_id).await,
        _ => Ok(None),
    }
}

/// Links the identity to the signed-in user who started the flow, whatever
/// its email (spec), and ends their other sessions unless kept, in one
/// transaction. Only the browser still holding the session that started
/// the link may finish it. After the commit only the notice runs, detached.
async fn finish_link(
    app: &Arc<App>,
    parts: &Parts,
    provider: &str,
    identity: &Identity,
    link: LinkRequest,
) -> Result<Done, OkfError> {
    let Some(user) = linking_user(app, parts, &link).await? else {
        return Ok(Done::Refused(StatusCode::BAD_REQUEST, LINK_RETRY));
    };
    let email = identity.email.as_deref().map(str::trim).unwrap_or_default();
    let request = SessionLink {
        user_id: &link.user_id,
        session_id: &link.session_id,
        provider,
        subject: &identity.subject,
        email,
        keep_sessions: link.keep_sessions,
    };
    let ended = match identities::link_for_session(app.pool(), &request, app.clock().now()).await {
        Ok(ended) => ended,
        Err(OkfError::Refused { .. }) => {
            return Ok(Done::Refused(StatusCode::CONFLICT, LINKED_ELSEWHERE))
        }
        // The session ended between the check above and the link.
        Err(OkfError::Unauthenticated { .. }) => {
            return Ok(Done::Refused(StatusCode::BAD_REQUEST, LINK_RETRY))
        }
        Err(error) => return Err(error),
    };
    tracing::info!(user = %link.user_id, provider, ended, "OIDC identity linked from the account page");
    // After the commit: nothing that can block or fail.
    notify_detached(app, user, provider);
    Ok(Done::Redirect {
        session_cookie: HeaderMap::new(),
        to: ACCOUNT_PAGE.to_string(),
    })
}

/// `POST /me/identities/{provider}/link`'s body.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct LinkBody {
    /// Keep the user's other sessions (the account page's checkbox).
    #[serde(default)]
    keep_sessions: bool,
}

/// `POST /api/v1/me/identities/{provider}/link`: `{"url": <authorize URL>}`
/// and the flow cookie. The console navigates to the URL itself, so the
/// console CSP's `form-action 'self'` never meets the provider's origin.
///
/// # Errors
/// 403 for a key or grant; 404 for an unknown provider or with OIDC off;
/// 429 once the user's allowance is spent (a burst of
/// [`crate::oidc::LINK_STARTS_PER_USER`], refilled one per minute);
/// 503 when the provider cannot be reached.
async fn link(
    State(app): State<Arc<App>>,
    Path(provider): Path<String>,
    Extension(principal): Extension<Arc<Principal>>,
    session: Option<Extension<ConsoleSession>>,
    headers: HeaderMap,
    Json(body): Json<LinkBody>,
) -> Result<Response, AppError> {
    let (user_id, session) = match (&principal.kind, session) {
        (CredentialKind::Session { user_id }, Some(Extension(session))) => {
            (user_id.clone(), session)
        }
        _ => {
            return Err(OkfError::Forbidden {
                reason: "linking an account needs a console session".to_string(),
            }
            .into())
        }
    };
    let oidc = app.oidc().ok_or_else(|| OkfError::NotFound {
        path: format!("me/identities/{provider}/link"),
    })?;
    if oidc.label(&provider).is_none() {
        return Err(OkfError::NotFound {
            path: format!("me/identities/{provider}/link"),
        }
        .into());
    }
    // A signed-in user is not limited by address, so their own allowance
    // keeps them from filling the flow map every sign-in shares.
    if !oidc.may_start_link(&user_id, Instant::now()) {
        let body = serde_json::json!({
            "error": "TooManyRequests",
            "message": TOO_MANY_LINKS,
        });
        return Ok((StatusCode::TOO_MANY_REQUESTS, Json(body)).into_response());
    }
    let new = NewFlow {
        provider: &provider,
        return_to: ACCOUNT_PAGE.to_string(),
        link: Some(LinkRequest {
            user_id,
            session_id: session.id,
            keep_sessions: body.keep_sessions,
        }),
    };
    let (url, flow_cookie) = begin(&app, oidc, &headers, new).await?;
    Ok((flow_cookie, Json(serde_json::json!({ "url": url }))).into_response())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::time::Duration;

    use axum::http::StatusCode;
    use serde_json::json;
    use sqlx::PgPool;

    use super::{FLOW_COOKIE, LINKED_ELSEWHERE, LINK_RETRY, NO_ACCOUNT, SIGN_IN_RETRY};
    use crate::config::{AuthConfig, Config};
    use crate::http::api::testing::{
        keys, login_config, user_with_password, Browser, Payload, Reply, MAIN, PASSWORD,
    };
    use crate::http::console_session::COOKIE;
    use crate::http::limits::SIGN_IN_PER_IP;
    use crate::mcp::testing::{app_with, TestApp};
    use crate::oidc::testing::{self as idp, MockIdp, CLIENT_ID, SECRET};
    use crate::oidc::LINK_STARTS_PER_USER;
    use crate::store::identities;
    use crate::store::sign_in_events::{self, Method, Outcome};
    use crate::store::{passwords, users};

    /// An app with the mock provider as `example`. The client secret comes
    /// from the environment, as in production; each test names its own
    /// variable, so tests running at once never share one.
    async fn oidc_app(idp: &MockIdp, secret_env: &str) -> TestApp {
        // Edition 2021: `set_var` is a safe call.
        std::env::set_var(secret_env, SECRET);
        let config = Config {
            auth: AuthConfig {
                oidc: vec![idp::provider(idp, secret_env)],
                ..AuthConfig::default()
            },
            ..login_config()
        };
        app_with(
            &[("main", MAIN), ("other", &[("index.md", "# Other\n")])],
            config,
        )
        .await
    }

    fn query_of(url: &str) -> HashMap<String, String> {
        url::Url::parse(url)
            .expect("an absolute URL")
            .query_pairs()
            .into_owned()
            .collect()
    }

    /// Starts a sign-in and returns the authorize URL's query. The mock is
    /// told the nonce, as its authorize page would learn it.
    async fn start(
        test: &TestApp,
        idp: &MockIdp,
        browser: &mut Browser,
    ) -> HashMap<String, String> {
        let reply = browser
            .send(
                test,
                "GET",
                "/auth/oidc/example/start?return_to=%2Fui%2F%23%2Fkeys",
                Payload::Empty,
            )
            .await;
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.text());
        let cookie = reply.set_cookie(FLOW_COOKIE).expect("a flow cookie");
        assert!(
            cookie.contains("SameSite=Lax") && cookie.contains("Max-Age=600"),
            "{cookie}"
        );
        let query = query_of(reply.location());
        idp.set_nonce(&query["nonce"]);
        query
    }

    async fn callback(test: &TestApp, browser: &mut Browser, state: &str) -> Reply {
        let uri = format!("/auth/oidc/example/callback?code=c&state={state}");
        browser.send(test, "GET", &uri, Payload::Empty).await
    }

    /// Starts linking from `browser`'s session and returns the authorize
    /// URL's query.
    async fn start_link(
        test: &TestApp,
        idp: &MockIdp,
        browser: &mut Browser,
        body: serde_json::Value,
    ) -> HashMap<String, String> {
        let started = browser
            .send(
                test,
                "POST",
                "/api/v1/me/identities/example/link",
                Payload::Json(body),
            )
            .await;
        assert_eq!(started.status, StatusCode::OK, "{}", started.text());
        let query = query_of(started.json()["url"].as_str().expect("a URL"));
        idp.set_nonce(&query["nonce"]);
        query
    }

    async fn queued_mail(test: &TestApp) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM apalis.jobs")
            .fetch_one(test.app.pool())
            .await
            .expect("count")
    }

    async fn user_id(test: &TestApp) -> String {
        sqlx::query_scalar("SELECT id FROM users WHERE name = 'alice'")
            .fetch_one(test.app.pool())
            .await
            .expect("alice's id")
    }

    /// Waits (up to 10 s) until `want` mails are queued: the method-added
    /// notice is queued on a task of its own, after the response.
    async fn wait_for_mail(test: &TestApp, want: i64) {
        for _ in 0..200 {
            if queued_mail(test).await == want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("{want} mails were never queued");
    }

    async fn sessions_of(pool: &PgPool, user_id: &str) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM user_sessions WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(pool)
            .await
            .expect("count the sessions")
    }

    /// Waits (up to 10 s) until `want` backends of this database wait on a
    /// lock.
    async fn wait_for_lock_waiters(pool: &PgPool, want: i64) {
        for _ in 0..200 {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity \
                  WHERE datname = current_database() AND wait_event_type = 'Lock'",
            )
            .fetch_one(pool)
            .await
            .expect("count the lock waiters");
            if waiting >= want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("{want} backends never waited on a lock");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_start_redirect_carries_pkce_state_nonce_and_the_redirect_uri() {
        let idp = idp::start().await;
        let test = oidc_app(&idp, "OKF_TEST_OIDC_SECRET_START").await;

        let query = start(&test, &idp, &mut Browser::default()).await;

        assert_eq!(query["client_id"], CLIENT_ID);
        assert_eq!(query["response_type"], "code");
        assert_eq!(query["code_challenge_method"], "S256");
        assert_eq!(
            query["redirect_uri"],
            "https://okf.example.com/auth/oidc/example/callback"
        );
        assert_eq!(query["scope"], "openid email");
        assert!(query["state"].len() >= 22 && query["nonce"].len() >= 22);
        assert!(query["code_challenge"].len() >= 43);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_verified_email_links_the_identity_and_signs_in() {
        let idp = idp::start().await;
        let test = oidc_app(&idp, "OKF_TEST_OIDC_SECRET_LINK").await;
        let alice = user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        idp.will_issue("sub-1", Some("Alice@Example.com"), Some(true));
        let mut browser = Browser::default();

        let query = start(&test, &idp, &mut browser).await;
        let reply = callback(&test, &mut browser, &query["state"]).await;

        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.text());
        assert_eq!(reply.location(), "/ui/#/keys");
        assert!(browser.cookie(COOKIE).is_some());
        assert_eq!(browser.cookie(FLOW_COOKIE), None);
        let linked = identities::list(test.app.pool(), &alice.id)
            .await
            .expect("list");
        assert_eq!(linked.len(), 1);
        assert_eq!(linked[0].email_at_link, "Alice@Example.com");
        assert!(linked[0].last_used_at.is_some());
        wait_for_mail(&test, 1).await;
        let events = sign_in_events::recent(test.app.pool(), "alice", 5)
            .await
            .expect("events");
        assert!(matches!(
            (&events[0].method, &events[0].outcome),
            (Method::Oidc, Outcome::Ok)
        ));
        let session = browser
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;
        assert_eq!(session.json()["user"], "alice");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unverified_or_unknown_email_is_refused_with_403() {
        let idp = idp::start().await;
        let test = oidc_app(&idp, "OKF_TEST_OIDC_SECRET_REFUSE").await;
        let alice = user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;

        for (subject, email, verified) in [
            ("sub-2", Some("alice@example.com"), Some(false)),
            ("sub-3", Some("alice@example.com"), None),
            ("sub-4", Some("nobody@example.com"), Some(true)),
            ("sub-5", None, Some(true)),
            ("sub-6", Some("alice\0@example.com"), Some(true)),
        ] {
            idp.will_issue(subject, email, verified);
            let mut browser = Browser::default();
            let query = start(&test, &idp, &mut browser).await;
            let reply = callback(&test, &mut browser, &query["state"]).await;
            assert_eq!(reply.status, StatusCode::FORBIDDEN, "{subject}");
            assert!(reply.text().contains(NO_ACCOUNT), "{subject}");
            assert_eq!(browser.cookie(COOKIE), None);
        }
        assert!(identities::list(test.app.pool(), &alice.id)
            .await
            .expect("list")
            .is_empty());
        let events = sign_in_events::recent(test.app.pool(), "alice", 5)
            .await
            .expect("events");
        assert!(events.is_empty(), "refusals name no user");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_returning_subject_signs_in_after_its_email_changed() {
        let idp = idp::start().await;
        let test = oidc_app(&idp, "OKF_TEST_OIDC_SECRET_RETURN").await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let bob = user_with_password(&test, "bob", "bob@example.com", Some(PASSWORD)).await;
        idp.will_issue("sub-1", Some("alice@example.com"), Some(true));
        let mut first = Browser::default();
        let query = start(&test, &idp, &mut first).await;
        callback(&test, &mut first, &query["state"]).await;

        for email in ["alice@elsewhere.example", "bob@example.com"] {
            // Now verified as bob's address: `(provider, sub)` still wins.
            idp.will_issue("sub-1", Some(email), Some(email.starts_with("bob")));
            let mut browser = Browser::default();
            let query = start(&test, &idp, &mut browser).await;
            let reply = callback(&test, &mut browser, &query["state"]).await;

            assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.text());
            let session = browser
                .send(&test, "GET", "/api/v1/session", Payload::Empty)
                .await;
            assert_eq!(session.json()["user"], "alice", "{email}");
        }
        assert!(identities::list(test.app.pool(), &bob.id)
            .await
            .expect("list")
            .is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_wrong_state_a_wrong_nonce_a_provider_error_or_a_replay_is_400() {
        let idp = idp::start().await;
        let test = oidc_app(&idp, "OKF_TEST_OIDC_SECRET_BAD").await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        idp.will_issue("sub-1", Some("alice@example.com"), Some(true));

        let mut wrong_state = Browser::default();
        start(&test, &idp, &mut wrong_state).await;
        let wrong_state = callback(&test, &mut wrong_state, "not-the-state").await;

        let mut wrong_nonce = Browser::default();
        let query = start(&test, &idp, &mut wrong_nonce).await;
        idp.set_nonce("not-the-nonce");
        let wrong_nonce = callback(&test, &mut wrong_nonce, &query["state"]).await;

        let mut denied = Browser::default();
        let query = start(&test, &idp, &mut denied).await;
        let uri = format!(
            "/auth/oidc/example/callback?error=access_denied&state={}",
            query["state"]
        );
        let denied = denied.send(&test, "GET", &uri, Payload::Empty).await;

        // Another browser presenting a valid state, without the flow cookie.
        let mut started = Browser::default();
        let query = start(&test, &idp, &mut started).await;
        let elsewhere = callback(&test, &mut Browser::default(), &query["state"]).await;

        let mut replayed = Browser::default();
        let query = start(&test, &idp, &mut replayed).await;
        let flow = replayed.cookie(FLOW_COOKIE).expect("a flow").to_string();
        let first = callback(&test, &mut replayed, &query["state"]).await;
        let again = callback(
            &test,
            &mut Browser::with_cookie(FLOW_COOKIE, &flow),
            &query["state"],
        )
        .await;

        for (what, reply) in [
            ("state", &wrong_state),
            ("nonce", &wrong_nonce),
            ("error", &denied),
            ("another browser", &elsewhere),
            ("replay", &again),
        ] {
            assert_eq!(
                reply.status,
                StatusCode::BAD_REQUEST,
                "{what}: {}",
                reply.text()
            );
            assert!(reply.text().contains(SIGN_IN_RETRY), "{what}");
            assert!(reply.set_cookie(COOKIE).is_none(), "{what}");
        }
        assert_eq!(first.status, StatusCode::SEE_OTHER);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_token_from_another_issuer_for_another_client_or_expired_is_400() {
        let idp = idp::start().await;
        let test = oidc_app(&idp, "OKF_TEST_OIDC_SECRET_CLAIMS").await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        idp.will_issue("sub-1", Some("alice@example.com"), Some(true));

        for (what, issuer, audience, expired) in [
            ("issuer", Some("https://id.example.com"), None, false),
            ("audience", None, Some("another-client"), false),
            ("expiry", None, None, true),
        ] {
            idp.will_sign_badly(issuer, audience, expired);
            let mut browser = Browser::default();
            let query = start(&test, &idp, &mut browser).await;
            let reply = callback(&test, &mut browser, &query["state"]).await;

            assert_eq!(
                reply.status,
                StatusCode::BAD_REQUEST,
                "{what}: {}",
                reply.text()
            );
            assert!(reply.set_cookie(COOKIE).is_none(), "{what}");
        }
        idp.will_sign_badly(None, None, false);
        let mut browser = Browser::default();
        let query = start(&test, &idp, &mut browser).await;
        let good = callback(&test, &mut browser, &query["state"]).await;
        assert_eq!(
            good.status,
            StatusCode::SEE_OTHER,
            "a good token still works"
        );
        assert_eq!(sessions_of(test.app.pool(), &user_id(&test).await).await, 1);
    }

    /// `POST /api/v1/me/identities/example/link` with an empty body.
    async fn post_link(test: &TestApp, browser: &mut Browser) -> Reply {
        browser
            .send(
                test,
                "POST",
                "/api/v1/me/identities/example/link",
                Payload::Json(json!({})),
            )
            .await
    }

    /// The number of flows in progress.
    fn flows(test: &TestApp) -> usize {
        test.app.oidc().expect("OIDC is on").flows.len()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn past_the_address_limit_a_start_is_429_without_a_flow() {
        let idp = idp::start().await;
        let test = oidc_app(&idp, "OKF_TEST_OIDC_SECRET_LIMIT").await;

        // `oneshot` requests carry no address, so they share one bucket.
        for _ in 0..SIGN_IN_PER_IP {
            start(&test, &idp, &mut Browser::default()).await;
        }
        let mut browser = Browser::default();
        let limited = browser
            .send(&test, "GET", "/auth/oidc/example/start", Payload::Empty)
            .await;
        let unknown = browser
            .send(&test, "GET", "/auth/oidc/nope/start", Payload::Empty)
            .await;

        assert_eq!(
            limited.status,
            StatusCode::TOO_MANY_REQUESTS,
            "{}",
            limited.text()
        );
        assert!(limited.set_cookie(FLOW_COOKIE).is_none());
        assert_eq!(
            flows(&test),
            usize::try_from(SIGN_IN_PER_IP).expect("small")
        );
        assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_browser_starting_again_replaces_its_previous_flow() {
        let idp = idp::start().await;
        let test = oidc_app(&idp, "OKF_TEST_OIDC_SECRET_RESTART").await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        idp.will_issue("sub-1", Some("alice@example.com"), Some(true));
        let mut browser = Browser::default();

        for _ in 0..3 {
            start(&test, &idp, &mut browser).await;
        }
        let query = start(&test, &idp, &mut browser).await;
        let done = callback(&test, &mut browser, &query["state"]).await;

        assert_eq!(done.status, StatusCode::SEE_OTHER, "{}", done.text());
        assert_eq!(
            flows(&test),
            0,
            "the earlier flows were dropped, not stranded"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn repeated_link_starts_never_grow_the_flow_map_and_others_still_start() {
        let idp = idp::start().await;
        let test = oidc_app(&idp, "OKF_TEST_OIDC_SECRET_FLOOD").await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        user_with_password(&test, "bob", "bob@example.com", Some(PASSWORD)).await;
        let alice = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let session = alice.cookie(COOKIE).expect("a session").to_string();
        let csrf = alice.csrf.clone();

        // A script that never keeps the flow cookie, on one session: each
        // start replaces the session's previous flow.
        for _ in 0..5 {
            let mut scripted = Browser::with_cookie(COOKIE, &session);
            scripted.csrf = csrf.clone();
            assert_eq!(post_link(&test, &mut scripted).await.status, StatusCode::OK);
        }
        assert_eq!(flows(&test), 1, "one live link flow per session");

        // New sessions each time: the per-user allowance runs out.
        let mut refused = None;
        for _ in 5..=LINK_STARTS_PER_USER {
            let mut fresh = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
            let reply = post_link(&test, &mut fresh).await;
            if reply.status != StatusCode::OK {
                refused = Some(reply);
                break;
            }
        }
        let refused = refused.expect("the allowance ran out");
        assert_eq!(
            refused.status,
            StatusCode::TOO_MANY_REQUESTS,
            "{}",
            refused.text()
        );
        assert!(refused.set_cookie(FLOW_COOKIE).is_none());
        assert!(flows(&test) <= usize::try_from(LINK_STARTS_PER_USER).expect("small"));

        let mut bob = Browser::signed_in(&test, "bob@example.com", PASSWORD).await;
        let anyone = Browser::default()
            .send(&test, "GET", "/auth/oidc/example/start", Payload::Empty)
            .await;
        assert_eq!(
            post_link(&test, &mut bob).await.status,
            StatusCode::OK,
            "another user"
        );
        assert_eq!(anyone.status, StatusCode::SEE_OTHER, "a sign-in");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_subject_postgres_would_refuse_is_400_not_500() {
        let idp = idp::start().await;
        let test = oidc_app(&idp, "OKF_TEST_OIDC_SECRET_NUL").await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        idp.will_issue("sub\0one", Some("alice@example.com"), Some(true));
        let mut browser = Browser::default();

        let query = start(&test, &idp, &mut browser).await;
        let reply = callback(&test, &mut browser, &query["state"]).await;

        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.text());
        assert_eq!(browser.cookie(COOKIE), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_signed_in_user_links_a_provider_whatever_its_email() {
        let idp = idp::start().await;
        let test = oidc_app(&idp, "OKF_TEST_OIDC_SECRET_ACCOUNT").await;
        let alice = user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let mut here = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let mut elsewhere = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        idp.will_issue("sub-9", Some("someone@elsewhere.example"), Some(false));

        let query = start_link(&test, &idp, &mut here, json!({})).await;
        let done = callback(&test, &mut here, &query["state"]).await;

        assert_eq!(done.status, StatusCode::SEE_OTHER, "{}", done.text());
        assert_eq!(done.location(), "/ui/#/account");
        let linked = identities::list(test.app.pool(), &alice.id)
            .await
            .expect("list");
        assert_eq!(linked[0].email_at_link, "someone@elsewhere.example");
        let kept = here
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;
        let ended = elsewhere
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;
        assert_eq!(kept.status, StatusCode::OK);
        assert_eq!(ended.status, StatusCode::UNAUTHORIZED);
        wait_for_mail(&test, 1).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn keep_sessions_keeps_the_other_sessions_when_linking() {
        let idp = idp::start().await;
        let test = oidc_app(&idp, "OKF_TEST_OIDC_SECRET_KEEP").await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let mut here = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let mut elsewhere = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        idp.will_issue("sub-9", None, None);

        let query = start_link(&test, &idp, &mut here, json!({ "keep_sessions": true })).await;
        let done = callback(&test, &mut here, &query["state"]).await;

        assert_eq!(done.status, StatusCode::SEE_OTHER, "{}", done.text());
        let kept = elsewhere
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;
        assert_eq!(kept.status, StatusCode::OK);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_link_callback_needs_the_live_session_that_started_it() {
        let idp = idp::start().await;
        let test = oidc_app(&idp, "OKF_TEST_OIDC_SECRET_BOUND").await;
        let alice = user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        user_with_password(&test, "bob", "bob@example.com", Some(PASSWORD)).await;
        idp.will_issue("sub-9", Some("mallory@example.com"), Some(true));

        // The flow cookie alone, without alice's session (another browser
        // handed the callback, or alice signed out meanwhile).
        let mut started = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let query = start_link(&test, &idp, &mut started, json!({})).await;
        let flow = started.cookie(FLOW_COOKIE).expect("a flow").to_string();
        let mut bare = Browser::with_cookie(FLOW_COOKIE, &flow);
        let without_session = callback(&test, &mut bare, &query["state"]).await;

        // The flow cookie beside another user's session.
        let mut started = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let query = start_link(&test, &idp, &mut started, json!({})).await;
        let flow = started.cookie(FLOW_COOKIE).expect("a flow").to_string();
        let mut as_bob = Browser::signed_in(&test, "bob@example.com", PASSWORD).await;
        as_bob.add_cookie(FLOW_COOKIE, &flow);
        let with_bob = callback(&test, &mut as_bob, &query["state"]).await;

        // Alice's session, signed out before the provider sent her back.
        let mut started = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let alice_session = started.cookie(COOKIE).expect("a session").to_string();
        let query = start_link(&test, &idp, &mut started, json!({})).await;
        let signed_out = started
            .send(&test, "DELETE", "/api/v1/session", Payload::Empty)
            .await;
        assert_eq!(signed_out.status, StatusCode::NO_CONTENT);
        // A second tab still sends the session cookie the first one cleared.
        let mut stale = Browser::with_cookie(COOKIE, &alice_session);
        stale.add_cookie(FLOW_COOKIE, started.cookie(FLOW_COOKIE).expect("a flow"));
        let after_sign_out = callback(&test, &mut stale, &query["state"]).await;

        for (what, reply) in [
            ("no session", &without_session),
            ("bob's session", &with_bob),
            ("signed out", &after_sign_out),
        ] {
            assert_eq!(
                reply.status,
                StatusCode::BAD_REQUEST,
                "{what}: {}",
                reply.text()
            );
            assert!(reply.text().contains(LINK_RETRY), "{what}");
        }
        assert!(identities::list(test.app.pool(), &alice.id)
            .await
            .expect("list")
            .is_empty());
        let bob = users::find_by_email(test.app.pool(), "bob@example.com")
            .await
            .expect("find")
            .expect("bob");
        assert!(identities::list(test.app.pool(), &bob.id)
            .await
            .expect("list")
            .is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn linking_a_subject_another_user_holds_is_409() {
        let idp = idp::start().await;
        let test = oidc_app(&idp, "OKF_TEST_OIDC_SECRET_TAKEN").await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let bob = user_with_password(&test, "bob", "bob@example.com", Some(PASSWORD)).await;
        identities::link(
            test.app.pool(),
            &bob.id,
            "example",
            "sub-1",
            "bob@example.com",
            chrono::Utc::now(),
        )
        .await
        .expect("bob holds sub-1");
        let mut alice = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        idp.will_issue("sub-1", Some("bob@example.com"), Some(true));

        let query = start_link(&test, &idp, &mut alice, json!({ "keep_sessions": true })).await;
        let done = callback(&test, &mut alice, &query["state"]).await;

        assert_eq!(done.status, StatusCode::CONFLICT);
        assert!(done.text().contains("linked to another user"));
        assert!(LINKED_ELSEWHERE.contains("linked to another user"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_second_subject_claiming_a_linked_users_email_is_409() {
        let idp = idp::start().await;
        let test = oidc_app(&idp, "OKF_TEST_OIDC_SECRET_SECOND").await;
        let alice = user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        identities::link(
            test.app.pool(),
            &alice.id,
            "example",
            "sub-1",
            "alice@example.com",
            chrono::Utc::now(),
        )
        .await
        .expect("alice holds sub-1");
        idp.will_issue("sub-2", Some("alice@example.com"), Some(true));
        let mut browser = Browser::default();

        let query = start(&test, &idp, &mut browser).await;
        let reply = callback(&test, &mut browser, &query["state"]).await;

        assert_eq!(reply.status, StatusCode::CONFLICT, "{}", reply.text());
        assert_eq!(browser.cookie(COOKIE), None);
        assert_eq!(
            identities::list(test.app.pool(), &alice.id)
                .await
                .expect("list")
                .len(),
            1
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_sign_in_waiting_on_an_unlink_gets_no_session() {
        let idp = idp::start().await;
        let test = oidc_app(&idp, "OKF_TEST_OIDC_SECRET_UNLINK").await;
        let alice = user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let pool = test.app.pool();
        identities::link(
            pool,
            &alice.id,
            "example",
            "sub-1",
            "alice@example.com",
            chrono::Utc::now(),
        )
        .await
        .expect("link");
        idp.will_issue("sub-1", Some("alice@example.com"), Some(true));
        let mut browser = Browser::default();
        let query = start(&test, &idp, &mut browser).await;

        // An unlink in progress: it holds alice's row, as
        // `sign_in_methods::count` takes it. The callback finds alice by
        // `(provider, sub)` (a read that takes no lock) and then queues on
        // her row to insert the session; only then is the identity deleted
        // and the unlink committed.
        let mut holder = pool.begin().await.expect("begin");
        sqlx::query("SELECT 1 FROM users WHERE id = $1 FOR NO KEY UPDATE")
            .bind(&alice.id)
            .execute(&mut *holder)
            .await
            .expect("lock alice");
        let unlink = async {
            wait_for_lock_waiters(pool, 1).await;
            sqlx::query("DELETE FROM user_identities WHERE user_id = $1")
                .bind(&alice.id)
                .execute(&mut *holder)
                .await
                .expect("unlink");
            holder.commit().await.expect("commit the unlink");
        };
        // `join!` polls both futures on this task until both finish, like
        // `asyncio.gather`: the callback waits on the lock while `unlink`
        // runs.
        let (reply, ()) = tokio::join!(callback(&test, &mut browser, &query["state"]), unlink);

        assert_eq!(reply.status, StatusCode::BAD_REQUEST, "{}", reply.text());
        assert!(reply.set_cookie(COOKIE).is_none());
        assert_eq!(sessions_of(pool, &alice.id).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_sign_in_commits_nothing_until_its_event_is_written_and_never_waits_on_mail() {
        let idp = idp::start().await;
        let test = oidc_app(&idp, "OKF_TEST_OIDC_SECRET_COMMIT").await;
        let alice = user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let bob = user_with_password(&test, "bob", "bob@example.com", Some(PASSWORD)).await;
        let pool = test.app.pool();

        // The event table is locked: the sign-in waits inside its
        // transaction, with nothing committed, and finishes once it can.
        idp.will_issue("sub-1", Some("alice@example.com"), Some(true));
        let mut browser = Browser::default();
        let query = start(&test, &idp, &mut browser).await;
        let mut events = pool.begin().await.expect("begin");
        sqlx::query("LOCK TABLE sign_in_events IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *events)
            .await
            .expect("lock the events");
        let release = async {
            wait_for_lock_waiters(pool, 1).await;
            assert_eq!(sessions_of(pool, &alice.id).await, 0, "no session yet");
            assert!(
                identities::list(pool, &alice.id)
                    .await
                    .expect("list")
                    .is_empty(),
                "no link yet"
            );
            events.rollback().await.expect("release the events");
        };
        let (reply, ()) = tokio::join!(callback(&test, &mut browser, &query["state"]), release);

        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.text());
        assert!(reply.set_cookie(COOKIE).is_some());
        assert_eq!(sessions_of(pool, &alice.id).await, 1);
        let recorded = sign_in_events::recent(pool, "alice", 5)
            .await
            .expect("events");
        assert!(matches!(
            (&recorded[0].method, &recorded[0].outcome),
            (Method::Oidc, Outcome::Ok)
        ));

        // The mail queue is locked: the committed sign-in answers at once,
        // and the notice is queued once the queue is free.
        idp.will_issue("sub-2", Some("bob@example.com"), Some(true));
        let mut browser = Browser::default();
        let query = start(&test, &idp, &mut browser).await;
        let mut jobs = pool.begin().await.expect("begin");
        sqlx::query("LOCK TABLE apalis.jobs IN ACCESS EXCLUSIVE MODE")
            .execute(&mut *jobs)
            .await
            .expect("lock the mail queue");
        let reply = tokio::time::timeout(
            Duration::from_secs(30),
            callback(&test, &mut browser, &query["state"]),
        )
        .await
        .expect("the response never waits on the mail queue");
        assert_eq!(reply.status, StatusCode::SEE_OTHER, "{}", reply.text());
        assert_eq!(sessions_of(pool, &bob.id).await, 1);
        jobs.rollback().await.expect("release the mail queue");
        wait_for_mail(&test, 2).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_oidc_session_ends_with_a_reset_or_an_admin_change() {
        let idp = idp::start().await;
        let test = oidc_app(&idp, "OKF_TEST_OIDC_SECRET_RESET").await;
        let alice = user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        idp.will_issue("sub-1", Some("alice@example.com"), Some(true));
        let pool = test.app.pool();

        let mut first = Browser::default();
        let query = start(&test, &idp, &mut first).await;
        callback(&test, &mut first, &query["state"]).await;
        passwords::reset(
            pool,
            &alice.id,
            "amber-quarry-signal-77",
            chrono::Utc::now(),
        )
        .await
        .expect("reset");
        let after_reset = first
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;

        let mut second = Browser::default();
        let query = start(&test, &idp, &mut second).await;
        callback(&test, &mut second, &query["state"]).await;
        users::set_admin(pool, "alice", true, false)
            .await
            .expect("make alice an admin");
        let after_admin = second
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;

        assert_eq!(after_reset.status, StatusCode::UNAUTHORIZED);
        assert_eq!(after_admin.status, StatusCode::UNAUTHORIZED);
        assert_eq!(sessions_of(pool, &alice.id).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_sign_in_page_shows_each_provider_and_unknown_ones_are_404() {
        let idp = idp::start().await;
        let test = oidc_app(&idp, "OKF_TEST_OIDC_SECRET_PAGE").await;
        let mut browser = Browser::default();

        let page = browser
            .send(&test, "GET", "/signin", Payload::Empty)
            .await
            .text();
        let unknown = browser
            .send(&test, "GET", "/auth/oidc/nope/start", Payload::Empty)
            .await;

        assert!(page.contains(r#"id="signin-oidc-example""#));
        assert!(page.contains("Sign in with Example ID"));
        assert_eq!(unknown.status, StatusCode::NOT_FOUND);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn linking_needs_a_console_session_and_its_csrf_header() {
        let idp = idp::start().await;
        let test = oidc_app(&idp, "OKF_TEST_OIDC_SECRET_KEY").await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let mut keyed = Browser::default();
        keyed.bearer = Some(keys(&test).await.write);
        let mut forged = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        forged.csrf = None;

        let by_key = keyed
            .send(
                &test,
                "POST",
                "/api/v1/me/identities/example/link",
                Payload::Json(json!({})),
            )
            .await;
        let without_csrf = forged
            .send(
                &test,
                "POST",
                "/api/v1/me/identities/example/link",
                Payload::Json(json!({})),
            )
            .await;

        assert_eq!(by_key.status, StatusCode::FORBIDDEN);
        assert_eq!(without_csrf.status, StatusCode::FORBIDDEN);
        assert!(without_csrf.set_cookie(FLOW_COOKIE).is_none());
    }
}

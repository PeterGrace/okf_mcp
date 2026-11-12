//! Console sessions over HTTP (spec "Sessions"): the `okf_session` cookie,
//! the CSRF rule, the client address, and `require_principal`, the
//! `/api/v1` middleware that admits a bearer credential or a session
//! cookie. MCP and the archive routes keep bearer-only `require_key`, so a
//! cookie never reaches them.
//!
//! A session's principal is built on every request from the key cache (the
//! user's admin flag and ceiling as of the last reload), never from what
//! was read at sign-in: the flag can change between the password check and
//! the session's first request.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{ConnectInfo, Request, State};
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, HeaderValue, Method};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use subtle::ConstantTimeEq;

use crate::app::App;
use crate::auth::{authenticate, bearer, unauthorized, Principal};
use crate::config::AuthConfig;
use crate::error::OkfError;
use crate::http::AppError;
use crate::store::ceiling;
use crate::store::user_sessions::{self, NewSession};
use crate::store::users::{self, UserAuth};

/// The session cookie (plan Decision 9).
pub const COOKIE: &str = "okf_session";

/// The header a cookie-authenticated write must carry, equal to the
/// session's CSRF token.
pub const CSRF_HEADER: &str = "x-okf-csrf";

/// A cookie's `SameSite` attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SameSite {
    /// Sent on top-level navigations from other sites (the session cookie,
    /// so a link to the console arrives signed in; and the OIDC flow cookie,
    /// which the provider's redirect must carry).
    Lax,
    /// Never sent from another site (the passkey ceremony cookie).
    Strict,
}

impl SameSite {
    /// The attribute's value.
    fn as_str(self) -> &'static str {
        match self {
            Self::Lax => "Lax",
            Self::Strict => "Strict",
        }
    }
}

/// The console session a cookie-authenticated request came with, put in
/// the request's extensions next to its `Arc<Principal>`. Routes that act
/// on "the other sessions" (sign-out, passkeys, password change) read it.
#[derive(Clone)]
pub struct ConsoleSession {
    /// The raw session id (the cookie's value).
    pub id: String,
    /// Its CSRF token.
    pub csrf: String,
    /// When it ends regardless of activity.
    pub expires_at: DateTime<Utc>,
}

// Written by hand, not derived (Python's `__repr__`), so a stray `{:?}`
// never prints the id or the token.
impl std::fmt::Debug for ConsoleSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConsoleSession")
            .field("id", &"<redacted>")
            .field("csrf", &"<redacted>")
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// The client's address (plan Decision 7).
///
/// # Arguments
/// * `request_parts` - the request (its `ConnectInfo` extension and headers).
/// * `config` - `[auth]`, for `trusted_proxy_header`.
///
/// # Returns
/// With `trusted_proxy_header` set, the last address in the last such
/// header (the one the trusted proxy appended); otherwise, or when that
/// header is missing or unreadable, the peer address. Without the setting
/// no header is read at all: a client could write any value there. `None`
/// without `ConnectInfo` (in-process `oneshot` tests).
pub fn client_ip(request_parts: &Parts, config: &AuthConfig) -> Option<IpAddr> {
    let peer = request_parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map(|ConnectInfo(address)| address.ip());
    // `let ... else`: without a trusted header, the peer is the client.
    let Some(name) = config.trusted_proxy_header.as_deref() else {
        return peer;
    };
    forwarded_ip(&request_parts.headers, name).or(peer)
}

/// The last hop of the last `name` header. Earlier entries were written by
/// the client (or proxies before ours) and prove nothing.
fn forwarded_ip(headers: &HeaderMap, name: &str) -> Option<IpAddr> {
    let value = headers.get_all(name).iter().next_back()?.to_str().ok()?;
    value.rsplit(',').next()?.trim().parse().ok()
}

/// A cookie's value from the request's `Cookie` headers.
///
/// # Arguments
/// * `headers` - the request's headers.
/// * `name` - the cookie's name.
///
/// # Returns
/// The value, or `None` when absent or empty.
// `<'a>`: the returned `&str` borrows from `headers`, so it lives as long
// as they do (no copy is made).
pub(crate) fn cookie<'a>(headers: &'a HeaderMap, name: &str) -> Option<&'a str> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| {
            let (key, value) = pair.trim().split_once('=')?;
            (key == name && !value.is_empty()).then_some(value)
        })
}

/// One `Set-Cookie` value: `HttpOnly`, `Path=/`, `Secure` when asked.
///
/// # Arguments
/// * `name` - the cookie.
/// * `value` - its value (base64url from `keys::generate_with`, or empty to clear).
/// * `max_age` - its lifetime; zero clears it.
/// * `same_site` - its `SameSite`.
/// * `secure` - add `Secure` ([`secure_cookies`]).
///
/// # Returns
/// The header value.
///
/// # Panics
/// If `name` or `value` is not a valid header value: callers pass constants
/// and base64url, so that is a bug.
pub(crate) fn set_cookie(
    name: &str,
    value: &str,
    max_age: Duration,
    same_site: SameSite,
    secure: bool,
) -> HeaderValue {
    let secure = if secure { "; Secure" } else { "" };
    HeaderValue::from_str(&format!(
        "{name}={value}; Max-Age={}; Path=/; HttpOnly; SameSite={}{secure}",
        max_age.as_secs(),
        same_site.as_str(),
    ))
    .expect("cookie names and base64url values are valid header values")
}

/// Whether cookies get `Secure`: when `public_url` is `https` (plan
/// Decision 15; tests run on `http://127.0.0.1`).
///
/// # Arguments
/// * `app` - the shared state.
///
/// # Returns
/// `true` for an `https` `public_url`.
pub(crate) fn secure_cookies(app: &App) -> bool {
    app.config()
        .public_url
        .as_deref()
        .is_some_and(|url| url.starts_with("https://"))
}

/// Appends every header of `extra` to `target`, keeping repeated names
/// (`HeaderMap::extend` would replace an existing `Set-Cookie`).
///
/// # Arguments
/// * `target` - the headers to add to.
/// * `extra` - the headers to add.
pub(crate) fn append_all(target: &mut HeaderMap, extra: &HeaderMap) {
    for (name, value) in extra {
        target.append(name.clone(), value.clone());
    }
}

/// Does the request carry the session's CSRF token? Compared in constant
/// time.
fn csrf_matches(headers: &HeaderMap, expected: &str) -> bool {
    headers
        .get(CSRF_HEADER)
        .is_some_and(|value| value.as_bytes().ct_eq(expected.as_bytes()).into())
}

/// Methods that change nothing, so need no CSRF header.
fn is_safe_method(method: &Method) -> bool {
    matches!(*method, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// A user's sign-in identity: from the key cache, else (a user created
/// since the last reload) from the database.
///
/// # Arguments
/// * `app` - the shared state.
/// * `user_id` - the user's id.
///
/// # Returns
/// The user, or `None` once deleted.
///
/// # Errors
/// [`OkfError::Database`].
pub(crate) async fn session_user(app: &App, user_id: &str) -> Result<Option<UserAuth>, OkfError> {
    match app.keys().user_admin(user_id) {
        Some(user) => Ok(Some(user)),
        None => users::find_auth(app.pool(), user_id).await,
    }
}

/// The user of the console session this request's cookie names, for a
/// route outside `/api/v1` (Task 11's `/oauth/authorize`), which
/// [`require_principal`] does not cover. It touches the session as the
/// middleware does.
///
/// # Arguments
/// * `app` - the shared state.
/// * `headers` - the request's headers.
///
/// # Returns
/// The user, or `None` without a cookie, for an ended session, or once the
/// user is gone.
///
/// # Errors
/// [`OkfError::Database`].
// `expect(dead_code)` silences "never used" until a caller lands, and
// itself warns once one does, so it cannot be forgotten.
#[cfg_attr(
    not(test),
    expect(
        dead_code,
        reason = "OAuth consent as the signed-in user (Task 11) uses it; delete this then"
    )
)]
pub(crate) async fn signed_in_user(
    app: &App,
    headers: &HeaderMap,
) -> Result<Option<UserAuth>, OkfError> {
    let Some(id) = cookie(headers, COOKIE) else {
        return Ok(None);
    };
    let idle = app.settings().console_session_idle;
    match user_sessions::touch(app.pool(), id, idle, app.clock().now()).await? {
        Some(row) => session_user(app, &row.user_id).await,
        None => Ok(None),
    }
}

/// The principal of a live session's user, built now from the key cache
/// (or, for a user created since the last reload, from the database).
///
/// # Errors
/// [`OkfError::Database`].
async fn session_principal(app: &App, user_id: &str) -> Result<Option<Principal>, OkfError> {
    if let Some(user) = app.keys().user_admin(user_id) {
        return Ok(Some(Principal::for_session(
            &user,
            app.keys().ceiling_of(user_id),
        )));
    }
    // Not in the cache yet: two queries, until the next reload.
    let Some(user) = users::find_auth(app.pool(), user_id).await? else {
        return Ok(None);
    };
    match ceiling::ceiling_of(app.pool(), &user.name).await {
        Ok(ceiling) => Ok(Some(Principal::for_session(&user, ceiling))),
        // Deleted between the two reads.
        Err(OkfError::UnknownName { .. }) => Ok(None),
        Err(other) => Err(other),
    }
}

/// [`unauthorized`], also clearing a session cookie that no longer works.
fn unauthorized_clearing(app: &App) -> Response {
    let mut response = unauthorized(app);
    append_all(response.headers_mut(), &clear_cookie(app));
    response
}

/// axum middleware for `/api/v1`: a bearer credential, else a session
/// cookie (with [`CSRF_HEADER`] on anything but GET, HEAD and OPTIONS).
/// Inserts the `Arc<Principal>` and, for a cookie, the [`ConsoleSession`]
/// into the request's extensions.
///
/// # Arguments
/// * `app` - the shared state (the layer's state).
/// * `request` - the incoming request.
/// * `next` - the rest of the stack.
///
/// # Returns
/// The route's response; 401 with the challenge (clearing a dead cookie);
/// 403 for a cookie write without the CSRF token; 500 if a lookup fails.
// Used as `axum::middleware::from_fn_with_state(app, require_principal)`;
// axum fills each argument from the request (the extractor pattern).
pub async fn require_principal(
    State(app): State<Arc<App>>,
    mut request: Request,
    next: Next,
) -> Response {
    // A bearer credential wins and the cookie is not read at all (spec
    // "Bearer and cookie"); a bad bearer is a 401 even beside a good cookie.
    // The borrowed token ends with this expression, before `request` changes.
    let presented = match bearer(request.headers()) {
        Some(token) => Some(authenticate(&app, token).await),
        None => None,
    };
    match presented {
        Some(Ok(Some(principal))) => {
            tracing::debug!(key = %principal.key_id, "authenticated");
            request.extensions_mut().insert(principal);
            return next.run(request).await;
        }
        Some(Ok(None)) => return unauthorized(&app),
        // `AppError` logs it and answers 500 without detail.
        Some(Err(error)) => return AppError(error).into_response(),
        None => {}
    }
    // An owned copy: the request is changed below, which ends the borrow.
    let Some(id) = cookie(request.headers(), COOKIE).map(str::to_string) else {
        return unauthorized(&app);
    };
    let idle = app.settings().console_session_idle;
    let row = match user_sessions::touch(app.pool(), &id, idle, app.clock().now()).await {
        Ok(Some(row)) => row,
        Ok(None) => return unauthorized_clearing(&app),
        Err(error) => return AppError(error).into_response(),
    };
    if !is_safe_method(request.method()) && !csrf_matches(request.headers(), &row.csrf) {
        return AppError(OkfError::Forbidden {
            reason: "a console write needs its session's X-OKF-CSRF header; reload the console"
                .to_string(),
        })
        .into_response();
    }
    let principal = match session_principal(&app, &row.user_id).await {
        Ok(Some(principal)) => principal,
        Ok(None) => return unauthorized_clearing(&app),
        Err(error) => return AppError(error).into_response(),
    };
    tracing::debug!(key = %principal.key_id, "authenticated by a console session");
    request.extensions_mut().insert(Arc::new(principal));
    request.extensions_mut().insert(ConsoleSession {
        id,
        csrf: row.csrf,
        expires_at: row.expires_at,
    });
    next.run(request).await
}

/// Starts a console session: a new random id (never the one the browser
/// had: rotation), stored only as its hash, and the cookie.
///
/// # Arguments
/// * `app` - the shared state.
/// * `user` - the user who signed in (only its id is used: the session's
///   level is read from the key cache on each request).
/// * `ip` - their address, if known.
/// * `user_agent` - their `User-Agent` (the store keeps 256 characters).
///
/// # Returns
/// The `Set-Cookie` header and the new session (id and CSRF token).
///
/// # Errors
/// [`OkfError::Database`]; [`OkfError::Io`] if the random source fails;
/// [`OkfError::LimitExceeded`] for an absolute lifetime past the calendar.
pub async fn start_session(
    app: &App,
    user: &UserAuth,
    ip: Option<IpAddr>,
    user_agent: &str,
) -> Result<(HeaderMap, NewSession), OkfError> {
    let absolute = app.settings().console_session_absolute;
    let session = user_sessions::create(
        app.pool(),
        &user.id,
        ip,
        user_agent,
        absolute,
        app.clock().now(),
    )
    .await?;
    let mut headers = HeaderMap::new();
    headers.insert(
        header::SET_COOKIE,
        set_cookie(
            COOKIE,
            &session.id,
            absolute,
            SameSite::Lax,
            secure_cookies(app),
        ),
    );
    // The user's id only: never the session id or its token.
    tracing::info!(user = %user.id, "console session started");
    Ok((headers, session))
}

/// The `Set-Cookie` header that removes the session cookie.
///
/// # Arguments
/// * `app` - the shared state (for `Secure`).
///
/// # Returns
/// The header.
pub fn clear_cookie(app: &App) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::SET_COOKIE,
        set_cookie(
            COOKIE,
            "",
            Duration::ZERO,
            SameSite::Lax,
            secure_cookies(app),
        ),
    );
    headers
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::AuthConfig;

    fn parts(peer: Option<&str>, forwarded: &[&str]) -> Parts {
        // `axum::extract::Request` (imported above) is `Request<Body>`;
        // `builder()` lives on `http::Request<()>`.
        let mut builder = axum::http::Request::builder().uri("/signin");
        for value in forwarded {
            builder = builder.header("x-forwarded-for", *value);
        }
        let (mut parts, ()) = builder.body(()).expect("a request").into_parts();
        if let Some(peer) = peer {
            parts
                .extensions
                .insert(ConnectInfo(peer.parse::<SocketAddr>().expect("an address")));
        }
        parts
    }

    fn behind_proxy() -> AuthConfig {
        AuthConfig {
            trusted_proxy_header: Some("x-forwarded-for".to_string()),
            ..AuthConfig::default()
        }
    }

    #[test]
    fn without_a_trusted_header_the_peer_is_the_client() {
        let parts = parts(Some("192.0.2.1:5000"), &["198.51.100.9"]);
        assert_eq!(
            client_ip(&parts, &AuthConfig::default()),
            "192.0.2.1".parse().ok()
        );
    }

    #[test]
    fn behind_a_proxy_the_last_hop_of_the_last_header_is_the_client() {
        let parts = parts(
            Some("10.0.0.2:5000"),
            &["203.0.113.66, 198.51.100.1", "198.51.100.9, 192.0.2.44"],
        );
        assert_eq!(
            client_ip(&parts, &behind_proxy()),
            "192.0.2.44".parse().ok()
        );
    }

    #[test]
    fn a_missing_or_garbled_forwarded_header_falls_back_to_the_peer() {
        assert_eq!(
            client_ip(&parts(Some("10.0.0.2:5000"), &[]), &behind_proxy()),
            "10.0.0.2".parse().ok()
        );
        assert_eq!(
            client_ip(
                &parts(Some("10.0.0.2:5000"), &["not-an-address"]),
                &behind_proxy()
            ),
            "10.0.0.2".parse().ok()
        );
        // A client-written first hop never wins over a garbled last one.
        assert_eq!(
            client_ip(
                &parts(Some("10.0.0.2:5000"), &["192.0.2.9, garbage"]),
                &behind_proxy()
            ),
            "10.0.0.2".parse().ok()
        );
    }

    #[test]
    fn a_request_without_connect_info_has_no_address() {
        assert_eq!(client_ip(&parts(None, &[]), &AuthConfig::default()), None);
    }

    #[test]
    fn the_session_cookie_is_found_among_others_and_in_any_cookie_header() {
        let mut headers = HeaderMap::new();
        headers.append(
            header::COOKIE,
            HeaderValue::from_static("theme=dark; okf_passkey=p1"),
        );
        headers.append(
            header::COOKIE,
            HeaderValue::from_static("okf_session=abc; x=y"),
        );
        assert_eq!(cookie(&headers, COOKIE), Some("abc"));
        assert_eq!(cookie(&headers, "okf_passkey"), Some("p1"));
        assert_eq!(cookie(&headers, "missing"), None);
        let mut empty = HeaderMap::new();
        empty.insert(header::COOKIE, HeaderValue::from_static("okf_session="));
        assert_eq!(cookie(&empty, COOKIE), None, "an empty value is no cookie");
        let mut lookalike = HeaderMap::new();
        lookalike.insert(header::COOKIE, HeaderValue::from_static("xokf_session=abc"));
        assert_eq!(cookie(&lookalike, COOKIE), None, "names match exactly");
    }

    #[test]
    fn a_set_cookie_line_carries_every_attribute() {
        let secure = set_cookie(COOKIE, "abc", Duration::from_secs(60), SameSite::Lax, true);
        assert_eq!(
            secure,
            "okf_session=abc; Max-Age=60; Path=/; HttpOnly; SameSite=Lax; Secure"
        );
        let plain = set_cookie("okf_passkey", "", Duration::ZERO, SameSite::Strict, false);
        assert_eq!(
            plain,
            "okf_passkey=; Max-Age=0; Path=/; HttpOnly; SameSite=Strict"
        );
    }

    #[test]
    fn the_csrf_header_must_equal_the_sessions_token() {
        let mut headers = HeaderMap::new();
        assert!(!csrf_matches(&headers, "token"));
        headers.insert(CSRF_HEADER, HeaderValue::from_static("other"));
        assert!(!csrf_matches(&headers, "token"));
        headers.insert(CSRF_HEADER, HeaderValue::from_static("tokenx"));
        assert!(
            !csrf_matches(&headers, "token"),
            "a longer value is no match"
        );
        headers.insert(CSRF_HEADER, HeaderValue::from_static("token"));
        assert!(csrf_matches(&headers, "token"));
    }

    #[test]
    fn only_get_head_and_options_skip_the_csrf_check() {
        for safe in [Method::GET, Method::HEAD, Method::OPTIONS] {
            assert!(is_safe_method(&safe), "{safe}");
        }
        for write in [Method::POST, Method::PUT, Method::PATCH, Method::DELETE] {
            assert!(!is_safe_method(&write), "{write}");
        }
    }

    #[test]
    fn a_console_session_never_debug_prints_its_secrets() {
        let session = ConsoleSession {
            id: "session-id-secret".to_string(),
            csrf: "csrf-token-secret".to_string(),
            expires_at: Utc::now(),
        };

        let printed = format!("{session:?}");

        assert!(!printed.contains("session-id-secret"), "{printed}");
        assert!(!printed.contains("csrf-token-secret"), "{printed}");
        assert!(printed.contains("<redacted>"), "{printed}");
    }
}

#[cfg(test)]
mod http_tests {
    use axum::http::StatusCode;
    use chrono::Utc;

    use super::COOKIE;
    use crate::http::api::testing::{
        keys, login_app, user_with_password, Browser, Payload, PASSWORD,
    };
    use crate::store::ceiling;
    use crate::store::keys::Access;
    use crate::store::{passwords, user_sessions, users};

    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_reaches_the_api_with_exactly_its_ceiling() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        ceiling::entitle(test.app.pool(), "alice", "main", Access::Read, Utc::now())
            .await
            .expect("entitle");
        test.app.poll().await.expect("poll");
        let mut browser = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;

        let whoami = browser
            .send(&test, "GET", "/api/v1/whoami", Payload::Empty)
            .await;
        let session = browser
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;
        let reached = browser
            .send(&test, "GET", "/api/v1/catalogs/main/tree", Payload::Empty)
            .await;
        let unreached = browser
            .send(&test, "GET", "/api/v1/catalogs/other/tree", Payload::Empty)
            .await;
        let admin_only = browser
            .send(&test, "GET", "/api/v1/users", Payload::Empty)
            .await;
        let keys_list = browser
            .send(&test, "GET", "/api/v1/keys", Payload::Empty)
            .await;

        assert_eq!(whoami.status, StatusCode::OK, "{}", whoami.text());
        let whoami = whoami.json();
        assert_eq!(whoami["kind"], "session");
        assert_eq!(whoami["key_id"], "session:alice");
        assert_eq!(whoami["name"], serde_json::Value::Null);
        assert_eq!(whoami["user"], "alice");
        assert_eq!(whoami["access"], "read");
        assert_eq!(whoami["all_catalogs"], false);
        assert_eq!(whoami["catalogs"], serde_json::json!(["main"]));
        assert_eq!(whoami["teams"], serde_json::json!(["alice"]));
        let session = session.json();
        assert_eq!(session["user"], "alice");
        assert_eq!(session["email"], "alice@example.com");
        assert_eq!(session["admin"], false);
        assert_eq!(session["ceiling"], serde_json::json!({ "main": "read" }));
        assert!(session["csrf"]
            .as_str()
            .is_some_and(|csrf| csrf.len() >= 43));
        assert!(session["expires_at"].is_string());
        assert_eq!(reached.status, StatusCode::OK, "{}", reached.text());
        assert_eq!(unreached.status, StatusCode::NOT_FOUND);
        assert_eq!(admin_only.status, StatusCode::FORBIDDEN);
        assert_eq!(keys_list.status, StatusCode::FORBIDDEN);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_writes_only_where_its_ceiling_says_write() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let now = Utc::now();
        ceiling::entitle(test.app.pool(), "alice", "main", Access::Write, now)
            .await
            .expect("entitle main");
        ceiling::entitle(test.app.pool(), "alice", "other", Access::Read, now)
            .await
            .expect("entitle other");
        let mut browser = Browser::session_for(&test, "alice").await;
        let body = serde_json::json!({
            "disposition": "dismissed",
            "rationale": "checked",
            "idempotency_key": "k1",
        });
        let uri = |catalog: &str| {
            format!("/api/v1/catalogs/{catalog}/maintenance/findings/no-such-finding/disposition")
        };

        let on_main = browser
            .send(&test, "POST", &uri("main"), Payload::Json(body.clone()))
            .await;
        let on_other = browser
            .send(&test, "POST", &uri("other"), Payload::Json(body))
            .await;

        // Write on `main` passes the level check and reaches the lookup.
        assert_eq!(on_main.status, StatusCode::NOT_FOUND, "{}", on_main.text());
        assert_eq!(
            on_other.status,
            StatusCode::FORBIDDEN,
            "{}",
            on_other.text()
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_admin_users_session_is_an_admin_on_every_catalog() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        users::set_admin(test.app.pool(), "alice", true, false)
            .await
            .expect("set admin");
        test.app.poll().await.expect("poll");
        let mut browser = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;

        let whoami = browser
            .send(&test, "GET", "/api/v1/whoami", Payload::Empty)
            .await
            .json();
        let session = browser
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await
            .json();
        let users_list = browser
            .send(&test, "GET", "/api/v1/users", Payload::Empty)
            .await;

        assert_eq!(whoami["access"], "admin");
        assert_eq!(whoami["all_catalogs"], true);
        assert_eq!(whoami["catalogs"], serde_json::json!(["main", "other"]));
        assert_eq!(session["admin"], true);
        assert_eq!(users_list.status, StatusCode::OK, "{}", users_list.text());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_demotion_between_the_password_check_and_the_session_is_honoured() {
        // The issue #315 race: `set_admin` lands after the password was
        // checked (and its `UserAuth` read, admin) but before the session
        // exists, so `set_admin` had no session to end.
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        users::set_admin(test.app.pool(), "alice", true, false)
            .await
            .expect("set admin");
        test.app.poll().await.expect("poll");
        let passwords::SignInOutcome::Ok(checked) =
            passwords::verify(test.app.pool(), "alice@example.com", PASSWORD, Utc::now())
                .await
                .expect("check the password")
        else {
            panic!("the password was refused");
        };
        assert!(checked.is_admin, "admin when the password was checked");
        users::set_admin(test.app.pool(), "alice", false, true)
            .await
            .expect("clear admin");
        // What the API's `settle` and the poll loop do after a change.
        test.app.poll().await.expect("poll");
        let (_, session) = super::start_session(&test.app, &checked, None, "okf-test")
            .await
            .expect("start the session with the stale user");
        let mut browser = Browser::with_cookie(COOKIE, &session.id);
        browser.csrf = Some(session.csrf);

        let whoami = browser
            .send(&test, "GET", "/api/v1/whoami", Payload::Empty)
            .await
            .json();
        let current = browser
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await
            .json();
        let users_list = browser
            .send(&test, "GET", "/api/v1/users", Payload::Empty)
            .await;

        assert_eq!(whoami["access"], "read", "{whoami}");
        assert_eq!(whoami["all_catalogs"], false);
        assert_eq!(current["admin"], false);
        assert_eq!(users_list.status, StatusCode::FORBIDDEN);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_promotion_reaches_a_live_session_only_by_ending_it() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let mut browser = Browser::session_for(&test, "alice").await;
        let before = browser
            .send(&test, "GET", "/api/v1/whoami", Payload::Empty)
            .await
            .json();

        users::set_admin(test.app.pool(), "alice", true, false)
            .await
            .expect("set admin");
        let after = browser
            .send(&test, "GET", "/api/v1/whoami", Payload::Empty)
            .await;

        assert_eq!(before["access"], "read");
        // Decision 20: the change ended the session, at once, with no poll.
        assert_eq!(after.status, StatusCode::UNAUTHORIZED);
        assert_eq!(browser.cookie(COOKIE), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_cookie_write_needs_the_sessions_csrf_header() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let mut browser = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let csrf = browser.csrf.take();

        let refused = browser
            .send(&test, "DELETE", "/api/v1/session", Payload::Empty)
            .await;
        browser.csrf = Some("wrong".to_string());
        let wrong = browser
            .send(&test, "DELETE", "/api/v1/session", Payload::Empty)
            .await;
        browser.csrf = csrf;
        let still_signed_in = browser
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;
        let signed_out = browser
            .send(&test, "DELETE", "/api/v1/session", Payload::Empty)
            .await;
        let after = browser
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;

        assert_eq!(refused.status, StatusCode::FORBIDDEN);
        assert_eq!(wrong.status, StatusCode::FORBIDDEN);
        assert_eq!(
            still_signed_in.status,
            StatusCode::OK,
            "a refused write ends nothing"
        );
        assert_eq!(signed_out.status, StatusCode::NO_CONTENT);
        assert!(signed_out
            .set_cookie(COOKIE)
            .is_some_and(|line| line.contains("Max-Age=0;")));
        assert_eq!(browser.cookie(COOKIE), None);
        assert_eq!(after.status, StatusCode::UNAUTHORIZED);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_bearer_key_wins_over_the_cookie() {
        let test = login_app().await;
        let keys = keys(&test).await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let mut browser = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        browser.bearer = Some(keys.read.clone());

        let whoami = browser
            .send(&test, "GET", "/api/v1/whoami", Payload::Empty)
            .await
            .json();
        browser.bearer = Some("okf_not-a-real-key".to_string());
        let refused = browser
            .send(&test, "GET", "/api/v1/whoami", Payload::Empty)
            .await;

        assert_eq!(whoami["kind"], "key");
        assert_eq!(whoami["name"], "reader");
        assert_eq!(
            refused.status,
            StatusCode::UNAUTHORIZED,
            "a bad key is not rescued by the cookie"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_cookie_never_reaches_mcp_or_the_archive_routes() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        users::set_admin(test.app.pool(), "alice", true, false)
            .await
            .expect("set admin");
        // An admin session: every route below would admit an admin key.
        let mut browser = Browser::session_for(&test, "alice").await;
        let initialize = serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                       "clientInfo": {"name": "t", "version": "1"}},
        });

        let alive = browser
            .send(&test, "GET", "/api/v1/whoami", Payload::Empty)
            .await;
        let mcp = browser
            .send(&test, "POST", "/mcp", Payload::Json(initialize))
            .await;
        let export = browser
            .send(&test, "GET", "/catalogs/main/export", Payload::Empty)
            .await;
        let import = browser
            .send(&test, "POST", "/catalogs/main/import", Payload::Empty)
            .await;

        assert_eq!(alive.status, StatusCode::OK, "the session itself works");
        for (route, reply) in [("mcp", mcp), ("export", export), ("import", import)] {
            assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{route}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_revoked_session_is_refused_at_once() {
        let test = login_app().await;
        let alice = user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let mut browser = Browser::session_for(&test, "alice").await;
        let before = browser
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;

        user_sessions::revoke_all(test.app.pool(), &alice.id, None)
            .await
            .expect("revoke");
        let after = browser
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;

        assert_eq!(before.status, StatusCode::OK);
        assert_eq!(after.status, StatusCode::UNAUTHORIZED, "no poll needed");
        assert!(after
            .set_cookie(COOKIE)
            .is_some_and(|line| line.contains("Max-Age=0;")));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_idle_or_expired_session_is_refused_and_its_cookie_cleared() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let mut idle = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        sqlx::query("UPDATE user_sessions SET last_seen_at = now() - interval '13 hours'")
            .execute(test.app.pool())
            .await
            .expect("age the session");

        let idle_reply = idle
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;

        let mut expired = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        sqlx::query("UPDATE user_sessions SET expires_at = now() - interval '1 second'")
            .execute(test.app.pool())
            .await
            .expect("expire the session");
        let expired_reply = expired
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;

        for reply in [idle_reply, expired_reply] {
            assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
            assert!(reply
                .set_cookie(COOKIE)
                .is_some_and(|line| line.contains("Max-Age=0;")));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_cookie_is_a_401_that_clears_it() {
        let test = login_app().await;
        let mut browser = Browser::with_cookie(COOKIE, "not-a-session");

        let reply = browser
            .send(&test, "GET", "/api/v1/whoami", Payload::Empty)
            .await;
        let bare = Browser::default()
            .send(&test, "GET", "/api/v1/whoami", Payload::Empty)
            .await;

        assert_eq!(reply.status, StatusCode::UNAUTHORIZED);
        assert!(reply.set_cookie(COOKIE).is_some());
        assert!(reply.headers.contains_key("www-authenticate"));
        assert_eq!(bare.status, StatusCode::UNAUTHORIZED);
        assert!(
            bare.set_cookie(COOKIE).is_none(),
            "no cookie, nothing to clear"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn each_sign_in_gets_a_new_session_and_a_full_cookie() {
        let test = login_app().await;
        let alice = user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;

        let (first_cookie, first) = super::start_session(&test.app, &alice, None, "okf-test")
            .await
            .expect("first");
        let (_, second) = super::start_session(&test.app, &alice, None, "okf-test")
            .await
            .expect("second");

        assert_ne!(first.id, second.id, "a new id at every sign-in");
        assert_ne!(first.csrf, second.csrf);
        let line = first_cookie["set-cookie"].to_str().expect("text");
        let absolute = test.app.settings().console_session_absolute.as_secs();
        assert_eq!(
            line,
            format!(
                "okf_session={}; Max-Age={absolute}; Path=/; HttpOnly; SameSite=Lax; Secure",
                first.id
            )
        );
        let stored: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM user_sessions WHERE id_hash = $1 OR csrf_token = $2",
        )
        .bind(first.id.as_bytes())
        .bind(&first.csrf)
        .fetch_one(test.app.pool())
        .await
        .expect("count");
        assert_eq!(stored, 0, "neither raw value is stored");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn signed_in_user_reads_the_cookie_outside_the_api() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let browser = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let line = format!("{COOKIE}={}", browser.cookie(COOKIE).expect("a session"));
        let mut headers = axum::http::HeaderMap::new();
        headers.insert(
            axum::http::header::COOKIE,
            line.parse().expect("a header value"),
        );

        let user = super::signed_in_user(&test.app, &headers)
            .await
            .expect("look up");
        let nobody = super::signed_in_user(&test.app, &axum::http::HeaderMap::new())
            .await
            .expect("look up");

        assert_eq!(user.map(|user| user.name), Some("alice".to_string()));
        assert!(nobody.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_user_new_to_the_cache_is_read_from_the_database() {
        let test = login_app().await;
        let pool = test.app.pool();
        // Created and entitled with no poll: the cache does not know bob.
        users::create(
            pool,
            &users::NewUser {
                name: "bob",
                display_name: "",
            },
            Utc::now(),
        )
        .await
        .expect("create bob");
        ceiling::entitle(pool, "bob", "main", Access::Write, Utc::now())
            .await
            .expect("entitle");
        let bob_id: String = sqlx::query_scalar("SELECT id FROM users WHERE name = 'bob'")
            .fetch_one(pool)
            .await
            .expect("bob's id");
        assert_eq!(test.app.keys().user_admin(&bob_id), None);

        let principal = super::session_principal(&test.app, &bob_id)
            .await
            .expect("look up")
            .expect("bob");
        let nobody = super::session_principal(&test.app, "no-such-user")
            .await
            .expect("look up");

        assert_eq!(principal.key_id, "session:bob");
        assert_eq!(principal.level("main"), Some(Access::Write));
        assert_eq!(principal.level("other"), None);
        assert!(nobody.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_key_has_no_console_session_to_read_or_end() {
        let test = login_app().await;
        let keys = keys(&test).await;
        // Field by field: `..Browser::default()` cannot fill its private `cookies`.
        let mut browser = Browser::default();
        browser.bearer = Some(keys.admin.clone());

        let read = browser
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;
        let end = browser
            .send(&test, "DELETE", "/api/v1/session", Payload::Empty)
            .await;

        assert_eq!(read.status, StatusCode::FORBIDDEN);
        assert_eq!(end.status, StatusCode::FORBIDDEN);
    }
}

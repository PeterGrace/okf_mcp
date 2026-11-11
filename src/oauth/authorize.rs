//! `GET` and `POST /oauth/authorize` (spec "Endpoints" and "Consent flow").
//!
//! The GET validates the request in a fixed order. Until the client and its
//! exact redirect URI are known, a failure is an error page, never a
//! redirect (no open redirect). After that, failures go back to the client
//! as `error=...`. A valid request becomes a pending authorization in
//! memory, and the POSTs walk it through the sign-in code and consent.

use std::sync::Arc;
use std::time::Instant;

use axum::extract::rejection::{FormRejection, QueryRejection};
use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use serde::Deserialize;
use subtle::ConstantTimeEq;

use crate::app::App;
use crate::error::OkfError;
use crate::keys;
use crate::maintenance::types::new_id;
use crate::oauth::clients;
use crate::oauth::consent::{self, GrantSummary, PageView};
use crate::oauth::error::OAuthError;
use crate::oauth::state::{code_key, AuthorizationCode, PendingAuthorization, MAX_FAILED_ATTEMPTS};
use crate::oauth::{pkce, OAuth};
use crate::store::oauth::clients::{ClientKind, ClientRecord};
use crate::store::oauth::codes::{self as code_rows, SignInCodeRecord};

/// The message for every unusable sign-in code, so the page never says
/// whether a code exists, expired, was redeemed or was revoked.
const BAD_CODE: &str = "That sign-in code is not valid. Check it, or ask for a new one.";
/// The message for an unknown, expired or forged pending authorization.
const EXPIRED: &str = "This sign-in request has expired or is not valid.";

/// The routes: `GET` and `POST /oauth/authorize`.
///
/// # Returns
/// A router for [`crate::oauth::routes`] to merge.
pub fn routes() -> Router<Arc<App>> {
    Router::new().route("/oauth/authorize", get(authorize).post(consent_post))
}

/// The authorization request's parameters. Every field is optional, so a
/// missing one is the handler's error, in the spec's order, not serde's.
#[derive(Debug, Default, Deserialize)]
struct AuthorizeQuery {
    response_type: Option<String>,
    client_id: Option<String>,
    redirect_uri: Option<String>,
    state: Option<String>,
    code_challenge: Option<String>,
    code_challenge_method: Option<String>,
    resource: Option<String>,
    // `scope` is accepted and ignored (serde skips unknown fields): the
    // sign-in code decides the grant.
}

/// The consent form's fields.
// `Debug` by hand: the sign-in code and the CSRF token never reach a log.
#[derive(Deserialize)]
struct ConsentForm {
    pending: String,
    csrf: String,
    action: String,
    sign_in_code: Option<String>,
}

impl std::fmt::Debug for ConsentForm {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConsentForm")
            .field("pending", &self.pending)
            .field("action", &self.action)
            .finish_non_exhaustive()
    }
}

/// A 303 to the client's redirect URI with `pairs`, `state` and `iss` added.
///
/// 303, not 307: a 307 would make the browser re-POST the consent form,
/// sign-in code included, to the client.
///
/// # Arguments
/// * `redirect_uri` - an exact match of one of the client's URIs.
/// * `pairs` - `code`, or `error` and `error_description`.
/// * `state` - the client's `state`, echoed if it sent one.
/// * `issuer` - this server's issuer (RFC 9207).
///
/// # Returns
/// The redirect.
fn back_to_client(
    redirect_uri: &str,
    pairs: &[(&str, &str)],
    state: Option<&str>,
    issuer: &str,
) -> Response {
    // Registration (Task 3) parsed every redirect URI, so this cannot fail;
    // a failure is answered as an error page rather than a panic.
    let Ok(mut target) = url::Url::parse(redirect_uri) else {
        return consent::error_page(
            StatusCode::BAD_REQUEST,
            "The client's redirect URI is invalid.",
        );
    };
    {
        // `query_pairs_mut` borrows `target` mutably until this block ends,
        // and keeps any query the registered URI already has.
        let mut query = target.query_pairs_mut();
        query.extend_pairs(pairs);
        if let Some(state) = state {
            query.append_pair("state", state);
        }
        query.append_pair("iss", issuer);
    }
    Redirect::to(target.as_str()).into_response()
}

/// An OAuth error sent back to the client.
///
/// # Arguments
/// * `redirect_uri` - an exact match of one of the client's URIs.
/// * `state` - the client's `state`, echoed if it sent one.
/// * `issuer` - this server's issuer.
/// * `code` - the OAuth error code.
/// * `description` - its `error_description`.
///
/// # Returns
/// The redirect.
fn error_to_client(
    redirect_uri: &str,
    state: Option<&str>,
    issuer: &str,
    code: &str,
    description: &str,
) -> Response {
    back_to_client(
        redirect_uri,
        &[("error", code), ("error_description", description)],
        state,
        issuer,
    )
}

/// The page view of a pending authorization.
fn view(pending_id: &str, pending: &PendingAuthorization) -> PageView {
    // The URI matched one the client registered, and registration (or the
    // CIMD fetch) parsed it, so the fallbacks never show.
    let parsed = url::Url::parse(&pending.redirect_uri).ok();
    let redirect_origin = parsed
        .as_ref()
        .map(|url| url.origin().ascii_serialization())
        .unwrap_or_default();
    let client_host = parsed
        .as_ref()
        .and_then(|url| url.host_str().map(str::to_string))
        .unwrap_or_default();
    PageView {
        pending_id: pending_id.to_string(),
        csrf: pending.csrf.clone(),
        client_name: pending
            .client
            .client_name
            .clone()
            .unwrap_or_else(|| pending.client.client_id.clone()),
        client_host,
        verified: pending.client.kind == ClientKind::Cimd,
        redirect_origin,
    }
}

/// What a sign-in code grants, as step 2 states it.
fn summary(record: &SignInCodeRecord) -> GrantSummary {
    GrantSummary {
        access: record.access.as_str(),
        catalogs: if record.all_catalogs {
            "every catalog".to_string()
        } else {
            record.catalogs.join(", ")
        },
        actor: record.actor.clone(),
        default_catalog: record.default_catalog.clone(),
    }
}

/// The trusted half of a request: its client and exact redirect URI.
///
/// # Returns
/// The client and the redirect URI.
///
/// # Errors
/// The status and message of the error page to show: never a redirect,
/// since neither the client nor the redirect URI is trusted yet.
async fn trusted_client<'q>(
    app: &App,
    oauth: &OAuth,
    query: &'q AuthorizeQuery,
) -> Result<(ClientRecord, &'q str), (StatusCode, &'static str)> {
    // 1. The client.
    let Some(client_id) = query.client_id.as_deref() else {
        return Err((StatusCode::BAD_REQUEST, "The request names no client."));
    };
    let client = match clients::resolve(app, oauth, client_id).await {
        Ok(client) => client,
        Err(OAuthError::Server(error)) => {
            tracing::error!(%error, "resolving an OAuth client failed");
            return Err((
                StatusCode::INTERNAL_SERVER_ERROR,
                "The server could not check this client.",
            ));
        }
        Err(_) => {
            return Err((
                StatusCode::BAD_REQUEST,
                "This client is not known to the server.",
            ))
        }
    };
    // 2. Its exact redirect URI: string equality, no prefix or pattern.
    let Some(redirect_uri) = query
        .redirect_uri
        .as_deref()
        .filter(|uri| client.redirect_uris.iter().any(|known| known == uri))
    else {
        return Err((
            StatusCode::BAD_REQUEST,
            "The request's redirect URI is not one the client registered.",
        ));
    };
    Ok((client, redirect_uri))
}

/// `GET /oauth/authorize`: validates the request and shows step 1.
async fn authorize(
    State(app): State<Arc<App>>,
    query: Result<Query<AuthorizeQuery>, QueryRejection>,
) -> Response {
    // OAuth off: the routes are not mounted then, so this is only a guard.
    let Some(oauth) = app.oauth() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // An unparsable query is treated like an empty one: the first missing
    // parameter, `client_id`, is then the error page. `map` unwraps the
    // `Query` tuple struct; a rejection becomes the all-`None` default.
    let query = query.map(|Query(query)| query).unwrap_or_default();
    let (client, redirect_uri) = match trusted_client(&app, oauth, &query).await {
        Ok(trusted) => trusted,
        Err((status, message)) => return consent::error_page(status, message),
    };
    // From here on, errors go back to the client.
    let issuer = oauth.settings.issuer.as_str();
    let state = query.state.as_deref();
    let back = |code: &str, description: &str| {
        error_to_client(redirect_uri, state, issuer, code, description)
    };
    // 3. The rest, in the spec's order.
    if query.response_type.as_deref() != Some("code") {
        return back(
            "unsupported_response_type",
            "only response_type=code is supported",
        );
    }
    let Some(code_challenge) = query
        .code_challenge
        .as_deref()
        .filter(|challenge| pkce::is_valid_challenge(challenge))
    else {
        return back("invalid_request", "a PKCE S256 code_challenge is required");
    };
    if query.code_challenge_method.as_deref() != Some("S256") {
        return back("invalid_request", "code_challenge_method must be S256");
    }
    if query.resource.as_deref() != Some(oauth.settings.resource.as_str()) {
        return back(
            "invalid_target",
            "resource must name this server's MCP endpoint",
        );
    }
    // 4. A pending authorization, and step 1.
    let (pending_id, csrf) = match (keys::generate_with(""), keys::generate_with("")) {
        (Ok(id), Ok(csrf)) => (id, csrf),
        (Err(error), _) | (_, Err(error)) => return server_error(&error),
    };
    let pending = PendingAuthorization {
        csrf,
        client,
        redirect_uri: redirect_uri.to_string(),
        state: query.state.clone(),
        code_challenge: code_challenge.to_string(),
        resource: oauth.settings.resource.clone(),
        sign_in_code_id: None,
        failed_attempts: 0,
    };
    let page = consent::code_page(&view(&pending_id, &pending), None);
    // `insert` is `false` when the bounded map is full: an unauthenticated
    // GET must not grow memory without limit.
    if !oauth.pending.insert(pending_id, pending, Instant::now()) {
        return consent::error_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "Too many sign-ins are in progress. Try again in a few minutes.",
        );
    }
    page
}

/// `POST /oauth/authorize`: the consent form (spec "Consent flow").
async fn consent_post(
    State(app): State<Arc<App>>,
    form: Result<Form<ConsentForm>, FormRejection>,
) -> Response {
    // OAuth off: the routes are not mounted then, so this is only a guard.
    let Some(oauth) = app.oauth() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let now = Instant::now();
    if !oauth.consent_limit.try_take(now) {
        return consent::error_page(
            StatusCode::TOO_MANY_REQUESTS,
            "Too many sign-in attempts. Wait a minute.",
        );
    }
    let Ok(Form(form)) = form else {
        return consent::error_page(StatusCode::BAD_REQUEST, EXPIRED);
    };
    // The CSRF token is checked before anything else, and a mismatch leaves
    // the pending authorization alone: someone who learned its id cannot
    // cancel it. `update` holds the map's lock only for the closure.
    let pending = oauth.pending.update(&form.pending, now, |pending| {
        let matches: bool = pending.csrf.as_bytes().ct_eq(form.csrf.as_bytes()).into();
        matches.then(|| pending.clone())
    });
    // `Some(Some(_))`: the entry is live (outer) and the token matched (inner).
    let Some(Some(pending)) = pending else {
        return consent::error_page(StatusCode::BAD_REQUEST, EXPIRED);
    };
    match form.action.as_str() {
        "deny" => {
            oauth.pending.remove(&form.pending);
            error_to_client(
                &pending.redirect_uri,
                pending.state.as_deref(),
                &oauth.settings.issuer,
                "access_denied",
                "the person signing in denied access",
            )
        }
        "code" => {
            let presented = form.sign_in_code.as_deref().unwrap_or_default();
            check_code(&app, oauth, &form.pending, &pending, presented).await
        }
        "approve" => approve(&app, oauth, &form.pending, &pending).await,
        _ => consent::error_page(StatusCode::BAD_REQUEST, EXPIRED),
    }
}

/// Counts a failed attempt and answers step 1 again, or drops the request
/// at [`MAX_FAILED_ATTEMPTS`].
fn failed_attempt(oauth: &OAuth, pending_id: &str, pending: &PendingAuthorization) -> Response {
    let attempts = oauth.pending.update(pending_id, Instant::now(), |pending| {
        pending.failed_attempts = pending.failed_attempts.saturating_add(1);
        pending.sign_in_code_id = None;
        pending.failed_attempts
    });
    match attempts {
        Some(attempts) if attempts < MAX_FAILED_ATTEMPTS => {
            consent::code_page(&view(pending_id, pending), Some(BAD_CODE))
        }
        Some(_) | None => {
            oauth.pending.remove(pending_id);
            consent::error_page(StatusCode::BAD_REQUEST, "Too many wrong sign-in codes.")
        }
    }
}

/// `action=code`: checks the code without consuming it, and shows step 2.
async fn check_code(
    app: &App,
    oauth: &OAuth,
    pending_id: &str,
    pending: &PendingAuthorization,
    presented: &str,
) -> Response {
    match code_rows::find_live(app.pool(), presented, app.clock().now()).await {
        Ok(Some(record)) => {
            let code_id = record.id.clone();
            let stored = oauth.pending.update(pending_id, Instant::now(), |pending| {
                pending.sign_in_code_id = Some(code_id);
            });
            if stored.is_none() {
                return consent::error_page(StatusCode::BAD_REQUEST, EXPIRED);
            }
            consent::summary_page(&view(pending_id, pending), &summary(&record))
        }
        Ok(None) => failed_attempt(oauth, pending_id, pending),
        Err(error) => server_error(&error),
    }
}

/// `action=approve`: redeems the code into a grant, mints an authorization
/// code and sends the browser back to the client.
async fn approve(
    app: &App,
    oauth: &OAuth,
    pending_id: &str,
    pending: &PendingAuthorization,
) -> Response {
    let Some(code_id) = pending.sign_in_code_id.as_deref() else {
        return consent::code_page(&view(pending_id, pending), None);
    };
    let grant_id =
        match code_rows::redeem(app.pool(), code_id, &pending.client, app.clock().now()).await {
            Ok(grant_id) => grant_id,
            // Expired, revoked or redeemed meanwhile (another approve won).
            Err(OkfError::Refused { .. }) => return failed_attempt(oauth, pending_id, pending),
            Err(error) => return server_error(&error),
        };
    // The request is spent once the grant exists, whatever happens next.
    oauth.pending.remove(pending_id);
    // `redeem` bumped admin_generation, so this loads the grant into the key
    // cache before the client's token request can arrive.
    if let Err(error) = app.poll().await {
        tracing::error!(%error, "poll after redeeming a sign-in code failed");
    }
    let (code, family_id) = match (
        keys::generate_with(keys::AUTHORIZATION_CODE_PREFIX),
        new_id(),
    ) {
        (Ok(code), Ok(family_id)) => (code, family_id),
        (Err(error), _) | (_, Err(error)) => return server_error(&error),
    };
    // A full map refuses the code; the grant stays and can be revoked, and
    // the person starts again from their client.
    let stored = oauth.codes.insert(
        code_key(&code),
        AuthorizationCode {
            grant_id,
            client_id: pending.client.client_id.clone(),
            redirect_uri: pending.redirect_uri.clone(),
            code_challenge: pending.code_challenge.clone(),
            resource: pending.resource.clone(),
            family_id,
            used: false,
        },
        Instant::now(),
    );
    if !stored {
        return consent::error_page(
            StatusCode::SERVICE_UNAVAILABLE,
            "The server is busy. Start again from your client in a minute.",
        );
    }
    back_to_client(
        &pending.redirect_uri,
        &[("code", &code)],
        pending.state.as_deref(),
        &oauth.settings.issuer,
    )
}

/// A 500 error page; the details go to the log only.
fn server_error(error: &OkfError) -> Response {
    tracing::error!(%error, "consent failed");
    consent::error_page(
        StatusCode::INTERNAL_SERVER_ERROR,
        "The server could not complete sign-in.",
    )
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant};

    use axum::http::{HeaderMap, StatusCode};

    use crate::mcp::testing::TestApp;
    use crate::oauth::pkce;
    use crate::oauth::state::code_key;
    use crate::oauth::testing::{
        authorize_query, form, hidden, oauth_app, send, sign_in_code, CLIENT_ID, PUBLIC_URL,
        REDIRECT_URI, VERIFIER,
    };
    use crate::store::keys::{self as key_rows, Access, KeyKind};
    use crate::store::oauth::codes as code_rows;

    /// Replaces one parameter of the valid query.
    fn query_with(name: &str, value: &str) -> String {
        authorize_query()
            .split('&')
            .map(|pair| match pair.split_once('=') {
                Some((key, _)) if key == name => form(&[(name, value)]),
                _ => pair.to_string(),
            })
            .collect::<Vec<_>>()
            .join("&")
    }

    /// The valid query without one parameter.
    fn query_without(name: &str) -> String {
        authorize_query()
            .split('&')
            .filter(|pair| !pair.starts_with(&format!("{name}=")))
            .collect::<Vec<_>>()
            .join("&")
    }

    fn location(headers: &HeaderMap) -> url::Url {
        url::Url::parse(headers["location"].to_str().unwrap()).unwrap()
    }

    fn param(url: &url::Url, name: &str) -> Option<String> {
        url.query_pairs()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.into_owned())
    }

    /// Runs step 1 and returns `(pending, csrf)`.
    async fn start(test: &TestApp) -> (String, String) {
        let (_, _, page) = send(
            test,
            "GET",
            &format!("/oauth/authorize?{}", authorize_query()),
            None,
        )
        .await;
        (hidden(&page, "pending"), hidden(&page, "csrf"))
    }

    async fn post(test: &TestApp, pairs: &[(&str, &str)]) -> (StatusCode, HeaderMap, String) {
        send(test, "POST", "/oauth/authorize", Some(form(pairs))).await
    }

    async fn oauth_grants(test: &TestApp) -> usize {
        key_rows::list(test.app.pool())
            .await
            .unwrap()
            .iter()
            .filter(|row| row.kind == KeyKind::OAuth)
            .count()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_valid_request_shows_step_one_for_the_client() {
        let test = oauth_app().await;
        let (status, headers, page) = send(
            &test,
            "GET",
            &format!("/oauth/authorize?{}", authorize_query()),
            None,
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("Example Client"));
        assert!(page.contains("returns to client.example.com"));
        assert!(page.contains("unverified"), "a DCR client");
        assert!(headers["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("form-action 'self' https://client.example.com;"));
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(headers["referrer-policy"], "no-referrer");
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert!(!hidden(&page, "pending").is_empty());
        assert!(!hidden(&page, "csrf").is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn before_the_redirect_uri_is_trusted_errors_never_redirect() {
        let test = oauth_app().await;
        for query in [
            query_with("client_id", "no-such-client"),
            query_without("client_id"),
            query_with("redirect_uri", "https://attacker.example.com/callback"),
            query_with("redirect_uri", &format!("{REDIRECT_URI}/extra")),
            query_with("redirect_uri", &format!("{REDIRECT_URI}?x=1")),
            query_without("redirect_uri"),
            // Later errors too: the untrusted redirect URI wins.
            format!(
                "{}&response_type=token",
                query_with("redirect_uri", "https://attacker.example.com/callback")
                    .replace("response_type=code", "")
            ),
            "%zz=%zz&&=".to_string(),
        ] {
            let (status, headers, page) =
                send(&test, "GET", &format!("/oauth/authorize?{query}"), None).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{query}");
            assert!(headers.get("location").is_none(), "{query}");
            assert!(page.contains("Cannot connect"), "{query}");
            assert!(!page.contains("attacker.example.com"), "{query}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn after_the_redirect_uri_is_trusted_errors_go_back_to_the_client() {
        let test = oauth_app().await;
        for (query, expected) in [
            (
                query_with("response_type", "token"),
                "unsupported_response_type",
            ),
            (query_without("response_type"), "unsupported_response_type"),
            (
                query_with("code_challenge_method", "plain"),
                "invalid_request",
            ),
            (query_without("code_challenge_method"), "invalid_request"),
            (query_with("code_challenge", "short"), "invalid_request"),
            (query_without("code_challenge"), "invalid_request"),
            (
                query_with("resource", "https://other.example.com/mcp"),
                "invalid_target",
            ),
            (query_without("resource"), "invalid_target"),
        ] {
            let (status, headers, _) =
                send(&test, "GET", &format!("/oauth/authorize?{query}"), None).await;
            assert_eq!(status, StatusCode::SEE_OTHER, "{query}");
            let target = location(&headers);
            assert!(target.as_str().starts_with(REDIRECT_URI), "{query}");
            assert_eq!(
                param(&target, "error").as_deref(),
                Some(expected),
                "{query}"
            );
            assert_eq!(param(&target, "state").as_deref(), Some("xyz"));
            assert_eq!(param(&target, "iss").as_deref(), Some(PUBLIC_URL));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_code_then_approve_creates_a_grant_and_redirects_with_a_code() {
        let test = oauth_app().await;
        let code = sign_in_code(&test, Access::Write).await;
        let (pending, csrf) = start(&test).await;

        let (status, headers, summary) = post(
            &test,
            &[
                ("pending", &pending),
                ("csrf", &csrf),
                ("action", "code"),
                ("sign_in_code", &code),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(summary.contains("<strong>write</strong> access to <strong>main</strong>"));
        assert!(summary.contains("as actor <strong>example-actor</strong>"));
        assert!(headers["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("form-action 'self' https://client.example.com;"));

        let (status, headers, _) = post(
            &test,
            &[
                ("pending", &pending),
                ("csrf", &csrf),
                ("action", "approve"),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let target = location(&headers);
        let issued = param(&target, "code").expect("a code");
        assert!(issued.starts_with("okfac_"));
        assert_eq!(param(&target, "state").as_deref(), Some("xyz"));
        assert_eq!(param(&target, "iss").as_deref(), Some(PUBLIC_URL));

        // The grant exists, is in the key cache, and the code is waiting.
        let oauth = test.app.oauth().expect("oauth on");
        let waiting = oauth
            .codes
            .update(&code_key(&issued), Instant::now(), |code| code.clone())
            .expect("the code is stored");
        assert!(!waiting.used);
        assert!(pkce::verify(&waiting.code_challenge, VERIFIER));
        assert_eq!(waiting.redirect_uri, REDIRECT_URI);
        assert_eq!(waiting.resource, oauth.settings.resource);
        let principal = test
            .app
            .keys()
            .find_grant(&waiting.grant_id)
            .expect("cached grant");
        assert_eq!(principal.actor, "example-actor");
        assert_eq!(principal.access, Access::Write);
        let rows = key_rows::list(test.app.pool()).await.unwrap();
        let grant = rows.iter().find(|row| row.id == waiting.grant_id).unwrap();
        assert_eq!(grant.kind, KeyKind::OAuth);
        assert_eq!(grant.client_id.as_deref(), Some(CLIENT_ID));
        // The sign-in code is spent.
        let live = code_rows::find_live(test.app.pool(), &code, test.app.clock().now())
            .await
            .unwrap();
        assert!(live.is_none());

        // The pending request is spent: a second approve finds nothing.
        let (status, _, _) = post(
            &test,
            &[
                ("pending", &pending),
                ("csrf", &csrf),
                ("action", "approve"),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_sign_in_code_is_consumed_only_on_approve() {
        let test = oauth_app().await;
        let code = sign_in_code(&test, Access::Read).await;
        let (pending, csrf) = start(&test).await;
        let (status, _, _) = post(
            &test,
            &[
                ("pending", &pending),
                ("csrf", &csrf),
                ("action", "code"),
                ("sign_in_code", &code),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        let (status, headers, _) = post(
            &test,
            &[("pending", &pending), ("csrf", &csrf), ("action", "deny")],
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        assert_eq!(
            param(&location(&headers), "error").as_deref(),
            Some("access_denied")
        );

        // Step 2 then Deny left the code live and made no grant.
        let live = code_rows::find_live(test.app.pool(), &code, test.app.clock().now())
            .await
            .unwrap();
        assert!(live.is_some(), "the code survives a deny");
        assert_eq!(oauth_grants(&test).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deny_redirects_with_access_denied() {
        let test = oauth_app().await;
        let (pending, csrf) = start(&test).await;
        let (status, headers, _) = post(
            &test,
            &[("pending", &pending), ("csrf", &csrf), ("action", "deny")],
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let target = location(&headers);
        assert!(target.as_str().starts_with(REDIRECT_URI));
        assert_eq!(param(&target, "error").as_deref(), Some("access_denied"));
        assert_eq!(param(&target, "state").as_deref(), Some("xyz"));
        assert_eq!(param(&target, "iss").as_deref(), Some(PUBLIC_URL));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_wrong_csrf_token_is_refused_and_keeps_the_request() {
        let test = oauth_app().await;
        let code = sign_in_code(&test, Access::Read).await;
        let (pending, csrf) = start(&test).await;
        for forged in ["forged", "", &csrf[..csrf.len() - 1]] {
            for action in ["deny", "code", "approve"] {
                let (status, headers, page) = post(
                    &test,
                    &[
                        ("pending", &pending),
                        ("csrf", forged),
                        ("action", action),
                        ("sign_in_code", &code),
                    ],
                )
                .await;
                assert_eq!(status, StatusCode::BAD_REQUEST, "{action} {forged:?}");
                assert!(headers.get("location").is_none());
                assert!(page.contains("Cannot connect"));
            }
        }
        // The real form still works: a forger cannot cancel someone's request.
        let (status, _, _) = post(
            &test,
            &[("pending", &pending), ("csrf", &csrf), ("action", "deny")],
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn another_requests_csrf_token_does_not_work() {
        let test = oauth_app().await;
        let (first, _) = start(&test).await;
        let (_, second_csrf) = start(&test).await;
        let (status, headers, _) = post(
            &test,
            &[
                ("pending", &first),
                ("csrf", &second_csrf),
                ("action", "deny"),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(headers.get("location").is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn five_bad_codes_end_the_request() {
        let test = oauth_app().await;
        let (pending, csrf) = start(&test).await;
        let bad = [
            ("pending", pending.as_str()),
            ("csrf", csrf.as_str()),
            ("action", "code"),
            ("sign_in_code", "okfsc_wrong"),
        ];
        for attempt in 1..=4 {
            let (status, _, page) = post(&test, &bad).await;
            assert_eq!(status, StatusCode::OK, "attempt {attempt}");
            assert!(page.contains("That sign-in code is not valid"));
        }
        let (status, _, page) = post(&test, &bad).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(page.contains("Too many"));
        // Even a good code no longer helps: the request is gone.
        let code = sign_in_code(&test, Access::Read).await;
        let (status, _, _) = post(
            &test,
            &[
                ("pending", &pending),
                ("csrf", &csrf),
                ("action", "code"),
                ("sign_in_code", &code),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn consent_posts_are_rate_limited() {
        let test = oauth_app().await;
        let (pending, csrf) = start(&test).await;
        let oauth = test.app.oauth().expect("oauth on");
        // Drain the bucket at an instant a minute ahead: the handler's own
        // (earlier) instants then refill nothing, so the test is exact.
        let later = Instant::now() + Duration::from_secs(60);
        while oauth.consent_limit.try_take(later) {}
        let (status, headers, page) = post(
            &test,
            &[("pending", &pending), ("csrf", &csrf), ("action", "deny")],
        )
        .await;
        assert_eq!(status, StatusCode::TOO_MANY_REQUESTS);
        assert!(headers.get("location").is_none());
        assert!(page.contains("Too many sign-in attempts"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn approve_without_an_accepted_code_goes_back_to_step_one() {
        let test = oauth_app().await;
        let (pending, csrf) = start(&test).await;
        let (status, _, page) = post(
            &test,
            &[
                ("pending", &pending),
                ("csrf", &csrf),
                ("action", "approve"),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains(r#"id="sign-in-code""#));
        assert_eq!(oauth_grants(&test).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_code_redeemed_elsewhere_meanwhile_is_refused_at_approve() {
        let test = oauth_app().await;
        let code = sign_in_code(&test, Access::Read).await;
        let (first, first_csrf) = start(&test).await;
        let (second, second_csrf) = start(&test).await;
        for (pending, csrf) in [(&first, &first_csrf), (&second, &second_csrf)] {
            let (status, _, _) = post(
                &test,
                &[
                    ("pending", pending),
                    ("csrf", csrf),
                    ("action", "code"),
                    ("sign_in_code", &code),
                ],
            )
            .await;
            assert_eq!(status, StatusCode::OK);
        }
        let (status, _, _) = post(
            &test,
            &[
                ("pending", &first),
                ("csrf", &first_csrf),
                ("action", "approve"),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let (status, _, page) = post(
            &test,
            &[
                ("pending", &second),
                ("csrf", &second_csrf),
                ("action", "approve"),
            ],
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("That sign-in code is not valid"));
        assert_eq!(oauth_grants(&test).await, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_malformed_form_is_an_error_page() {
        let test = oauth_app().await;
        for body in ["action=code", "pending=x&csrf=y&action=dance"] {
            let (status, headers, page) =
                send(&test, "POST", "/oauth/authorize", Some(body.into())).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{body}");
            assert!(headers.get("location").is_none());
            assert!(page.contains("Cannot connect"));
        }
    }
}

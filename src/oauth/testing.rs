//! Test helpers for the OAuth routes (`#[cfg(test)]`): an app with OAuth on,
//! a registered DCR client, a live sign-in code, and a request helper over
//! the whole router. Tasks 5 and 6 share them.

use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{header, HeaderMap, Request, StatusCode};
use chrono::{TimeDelta, Utc};
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

use crate::http::api::testing::{Browser, MAIN};
use crate::http::console_session::COOKIE;
use crate::http::router;
use crate::mcp::testing::{app_with, TestApp};
use crate::store::keys::{Access, Grants};
use crate::store::oauth::clients::ClientKind;
use crate::store::oauth::codes::{self as code_rows, insert_test_client, NewSignInCode};
use crate::store::oauth::testing::oauth_config;

/// The public URL the test apps run under.
pub(crate) const PUBLIC_URL: &str = "https://okf.example.com";
/// The resource every valid request names.
pub(crate) const RESOURCE: &str = "https://okf.example.com/mcp";
/// The test DCR client's id.
pub(crate) const CLIENT_ID: &str = "test-client";
/// The test client's one redirect URI.
pub(crate) const REDIRECT_URI: &str = "https://client.example.com/callback";
/// A valid S256 challenge (RFC 7636 appendix B, a published vector).
pub(crate) const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
/// The verifier of [`CHALLENGE`].
pub(crate) const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";

/// An app holding `main` and `other` with OAuth on at [`PUBLIC_URL`], and
/// the DCR client [`CLIENT_ID`] (named "Example Client") registered.
///
/// # Returns
/// The app and its database.
///
/// # Panics
/// If the database or the insert fails: a broken test setup.
pub(crate) async fn oauth_app() -> TestApp {
    let test = app_with(
        &[("main", MAIN), ("other", &[("index.md", "# Other\n")])],
        oauth_config(PUBLIC_URL),
    )
    .await;
    // Corrected (issue #247): `insert_dcr` lets Postgres pick a UUID, so the
    // fixed test id goes in through the store's plain-SQL helper.
    insert_test_client(test.app.pool(), CLIENT_ID, ClientKind::Dcr, REDIRECT_URI).await;
    test
}

/// A live sign-in code: `access` on `main`, actor `example-actor`.
///
/// # Arguments
/// * `test` - the app.
/// * `access` - `Read` or `Write`.
///
/// # Returns
/// The `okfsc_` code.
///
/// # Panics
/// If the store refuses the code.
pub(crate) async fn sign_in_code(test: &TestApp, access: Access) -> String {
    let catalogs = ["main".to_string()];
    let new = NewSignInCode {
        name: "example",
        access,
        grants: Grants::Catalogs(&catalogs),
        default_catalog: Some("main"),
        actor: "example-actor",
        expires_at: Utc::now() + TimeDelta::hours(24),
        user: crate::db::TEST_USER,
    };
    code_rows::create(test.app.pool(), &new, Utc::now())
        .await
        .expect("create a sign-in code")
        .code
}

/// A valid authorization request's query string on the sign-in code path
/// (`signin=code`; the client's `state` is `xyz`).
///
/// # Returns
/// The encoded query, without the `?`.
pub(crate) fn authorize_query() -> String {
    format!("{}&signin=code", user_authorize_query())
}

/// A valid authorization request's query string for a signed-in person:
/// [`authorize_query`] without `signin=code`. Its pairs are in the order
/// the server rebuilds a request in, so it is also `/signin`'s `return_to`.
///
/// # Returns
/// The encoded query, without the `?`.
pub(crate) fn user_authorize_query() -> String {
    form(&[
        ("response_type", "code"),
        ("client_id", CLIENT_ID),
        ("redirect_uri", REDIRECT_URI),
        ("state", "xyz"),
        ("code_challenge", CHALLENGE),
        ("code_challenge_method", "S256"),
        ("resource", RESOURCE),
    ])
}

/// `application/x-www-form-urlencoded` text of `pairs`.
///
/// # Arguments
/// * `pairs` - `(name, value)` pairs, in order.
///
/// # Returns
/// The encoded text.
pub(crate) fn form(pairs: &[(&str, &str)]) -> String {
    form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish()
}

/// Sends one request through the whole router.
///
/// # Arguments
/// * `test` - the app.
/// * `method` - `GET` or `POST`.
/// * `uri` - the path and query.
/// * `body` - a form body (sent as `application/x-www-form-urlencoded`), if any.
///
/// # Returns
/// The status, the headers and the body as text.
///
/// # Panics
/// If the request cannot be built or the body cannot be read.
pub(crate) async fn send(
    test: &TestApp,
    method: &str,
    uri: &str,
    body: Option<String>,
) -> (StatusCode, HeaderMap, String) {
    send_as(test, method, uri, body, None).await
}

/// As [`send`], with a console session's cookie when `session` is given.
///
/// # Arguments
/// * `test` - the app.
/// * `method` - `GET` or `POST`.
/// * `uri` - the path and query.
/// * `body` - a form body, if any.
/// * `session` - a signed-in browser (Task 7a's `Browser`) whose session
///   cookie to send, if any.
///
/// # Returns
/// The status, the headers and the body as text.
///
/// # Panics
/// If the request cannot be built or the body cannot be read.
pub(crate) async fn send_as(
    test: &TestApp,
    method: &str,
    uri: &str,
    body: Option<String>,
    session: Option<&Browser>,
) -> (StatusCode, HeaderMap, String) {
    let mut request = Request::builder().method(method).uri(uri);
    if body.is_some() {
        request = request.header(header::CONTENT_TYPE, "application/x-www-form-urlencoded");
    }
    // Only the cookie: these are top-level navigations and form posts,
    // which carry no CSRF header (the consent form has its own token).
    if let Some(id) = session.and_then(|browser| browser.cookie(COOKIE)) {
        request = request.header(header::COOKIE, format!("{COOKIE}={id}"));
    }
    let request = request
        .body(body.map_or_else(Body::empty, Body::from))
        .expect("request");
    let response = router(Arc::clone(&test.app), &CancellationToken::new())
        .oneshot(request)
        .await
        .expect("response");
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 1 << 20).await.expect("body");
    (
        status,
        headers,
        String::from_utf8_lossy(&bytes).into_owned(),
    )
}

/// The `value` of the hidden input `name` in a consent page.
///
/// # Arguments
/// * `page` - the page's HTML.
/// * `name` - the input's name.
///
/// # Returns
/// The value, as written (escaped).
///
/// # Panics
/// If the page has no such input.
pub(crate) fn hidden(page: &str, name: &str) -> String {
    let marker = format!("name=\"{name}\" value=\"");
    let start = page.find(&marker).expect("hidden input") + marker.len();
    let end = start + page[start..].find('"').expect("closing quote");
    page[start..end].to_string()
}

/// Every hidden input of the form `form_id` in a consent page, in order:
/// what a browser would post from that form, minus its buttons.
///
/// # Arguments
/// * `page` - the page's HTML.
/// * `form_id` - the form's `id` attribute.
///
/// # Returns
/// `(name, value)` pairs, as written (escaped).
///
/// # Panics
/// If the page has no such form.
pub(crate) fn hidden_inputs(page: &str, form_id: &str) -> Vec<(String, String)> {
    let start = page.find(&format!("id=\"{form_id}\"")).expect("the form");
    let form = &page[start..];
    let form = &form[..form.find("</form>").expect("the form's end")];
    form.match_indices("<input type=\"hidden\" name=\"")
        .map(|(at, marker)| {
            let rest = &form[at + marker.len()..];
            let (name, rest) = rest.split_once("\" value=\"").expect("a value");
            let (value, _) = rest.split_once('"').expect("closing quote");
            (name.to_string(), value.to_string())
        })
        .collect()
}

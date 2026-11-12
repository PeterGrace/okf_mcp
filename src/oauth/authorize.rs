//! `GET` and `POST /oauth/authorize` (spec "Endpoints" and "Consent flow").
//!
//! The GET validates the request in a fixed order. Until the client and its
//! exact redirect URI are known, a failure is an error page, never a
//! redirect (no open redirect). After that, failures go back to the client
//! as `error=...`. A valid request becomes a pending authorization in
//! memory, and the POSTs walk it through the sign-in code and consent.
//!
//! Who approves (spec "OAuth consent as the user"): with `signin=code`, the
//! sign-in code path (step 1 and 2), whose grants stay admin-issued. Else a
//! signed-in person (the console session cookie) chooses catalogs within
//! their ceiling, and the approval is a self-issued grant. Without a
//! session, the GET sends the browser to `/signin` first, before anything
//! is stored.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Instant;

use axum::extract::rejection::{FormRejection, QueryRejection};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Form, Router};
use serde::Deserialize;
use subtle::ConstantTimeEq;

use crate::app::App;
use crate::error::OkfError;
use crate::http::console_session::{cookie, signed_in_user, COOKIE};
use crate::http::signin::cross_site;
use crate::keys;
use crate::maintenance::types::new_id;
use crate::oauth::clients;
use crate::oauth::consent::{self, GrantSummary, PageView, UserPage};
use crate::oauth::error::OAuthError;
use crate::oauth::state::{
    code_key, AcceptedCode, AuthorizationCode, PendingAuthorization, UserConsent,
    MAX_FAILED_ATTEMPTS,
};
use crate::oauth::{pkce, OAuth};
use crate::store::ceiling::{self, Ceiling};
use crate::store::keys::Access;
use crate::store::oauth::clients::{ClientKind, ClientRecord};
use crate::store::oauth::codes::{self as code_rows, SignInCodeRecord};
use crate::store::oauth::grants::{self as grant_rows, NewSelfIssuedGrant};
use crate::store::users::UserAuth;

/// The message for every unusable sign-in code, so the page never says
/// whether a code exists, expired, was redeemed or was revoked.
const BAD_CODE: &str = "That sign-in code is not valid. Check it, or ask for a new one.";
/// The message for an Approve from a step-2 page that a later code entry
/// (in another copy of the form) replaced.
const STALE: &str =
    "That approval page was out of date. Approve from the latest page, or enter the code again.";
/// The message for an unknown, expired or forged pending authorization.
const EXPIRED: &str = "This sign-in request has expired or is not valid.";
/// The message for a consent the page could not have produced (a catalog
/// or level it did not offer, or an unknown field).
const TAMPERED: &str =
    "That approval did not match what was offered. Start again from your client.";
/// The message for a consent posted after its user signed out or switched.
const SIGNED_OUT: &str =
    "You are no longer signed in as the person this page was for. Start again from your client.";
/// The message for a consent whose access shrank while the page was open.
const CHANGED: &str = "Your access changed while this page was open. Start again from your client.";
/// The message for a consent with no catalog ticked.
const NOTHING_CHOSEN: &str = "Choose at least one catalog, or Deny.";
/// The message for a default catalog that is not ticked.
const DEFAULT_NOT_CHOSEN: &str = "The default catalog must be one you allow.";
/// A form posted from another site (403).
const CROSS_SITE: &str = "This form can only be sent from this site. Start again from your client.";

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
    /// `code` asks for the sign-in code path (step 1) even with a session;
    /// the "Use a sign-in code instead" links set it.
    signin: Option<String>,
    // `scope` is accepted and ignored (serde skips unknown fields): the
    // sign-in code or the person's choice decides the grant.
}

/// The consent form's fields.
// `Debug` by hand: the pending id, the CSRF token, the sign-in code and the
// consent nonce never reach a log.
#[derive(Deserialize)]
struct ConsentForm {
    pending: String,
    csrf: String,
    action: String,
    sign_in_code: Option<String>,
    /// The step-2 page's token (see [`AcceptedCode`]), or the signed-in
    /// page's consent nonce (see [`UserConsent`]); absent on step 1.
    summary: Option<String>,
    /// The signed-in page's default catalog select (empty: none).
    default_catalog: Option<String>,
    /// Every other field: the signed-in page's `grant.<catalog>` checkboxes
    /// and `level.<catalog>` selects. `flatten` collects the fields not
    /// named above into this map, like `**kwargs`.
    #[serde(flatten)]
    choices: BTreeMap<String, String>,
}

impl std::fmt::Debug for ConsentForm {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConsentForm")
            .field("pending", &"<redacted>")
            .field("action", &self.action)
            .finish_non_exhaustive()
    }
}

/// What an Approve finds, decided under the pending map's lock.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Approval {
    /// No code has been accepted yet: back to step 1.
    NoCode,
    /// The form came from a step-2 page that a later code entry replaced.
    Stale,
    /// The form is the current summary's: redeem this code id.
    Current(String),
}

/// Decides an Approve against the accepted code, comparing the posted
/// summary token in constant time.
///
/// # Arguments
/// * `accepted` - the pending request's accepted code, if any.
/// * `posted` - the form's `summary` field, if any.
///
/// # Returns
/// [`Approval::Current`] only when `posted` is that code's summary token.
fn approval(accepted: Option<&AcceptedCode>, posted: Option<&str>) -> Approval {
    let Some(accepted) = accepted else {
        return Approval::NoCode;
    };
    // `is_some_and`: `posted is not None and <predicate>`.
    let current = posted.is_some_and(|posted| {
        accepted
            .summary_nonce
            .as_bytes()
            .ct_eq(posted.as_bytes())
            .into()
    });
    if current {
        Approval::Current(accepted.code_id.clone())
    } else {
        Approval::Stale
    }
}

/// Spends a signed-in page's consent nonce: when `posted` is the current
/// nonce, it is replaced by `fresh`, so the same page (or a replay of its
/// form) cannot approve again. Runs under the pending map's lock.
///
/// # Arguments
/// * `consent` - the pending request's consent, if it is on the user path.
/// * `posted` - the form's `summary` field, if any.
/// * `fresh` - an unseen random token to put in its place.
///
/// # Returns
/// Whether the nonce matched and was spent.
fn claim_consent(consent: Option<&mut UserConsent>, posted: Option<&str>, fresh: String) -> bool {
    let Some(consent) = consent else {
        return false;
    };
    let current =
        posted.is_some_and(|posted| consent.nonce.as_bytes().ct_eq(posted.as_bytes()).into());
    if current {
        consent.nonce = fresh;
    }
    current
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
        user: record.user.clone(),
        default_catalog: record.default_catalog.clone(),
    }
}

/// A request's validated parameters as a query string, in a fixed order and
/// without `signin`: what `/signin` returns to and the sign-in code link
/// extends. Rebuilt rather than copied from the request, so it carries only
/// visible ASCII and passes `pages::safe_return_to`.
///
/// # Arguments
/// * `query` - the request, already validated.
///
/// # Returns
/// The encoded query, without the `?`.
fn canonical_query(query: &AuthorizeQuery) -> String {
    let fields = [
        ("response_type", &query.response_type),
        ("client_id", &query.client_id),
        ("redirect_uri", &query.redirect_uri),
        ("state", &query.state),
        ("code_challenge", &query.code_challenge),
        ("code_challenge_method", &query.code_challenge_method),
        ("resource", &query.resource),
    ];
    // `filter_map` drops the absent ones (only `state` can be, by now).
    form_urlencoded::Serializer::new(String::new())
        .extend_pairs(
            fields
                .iter()
                .filter_map(|(name, value)| value.as_deref().map(|value| (*name, value))),
        )
        .finish()
}

/// The sign-in code path's URL for a request's [`canonical_query`].
fn code_href(query: &str) -> String {
    format!("/oauth/authorize?{query}&signin=code")
}

/// The "Use a sign-in code instead" link for an authorize URL.
///
/// # Arguments
/// * `return_to` - a same-origin URL, such as `/signin`'s sanitised
///   `return_to`.
///
/// # Returns
/// The URL with `signin=code` added, or `None` when it is not an authorize
/// URL (so `/signin` shows no link for console sign-ins).
pub(crate) fn code_path_href(return_to: &str) -> Option<String> {
    // `strip_prefix` is Python's `removeprefix`, returning `None` when the
    // prefix is absent.
    return_to.strip_prefix("/oauth/authorize?").map(code_href)
}

/// A 303 to `/signin`, which comes back to this authorize URL after
/// sign-in. Nothing is stored: the GET simply runs again.
///
/// # Arguments
/// * `query` - the request's [`canonical_query`].
///
/// # Returns
/// The redirect.
fn to_sign_in(query: &str) -> Response {
    let return_to = format!("/oauth/authorize?{query}");
    // `byte_serialize` percent-encodes for a query value, like Python's
    // `urllib.parse.quote_plus`.
    let encoded: String = form_urlencoded::byte_serialize(return_to.as_bytes()).collect();
    Redirect::to(&format!("/signin?return_to={encoded}")).into_response()
}

/// Why a signed-in consent's choices were refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Choice {
    /// Nothing was ticked: show the form again.
    Empty,
    /// Something the page never offered: refuse the request.
    Tampered,
}

/// The catalogs and levels a signed-in consent chose, checked against what
/// its page offered (the posted form is never trusted).
///
/// # Arguments
/// * `choices` - the form's `grant.<catalog>` and `level.<catalog>` fields.
/// * `offered` - the ceiling the page showed.
///
/// # Returns
/// Each ticked catalog's level.
///
/// # Errors
/// [`Choice::Empty`] when nothing is ticked; [`Choice::Tampered`] for a
/// catalog not offered, a level above the offer or unknown, a ticked
/// catalog without a level, a checkbox value other than `on`, or any
/// other field.
fn chosen_levels(
    choices: &BTreeMap<String, String>,
    offered: &Ceiling,
) -> Result<BTreeMap<String, Access>, Choice> {
    let mut chosen = BTreeMap::new();
    for (field, value) in choices {
        if let Some(catalog) = field.strip_prefix("grant.") {
            let allowed = *offered.get(catalog).ok_or(Choice::Tampered)?;
            let level = choices
                .get(&format!("level.{catalog}"))
                .and_then(|level| Access::parse(level))
                .ok_or(Choice::Tampered)?;
            if value != "on" || level > allowed {
                return Err(Choice::Tampered);
            }
            chosen.insert(catalog.to_string(), level);
        } else if let Some(catalog) = field.strip_prefix("level.") {
            // Selects post even when unticked; they must still be offered
            // ones, with a level the page could show.
            let allowed = *offered.get(catalog).ok_or(Choice::Tampered)?;
            if Access::parse(value).is_none_or(|level| level > allowed) {
                return Err(Choice::Tampered);
            }
        } else {
            return Err(Choice::Tampered);
        }
    }
    if chosen.is_empty() {
        Err(Choice::Empty)
    } else {
        Ok(chosen)
    }
}

/// Renders the signed-in consent page for a pending authorization.
///
/// # Arguments
/// * `pending_id`, `pending` - the request; its `consent` is set.
/// * `user` - the signed-in user's name.
/// * `error` - a message, if the page is shown again.
///
/// # Returns
/// The page, or the expired page if `pending` is on the code path.
fn user_consent_page(
    pending_id: &str,
    pending: &PendingAuthorization,
    user: &str,
    error: Option<&str>,
) -> Response {
    let Some(consent) = pending.consent.as_ref() else {
        return consent::error_page(StatusCode::BAD_REQUEST, EXPIRED);
    };
    consent::user_page(
        &view(pending_id, pending),
        &UserPage {
            user,
            ceiling: &consent.offered,
            nonce: &consent.nonce,
            code_href: &code_href(&pending.query),
        },
        error,
    )
}

/// The consent a signed-in person's page starts with: their ceiling now
/// and a fresh nonce.
///
/// # Errors
/// [`OkfError::UnknownName`] once the user is gone; [`OkfError::Io`] if
/// the OS random source fails; [`OkfError::Database`].
async fn new_consent(app: &App, user: &UserAuth) -> Result<UserConsent, OkfError> {
    Ok(UserConsent {
        user_id: user.id.clone(),
        nonce: keys::generate_with("")?,
        offered: ceiling::ceiling_of(app.pool(), &user.name).await?,
    })
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
        Err(error) => {
            // The page stays generic; the log says why, so an operator can
            // tell an unknown DCR id from a document this server refuses
            // (issue #273). A client id is not a secret.
            tracing::warn!(client_id, reason = %error, "refused an OAuth client");
            return Err((
                StatusCode::BAD_REQUEST,
                "This client is not known to the server.",
            ));
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

/// `GET /oauth/authorize`: validates the request, then shows step 1 (the
/// sign-in code path), the signed-in consent page, or sends the browser to
/// `/signin`.
async fn authorize(
    State(app): State<Arc<App>>,
    headers: HeaderMap,
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
    // 4. Who approves (spec "OAuth consent as the user"): the sign-in code
    // path when asked for, else the signed-in person, else /signin. This
    // comes after every check that can fail, and before anything is
    // stored: an anonymous GET leaves nothing behind.
    let query_string = canonical_query(&query);
    let user = if query.signin.as_deref() == Some("code") {
        None
    } else {
        match signed_in_user(&app, &headers).await {
            Ok(Some(user)) => Some(user),
            Ok(None) => return to_sign_in(&query_string),
            Err(error) => return server_error(&error),
        }
    };
    let consent = match &user {
        Some(user) => match new_consent(&app, user).await {
            Ok(consent) => Some(consent),
            // Deleted since the session was read: as if signed out.
            Err(OkfError::UnknownName { .. }) => return to_sign_in(&query_string),
            Err(error) => return server_error(&error),
        },
        None => None,
    };
    // 5. A pending authorization, and its first page.
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
        accepted: None,
        failed_attempts: 0,
        consent,
        query: query_string,
    };
    let page = match &user {
        Some(user) => user_consent_page(&pending_id, &pending, &user.name, None),
        None => consent::code_page(&view(&pending_id, &pending), None),
    };
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
    headers: HeaderMap,
    form: Result<Form<ConsentForm>, FormRejection>,
) -> Response {
    // OAuth off: the routes are not mounted then, so this is only a guard.
    let Some(oauth) = app.oauth() else {
        return StatusCode::NOT_FOUND.into_response();
    };
    // Another site's page posting this form into the person's browser
    // (with its own pending id and token) is refused before anything else.
    if cross_site(&headers) {
        return consent::error_page(StatusCode::FORBIDDEN, CROSS_SITE);
    }
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
    // A signed-in Approve spends its page's nonce; the replacement is made
    // here, outside the lock, because making it can fail.
    let mut fresh = if form.action == "consent" {
        match keys::generate_with("") {
            Ok(nonce) => Some(nonce),
            Err(error) => return server_error(&error),
        }
    } else {
        None
    };
    // The CSRF token is checked before anything else, and a mismatch leaves
    // the pending authorization alone: someone who learned its id cannot
    // cancel it. Under the same lock (`update` holds it only for the
    // closure), an Approve's summary token is matched against the code it
    // was rendered for, so the code id and its token come from one snapshot,
    // and a signed-in Approve's nonce is spent, so it approves once.
    let pending = oauth.pending.update(&form.pending, now, |pending| {
        let matches: bool = pending.csrf.as_bytes().ct_eq(form.csrf.as_bytes()).into();
        matches.then(|| {
            let decided = approval(pending.accepted.as_ref(), form.summary.as_deref());
            // `take` moves the token out of the `Option`, leaving `None`.
            let claimed = fresh.take().is_some_and(|fresh| {
                claim_consent(pending.consent.as_mut(), form.summary.as_deref(), fresh)
            });
            (pending.clone(), decided, claimed)
        })
    });
    // `Some(Some(_))`: the entry is live (outer) and the token matched (inner).
    let Some(Some((pending, decided, claimed))) = pending else {
        return consent::error_page(StatusCode::BAD_REQUEST, EXPIRED);
    };
    // The sign-in code steps belong to the code path only: on the user
    // path, the code is reached through the link, a fresh GET.
    let code_path = pending.consent.is_none();
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
        "code" if code_path => {
            let presented = form.sign_in_code.as_deref().unwrap_or_default();
            check_code(&app, oauth, &form.pending, &pending, presented).await
        }
        "approve" if code_path => match decided {
            Approval::NoCode => consent::code_page(&view(&form.pending, &pending), None),
            // Nothing changes: the latest summary still approves.
            Approval::Stale => consent::code_page(&view(&form.pending, &pending), Some(STALE)),
            Approval::Current(code_id) => {
                approve(&app, oauth, &form.pending, &pending, &code_id).await
            }
        },
        // `claimed`: on the user path, with this page's current nonce.
        "consent" if claimed => consent_as_user(&app, oauth, &form, &pending, &headers).await,
        _ => consent::error_page(StatusCode::BAD_REQUEST, EXPIRED),
    }
}

/// Counts a failed attempt and answers step 1 again, or drops the request
/// at [`MAX_FAILED_ATTEMPTS`].
fn failed_attempt(oauth: &OAuth, pending_id: &str, pending: &PendingAuthorization) -> Response {
    let attempts = oauth.pending.update(pending_id, Instant::now(), |pending| {
        pending.failed_attempts = pending.failed_attempts.saturating_add(1);
        pending.accepted = None;
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

/// `action=code`: checks the code without consuming it, and shows step 2
/// with a fresh summary token that replaces any earlier summary's.
async fn check_code(
    app: &App,
    oauth: &OAuth,
    pending_id: &str,
    pending: &PendingAuthorization,
    presented: &str,
) -> Response {
    match code_rows::find_live(app.pool(), presented, app.clock().now()).await {
        Ok(Some(record)) => {
            let summary_nonce = match keys::generate_with("") {
                Ok(nonce) => nonce,
                Err(error) => return server_error(&error),
            };
            let accepted = AcceptedCode {
                code_id: record.id.clone(),
                summary_nonce: summary_nonce.clone(),
            };
            let stored = oauth.pending.update(pending_id, Instant::now(), |pending| {
                pending.accepted = Some(accepted);
            });
            if stored.is_none() {
                return consent::error_page(StatusCode::BAD_REQUEST, EXPIRED);
            }
            consent::summary_page(
                &view(pending_id, pending),
                &summary(&record),
                &summary_nonce,
            )
        }
        Ok(None) => failed_attempt(oauth, pending_id, pending),
        Err(error) => server_error(&error),
    }
}

/// `action=approve` from the current summary: redeems its code into a
/// grant and [`finish`]es.
async fn approve(
    app: &App,
    oauth: &OAuth,
    pending_id: &str,
    pending: &PendingAuthorization,
    code_id: &str,
) -> Response {
    let grant_id =
        match code_rows::redeem(app.pool(), code_id, &pending.client, app.clock().now()).await {
            Ok(grant_id) => grant_id,
            // Expired, revoked or redeemed meanwhile (another approve won).
            Err(OkfError::Refused { .. }) => return failed_attempt(oauth, pending_id, pending),
            Err(error) => return server_error(&error),
        };
    finish(app, oauth, pending_id, pending, grant_id).await
}

/// Ends an approved request: drops the pending authorization, loads the
/// new grant into the key cache, mints an authorization code bound to it
/// and sends the browser back to the client. Both paths end here.
///
/// # Arguments
/// * `app`, `oauth` - the application and its OAuth state.
/// * `pending_id`, `pending` - the request.
/// * `grant_id` - the grant the approval created.
///
/// # Returns
/// A 303 to the client with `code`, or an error page.
async fn finish(
    app: &App,
    oauth: &OAuth,
    pending_id: &str,
    pending: &PendingAuthorization,
    grant_id: String,
) -> Response {
    // The request is spent once the grant exists, whatever happens next.
    oauth.pending.remove(pending_id);
    // Creating the grant bumped admin_generation, so this loads it into the
    // key cache before the client's token request can arrive.
    if let Err(error) = app.poll().await {
        tracing::error!(%error, "poll after creating a grant failed");
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
            replayed: false,
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

/// `action=consent`: a signed-in person's approval, its nonce already spent
/// by [`claim_consent`]. It checks that the cookie is still that person's,
/// refuses any choice the page did not offer, and creates the self-issued
/// grant, which re-checks the session and the ceiling under the user's
/// row lock. Then it [`finish`]es as the code path does.
///
/// # Arguments
/// * `app`, `oauth` - the application and its OAuth state.
/// * `form` - the posted form.
/// * `pending` - the request, holding the replacement nonce.
/// * `headers` - the request's headers (the session cookie).
///
/// # Returns
/// A 303 to the client, the page again (nothing or a bad default chosen),
/// or an error page.
async fn consent_as_user(
    app: &App,
    oauth: &OAuth,
    form: &ConsentForm,
    pending: &PendingAuthorization,
    headers: &HeaderMap,
) -> Response {
    let Some(consent) = pending.consent.as_ref() else {
        return consent::error_page(StatusCode::BAD_REQUEST, EXPIRED);
    };
    // Every refusal below ends the request: the page cannot be trusted again.
    let refuse = |message| {
        oauth.pending.remove(&form.pending);
        consent::error_page(StatusCode::BAD_REQUEST, message)
    };
    let Some(session_id) = cookie(headers, COOKIE) else {
        return refuse(SIGNED_OUT);
    };
    let user = match signed_in_user(app, headers).await {
        Ok(Some(user)) if user.id == consent.user_id => user,
        Ok(_) => return refuse(SIGNED_OUT),
        Err(error) => return server_error(&error),
    };
    let chosen = match chosen_levels(&form.choices, &consent.offered) {
        Ok(chosen) => chosen,
        Err(Choice::Empty) => {
            return user_consent_page(&form.pending, pending, &user.name, Some(NOTHING_CHOSEN))
        }
        Err(Choice::Tampered) => {
            tracing::warn!(
                user = %user.name,
                client = %pending.client.client_id,
                "refused a consent outside the offered choices"
            );
            return refuse(TAMPERED);
        }
    };
    let default_catalog = form
        .default_catalog
        .as_deref()
        .filter(|name| !name.is_empty());
    if default_catalog.is_some_and(|name| !chosen.contains_key(name)) {
        return user_consent_page(&form.pending, pending, &user.name, Some(DEFAULT_NOT_CHOSEN));
    }
    let new = NewSelfIssuedGrant {
        user_id: &user.id,
        session_id,
        client: &pending.client,
        levels: &chosen,
        default_catalog,
    };
    let grant_id = match grant_rows::create_self_issued(app.pool(), &new, app.clock().now()).await {
        Ok(grant_id) => grant_id,
        // Signed out, reset or deleted since the check above.
        Err(OkfError::Unauthenticated { .. }) => return refuse(SIGNED_OUT),
        // The ceiling shrank, or a chosen catalog was deleted, meanwhile.
        Err(OkfError::Refused { .. } | OkfError::CatalogNotSelected { .. }) => {
            return refuse(CHANGED)
        }
        Err(error) => return server_error(&error),
    };
    tracing::info!(
        user = %user.name,
        client = %pending.client.client_id,
        grant = %grant_id,
        "consent granted as the signed-in user"
    );
    finish(app, oauth, &form.pending, pending, grant_id).await
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
    use std::collections::BTreeMap;
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use axum::body::Body;
    use axum::http::{HeaderMap, Request, StatusCode};
    use chrono::Utc;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;

    use super::{chosen_levels, Choice};
    use crate::http::api::testing::Browser;
    use crate::http::console_session::COOKIE;
    use crate::mcp::testing::TestApp;
    use crate::oauth::pkce;
    use crate::oauth::state::code_key;
    use crate::oauth::testing::{
        authorize_query, form, hidden, hidden_inputs, oauth_app, send, send_as, sign_in_code,
        user_authorize_query, CLIENT_ID, PUBLIC_URL, REDIRECT_URI, VERIFIER,
    };
    use crate::store::ceiling::{self, Ceiling};
    use crate::store::keys::{self as key_rows, Access, KeyKind};
    use crate::store::oauth::codes as code_rows;
    use crate::store::user_sessions;
    use crate::store::users::{self, NewUser};

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
        assert!(page.contains("id=\"consent-code-form\""));
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
        assert!(summary.contains(&format!(
            "as <strong>{}</strong> (actor <strong>example-actor</strong>)",
            crate::db::TEST_USER
        )));
        assert!(headers["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("form-action 'self' https://client.example.com;"));

        let (status, headers, _) = approve_from(&test, &summary).await;
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
        let (status, _, _) = approve_from(&test, &summary).await;
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
        let mut summaries = Vec::new();
        for (pending, csrf) in [(&first, &first_csrf), (&second, &second_csrf)] {
            let (status, _, summary) = post(
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
            summaries.push(summary);
        }
        let (status, _, _) = approve_from(&test, &summaries[0]).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let (status, _, page) = approve_from(&test, &summaries[1]).await;
        assert_eq!(status, StatusCode::OK);
        assert!(page.contains("That sign-in code is not valid"));
        assert_eq!(oauth_grants(&test).await, 1);
    }

    /// Posts the Approve button of a step-2 page's form, with exactly the
    /// hidden fields that page carries.
    async fn approve_from(test: &TestApp, summary: &str) -> (StatusCode, HeaderMap, String) {
        let fields = hidden_inputs(summary, "consent-approve-form");
        let mut pairs: Vec<(&str, &str)> = fields
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect();
        pairs.push(("action", "approve"));
        post(test, &pairs).await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_older_summary_cannot_approve_a_code_entered_after_it() {
        let test = oauth_app().await;
        let read_code = sign_in_code(&test, Access::Read).await;
        let write_code = sign_in_code(&test, Access::Write).await;
        let (pending, csrf) = start(&test).await;
        let mut summaries = Vec::new();
        // Two copies of step 1 under one pending request: A, then B.
        for code in [&read_code, &write_code] {
            let (status, _, summary) = post(
                &test,
                &[
                    ("pending", &pending),
                    ("csrf", &csrf),
                    ("action", "code"),
                    ("sign_in_code", code),
                ],
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            summaries.push(summary);
        }
        assert!(summaries[0].contains("<strong>read</strong> access"));
        assert!(summaries[1].contains("<strong>write</strong> access"));

        // A's page said read-only: its Approve must not redeem B (or A).
        let (status, headers, page) = approve_from(&test, &summaries[0]).await;
        assert_eq!(status, StatusCode::OK, "{page}");
        assert!(headers.get("location").is_none());
        assert!(page.contains(r#"id="sign-in-code""#), "back at step 1");
        assert_eq!(oauth_grants(&test).await, 0);
        let now = test.app.clock().now();
        for code in [&read_code, &write_code] {
            let live = code_rows::find_live(test.app.pool(), code, now)
                .await
                .unwrap();
            assert!(live.is_some(), "neither code is redeemed");
        }

        // A crafted Approve with no summary token is refused the same way.
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
        assert!(page.contains("out of date"));
        assert_eq!(oauth_grants(&test).await, 0);

        // B's own, current summary still approves, and grants write.
        let (status, headers, _) = approve_from(&test, &summaries[1]).await;
        assert_eq!(status, StatusCode::SEE_OTHER);
        let issued = param(&location(&headers), "code").expect("a code");
        let oauth = test.app.oauth().expect("oauth on");
        let waiting = oauth
            .codes
            .update(&code_key(&issued), Instant::now(), |code| code.clone())
            .expect("the code is stored");
        let principal = test
            .app
            .keys()
            .find_grant(&waiting.grant_id)
            .expect("cached grant");
        assert_eq!(principal.access, Access::Write);
        let read_live = code_rows::find_live(test.app.pool(), &read_code, now)
            .await
            .unwrap();
        assert!(read_live.is_some(), "A was never redeemed");
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

    // ---- Consent as the signed-in user (Task 11) ----

    /// Creates user `name` with these entitlements and signs it in.
    async fn signed_in(test: &TestApp, name: &str, grants: &[(&str, Access)]) -> Browser {
        users::create(
            test.app.pool(),
            &NewUser {
                name,
                display_name: "",
            },
            Utc::now(),
        )
        .await
        .expect("create the user");
        for (catalog, access) in grants {
            ceiling::entitle(test.app.pool(), name, catalog, *access, Utc::now())
                .await
                .expect("entitle");
        }
        Browser::session_for(test, name).await
    }

    /// The signed-in consent page for `session`.
    async fn user_page(test: &TestApp, session: &Browser) -> String {
        let (status, _, page) = send_as(
            test,
            "GET",
            &format!("/oauth/authorize?{}", user_authorize_query()),
            None,
            Some(session),
        )
        .await;
        assert_eq!(status, StatusCode::OK, "{page}");
        page
    }

    /// The pending id, CSRF token and nonce of a signed-in page, with
    /// `action=consent`, then `choices`.
    fn consent_form(page: &str, choices: &[(&str, &str)]) -> String {
        let (pending, csrf, summary) = (
            hidden(page, "pending"),
            hidden(page, "csrf"),
            hidden(page, "summary"),
        );
        let mut pairs = vec![
            ("pending", pending.as_str()),
            ("csrf", csrf.as_str()),
            ("summary", summary.as_str()),
            ("action", "consent"),
        ];
        pairs.extend_from_slice(choices);
        form(&pairs)
    }

    /// Posts `choices` from `page`'s consent form as `session`.
    async fn consent_as(
        test: &TestApp,
        session: Option<&Browser>,
        page: &str,
        choices: &[(&str, &str)],
    ) -> (StatusCode, HeaderMap, String) {
        send_as(
            test,
            "POST",
            "/oauth/authorize",
            Some(consent_form(page, choices)),
            session,
        )
        .await
    }

    /// The grant behind the authorization code in a 303 to the client.
    fn grant_of(test: &TestApp, headers: &HeaderMap) -> String {
        let issued = param(&location(headers), "code").expect("a code");
        test.app
            .oauth()
            .expect("oauth on")
            .codes
            .update(&code_key(&issued), Instant::now(), |code| {
                code.grant_id.clone()
            })
            .expect("the code is stored")
    }

    const READ_MAIN: &[(&str, &str)] = &[("grant.main", "on"), ("level.main", "read")];

    #[tokio::test(flavor = "multi_thread")]
    async fn without_a_session_authorize_sends_the_person_to_sign_in() {
        let test = oauth_app().await;

        let (status, headers, _) = send(
            &test,
            "GET",
            &format!("/oauth/authorize?{}", user_authorize_query()),
            None,
        )
        .await;

        assert_eq!(status, StatusCode::SEE_OTHER);
        let target = headers["location"].to_str().expect("a location");
        let parsed = url::Url::parse(&format!("{PUBLIC_URL}{target}")).expect("a URL");
        assert_eq!(parsed.path(), "/signin");
        let return_to = param(&parsed, "return_to").expect("a return_to");
        assert_eq!(
            return_to,
            format!("/oauth/authorize?{}", user_authorize_query())
        );
        // It survives `/signin`'s check, so sign-in comes back here.
        assert_eq!(
            crate::http::pages::safe_return_to(Some(&return_to)),
            return_to
        );
        assert_eq!(
            test.app.oauth().expect("oauth on").pending.len(),
            0,
            "nothing is stored before sign-in"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_sign_in_redirect_comes_after_the_request_is_validated() {
        let test = oauth_app().await;
        let bad_redirect = user_authorize_query().replace(
            "client.example.com%2Fcallback",
            "attacker.example.com%2Fcallback",
        );
        let bad_type = user_authorize_query().replace("response_type=code", "response_type=token");

        let (untrusted, untrusted_headers, _) = send(
            &test,
            "GET",
            &format!("/oauth/authorize?{bad_redirect}"),
            None,
        )
        .await;
        let (to_client, client_headers, _) =
            send(&test, "GET", &format!("/oauth/authorize?{bad_type}"), None).await;

        assert_eq!(untrusted, StatusCode::BAD_REQUEST);
        assert!(untrusted_headers.get("location").is_none());
        assert_eq!(to_client, StatusCode::SEE_OTHER);
        let target = location(&client_headers);
        assert!(target.as_str().starts_with(REDIRECT_URI));
        assert_eq!(
            param(&target, "error").as_deref(),
            Some("unsupported_response_type")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_sign_in_code_path_needs_no_session_and_ignores_one() {
        let test = oauth_app().await;
        let alice = signed_in(&test, "alice", &[("main", Access::Read)]).await;
        let uri = format!("/oauth/authorize?{}", authorize_query());

        let (anonymous, _, code_page) = send(&test, "GET", &uri, None).await;
        let (signed, _, also_code_page) = send_as(&test, "GET", &uri, None, Some(&alice)).await;

        assert_eq!(anonymous, StatusCode::OK);
        assert!(code_page.contains("id=\"consent-code-form\""));
        assert_eq!(signed, StatusCode::OK);
        assert!(also_code_page.contains("id=\"consent-code-form\""));
        assert!(!also_code_page.contains("consent-user-form"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_signed_in_person_sees_only_their_ceiling() {
        let test = oauth_app().await;
        let alice = signed_in(&test, "alice", &[("main", Access::Read)]).await;

        let page = user_page(&test, &alice).await;

        assert!(page.contains("id=\"consent-user-form\""));
        assert!(page.contains("<strong id=\"consent-user\">alice</strong>"));
        assert!(page.contains("name=\"grant.main\""));
        assert!(
            !page.contains("grant.other"),
            "other is outside the ceiling"
        );
        assert!(
            !page.contains("<option value=\"write\">"),
            "main is read-only for alice"
        );
        assert!(page.contains("id=\"consent-use-code\""));
        assert!(page.contains(&format!(
            "href=\"/oauth/authorize?{}&amp;signin=code\"",
            user_authorize_query().replace('&', "&amp;")
        )));
        assert!(!hidden(&page, "summary").is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn approving_as_the_person_creates_a_capped_self_issued_grant() {
        let test = oauth_app().await;
        let alice = signed_in(
            &test,
            "alice",
            &[("main", Access::Write), ("other", Access::Read)],
        )
        .await;
        let page = user_page(&test, &alice).await;

        let (status, headers, body) = consent_as(
            &test,
            Some(&alice),
            &page,
            &[
                ("grant.main", "on"),
                ("level.main", "write"),
                ("level.other", "read"),
                ("default_catalog", "main"),
            ],
        )
        .await;

        assert_eq!(status, StatusCode::SEE_OTHER, "{body}");
        let target = location(&headers);
        assert!(target.as_str().starts_with(REDIRECT_URI));
        assert_eq!(param(&target, "state").as_deref(), Some("xyz"));
        let grant = grant_of(&test, &headers);
        let principal = test.app.keys().find_grant(&grant).expect("cached grant");
        assert_eq!(principal.user.as_deref(), Some("alice"));
        assert_eq!(principal.actor, "Example Client");
        assert_eq!(principal.level("main"), Some(Access::Write));
        assert_eq!(principal.level("other"), None, "not ticked");
        assert_eq!(principal.default_catalog.as_deref(), Some("main"));
        let record = key_rows::list(test.app.pool())
            .await
            .unwrap()
            .into_iter()
            .find(|row| row.id == grant)
            .unwrap();
        assert!(record.self_issued);
        assert_eq!(record.kind, KeyKind::OAuth);
        assert_eq!(record.client_id.as_deref(), Some(CLIENT_ID));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lowering_the_ceiling_lowers_the_grant_after_a_reload() {
        let test = oauth_app().await;
        let alice = signed_in(&test, "alice", &[("main", Access::Write)]).await;
        let page = user_page(&test, &alice).await;
        let (_, headers, _) = consent_as(
            &test,
            Some(&alice),
            &page,
            &[
                ("grant.main", "on"),
                ("level.main", "write"),
                ("default_catalog", ""),
            ],
        )
        .await;
        let grant = grant_of(&test, &headers);
        let granted = test
            .app
            .keys()
            .find_grant(&grant)
            .expect("cached")
            .level("main");

        ceiling::entitle(test.app.pool(), "alice", "main", Access::Read, Utc::now())
            .await
            .expect("lower");
        test.app.poll().await.expect("reload");
        let lowered = test
            .app
            .keys()
            .find_grant(&grant)
            .expect("cached")
            .level("main");
        ceiling::unentitle(test.app.pool(), "alice", "main")
            .await
            .expect("remove");
        test.app.poll().await.expect("reload");
        let removed = test
            .app
            .keys()
            .find_grant(&grant)
            .map(|principal| principal.level("main"));

        assert_eq!(granted, Some(Access::Write));
        assert_eq!(lowered, Some(Access::Read));
        // Outside the ceiling the grant reaches nothing (it may stay cached
        // with no catalogs, or drop out of the cache).
        assert!(matches!(removed, None | Some(None)), "{removed:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_tampered_consent_is_refused_and_grants_nothing() {
        let test = oauth_app().await;
        let alice = signed_in(&test, "alice", &[("main", Access::Read)]).await;
        let cases: [&[(&str, &str)]; 6] = [
            &[("grant.other", "on"), ("level.other", "read")],
            &[("grant.main", "on"), ("level.main", "write")],
            &[("grant.main", "on"), ("level.main", "admin")],
            &[("grant.main", "yes"), ("level.main", "read")],
            &[
                ("grant.main", "on"),
                ("level.main", "read"),
                ("sneaky", "1"),
            ],
            &[
                ("grant.main", "on"),
                ("level.main", "read"),
                ("level.other", "read"),
            ],
        ];
        for choices in cases {
            let page = user_page(&test, &alice).await;
            let (status, _, body) = consent_as(&test, Some(&alice), &page, choices).await;
            assert_eq!(status, StatusCode::BAD_REQUEST, "{choices:?}");
            assert!(
                body.contains("did not match what was offered"),
                "{choices:?}"
            );
            // The request is over: the same form cannot be sent again.
            let (again, _, _) = consent_as(&test, Some(&alice), &page, READ_MAIN).await;
            assert_eq!(again, StatusCode::BAD_REQUEST, "{choices:?}");
        }
        // A forged consent nonce.
        let page = user_page(&test, &alice).await;
        let forged = form(&[
            ("pending", &hidden(&page, "pending")),
            ("csrf", &hidden(&page, "csrf")),
            ("summary", "forged"),
            ("action", "consent"),
            ("grant.main", "on"),
            ("level.main", "read"),
        ]);
        let (status, _, _) = send_as(
            &test,
            "POST",
            "/oauth/authorize",
            Some(forged),
            Some(&alice),
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(oauth_grants(&test).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_consent_posted_by_another_signed_in_user_is_refused() {
        let test = oauth_app().await;
        let alice = signed_in(&test, "alice", &[("main", Access::Read)]).await;
        let bob = signed_in(&test, "bob", &[("main", Access::Read)]).await;
        let page = user_page(&test, &alice).await;

        let (status, _, body) = consent_as(&test, Some(&bob), &page, READ_MAIN).await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("no longer signed in"), "{body}");
        assert_eq!(oauth_grants(&test).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_ended_after_the_page_cannot_approve() {
        let test = oauth_app().await;
        let alice = signed_in(&test, "alice", &[("main", Access::Read)]).await;
        let without_cookie = user_page(&test, &alice).await;
        let signed_out = user_page(&test, &alice).await;

        let (no_cookie, _, no_cookie_body) =
            consent_as(&test, None, &without_cookie, READ_MAIN).await;
        user_sessions::revoke(test.app.pool(), alice.cookie(COOKIE).expect("a session"))
            .await
            .expect("sign out");
        let (ended, _, ended_body) = consent_as(&test, Some(&alice), &signed_out, READ_MAIN).await;

        assert_eq!(no_cookie, StatusCode::BAD_REQUEST);
        assert!(no_cookie_body.contains("no longer signed in"));
        assert_eq!(ended, StatusCode::BAD_REQUEST);
        assert!(ended_body.contains("no longer signed in"));
        assert_eq!(oauth_grants(&test).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_ceiling_lowered_after_the_page_refuses_the_old_offer() {
        let test = oauth_app().await;
        let alice = signed_in(&test, "alice", &[("main", Access::Write)]).await;
        let page = user_page(&test, &alice).await;
        ceiling::entitle(test.app.pool(), "alice", "main", Access::Read, Utc::now())
            .await
            .expect("lower");

        let (status, _, body) = consent_as(
            &test,
            Some(&alice),
            &page,
            &[("grant.main", "on"), ("level.main", "write")],
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("access changed"), "{body}");
        assert_eq!(oauth_grants(&test).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_catalog_offered_since_the_page_was_shown_cannot_be_chosen() {
        // The nonce binds Approve to what that page offered, not to the
        // ceiling at POST time.
        let test = oauth_app().await;
        let alice = signed_in(&test, "alice", &[("main", Access::Read)]).await;
        let page = user_page(&test, &alice).await;
        ceiling::entitle(test.app.pool(), "alice", "other", Access::Read, Utc::now())
            .await
            .expect("widen");

        let (status, _, _) = consent_as(
            &test,
            Some(&alice),
            &page,
            &[("grant.other", "on"), ("level.other", "read")],
        )
        .await;

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(oauth_grants(&test).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn choosing_nothing_or_an_unchosen_default_shows_the_form_again() {
        let test = oauth_app().await;
        let alice = signed_in(
            &test,
            "alice",
            &[("main", Access::Read), ("other", Access::Read)],
        )
        .await;
        let page = user_page(&test, &alice).await;

        let (nothing, _, again) =
            consent_as(&test, Some(&alice), &page, &[("level.main", "read")]).await;
        let (default, _, again_too) = consent_as(
            &test,
            Some(&alice),
            &again,
            &[
                ("grant.main", "on"),
                ("level.main", "read"),
                ("default_catalog", "other"),
            ],
        )
        .await;

        assert_eq!(nothing, StatusCode::OK);
        assert!(again.contains("id=\"consent-user-form\""));
        assert!(again.contains("Choose at least one catalog"));
        assert_eq!(default, StatusCode::OK);
        assert!(again_too.contains("The default catalog must be one you allow"));
        assert_eq!(oauth_grants(&test).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn each_page_approves_at_most_once() {
        let test = oauth_app().await;
        let alice = signed_in(&test, "alice", &[("main", Access::Read)]).await;
        let first = user_page(&test, &alice).await;
        // Choosing nothing shows the page again with a new nonce, spending
        // the first page's.
        let (_, _, again) = consent_as(&test, Some(&alice), &first, &[]).await;

        let (stale, _, _) = consent_as(&test, Some(&alice), &first, READ_MAIN).await;
        // Two Approves of one page racing: one wins.
        let racing = consent_form(&again, READ_MAIN);
        let (one, two) = tokio::join!(
            send_as(
                &test,
                "POST",
                "/oauth/authorize",
                Some(racing.clone()),
                Some(&alice)
            ),
            send_as(
                &test,
                "POST",
                "/oauth/authorize",
                Some(racing),
                Some(&alice)
            ),
        );

        assert_eq!(stale, StatusCode::BAD_REQUEST);
        let mut statuses = [one.0, two.0];
        statuses.sort();
        assert_eq!(statuses, [StatusCode::SEE_OTHER, StatusCode::BAD_REQUEST]);
        assert_eq!(oauth_grants(&test).await, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_consent_posted_from_another_site_is_refused() {
        let test = oauth_app().await;
        let alice = signed_in(&test, "alice", &[("main", Access::Read)]).await;
        let page = user_page(&test, &alice).await;
        let post = |site: &'static str| {
            let request = Request::builder()
                .method("POST")
                .uri("/oauth/authorize")
                .header("content-type", "application/x-www-form-urlencoded")
                .header("sec-fetch-site", site)
                .header(
                    "cookie",
                    format!("{COOKIE}={}", alice.cookie(COOKIE).expect("a session")),
                )
                .body(Body::from(consent_form(&page, READ_MAIN)))
                .expect("a request");
            crate::http::router(Arc::clone(&test.app), &CancellationToken::new()).oneshot(request)
        };

        let cross = post("cross-site").await.expect("a response").status();
        let same_site = post("same-site").await.expect("a response").status();
        let same_origin = post("same-origin").await.expect("a response").status();

        assert_eq!(cross, StatusCode::FORBIDDEN);
        assert_eq!(same_site, StatusCode::FORBIDDEN);
        // Refused before the nonce was looked at: the page still approves.
        assert_eq!(same_origin, StatusCode::SEE_OTHER);
        assert_eq!(oauth_grants(&test).await, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_code_steps_are_refused_on_the_signed_in_path() {
        let test = oauth_app().await;
        let code = sign_in_code(&test, Access::Write).await;
        let alice = signed_in(&test, "alice", &[("main", Access::Read)]).await;
        let page = user_page(&test, &alice).await;
        let (pending, csrf) = (hidden(&page, "pending"), hidden(&page, "csrf"));

        let (entered, _, _) = send_as(
            &test,
            "POST",
            "/oauth/authorize",
            Some(form(&[
                ("pending", &pending),
                ("csrf", &csrf),
                ("action", "code"),
                ("sign_in_code", &code),
            ])),
            Some(&alice),
        )
        .await;

        assert_eq!(entered, StatusCode::BAD_REQUEST);
        // The code path's page cannot reach the user path's action either.
        let (_, _, code_page) = send(
            &test,
            "GET",
            &format!("/oauth/authorize?{}", authorize_query()),
            None,
        )
        .await;
        let (consented, _, _) = send_as(
            &test,
            "POST",
            "/oauth/authorize",
            Some(form(&[
                ("pending", &hidden(&code_page, "pending")),
                ("csrf", &hidden(&code_page, "csrf")),
                ("action", "consent"),
                ("grant.main", "on"),
                ("level.main", "read"),
            ])),
            Some(&alice),
        )
        .await;
        assert_eq!(consented, StatusCode::BAD_REQUEST);
        assert_eq!(oauth_grants(&test).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_empty_ceiling_offers_only_deny_and_the_code_path() {
        let test = oauth_app().await;
        let alice = signed_in(&test, "alice", &[]).await;

        let page = user_page(&test, &alice).await;

        assert!(page.contains("id=\"consent-empty\""));
        assert!(!page.contains("id=\"consent-approve\""));
        assert!(page.contains("id=\"consent-deny\""));
        assert!(page.contains("id=\"consent-use-code\""));
    }

    #[test]
    fn debug_never_prints_the_forms_secrets() {
        let posted = super::ConsentForm {
            pending: "pending-id-that-must-not-leak".to_string(),
            csrf: "csrf-token-that-must-not-leak".to_string(),
            action: "consent".to_string(),
            sign_in_code: Some("okfsc_code-that-must-not-leak".to_string()),
            summary: Some("consent-nonce-that-must-not-leak".to_string()),
            default_catalog: None,
            choices: BTreeMap::new(),
        };

        let printed = format!("{posted:?}");

        for secret in [
            "pending-id-that-must-not-leak",
            "csrf-token-that-must-not-leak",
            "okfsc_code-that-must-not-leak",
            "consent-nonce-that-must-not-leak",
        ] {
            assert!(!printed.contains(secret), "{printed}");
        }
        assert!(printed.contains("consent"), "{printed}");
    }

    #[test]
    fn chosen_levels_keep_ticked_catalogs_within_the_offer() {
        let offered: Ceiling = [
            ("main".to_string(), Access::Write),
            ("other".to_string(), Access::Read),
        ]
        .into_iter()
        .collect();
        let choices = |pairs: &[(&str, &str)]| -> BTreeMap<String, String> {
            pairs
                .iter()
                .map(|(k, v)| (k.to_string(), v.to_string()))
                .collect()
        };

        let chosen = chosen_levels(
            &choices(&[
                ("grant.main", "on"),
                ("level.main", "read"),
                ("level.other", "read"),
            ]),
            &offered,
        );

        assert_eq!(
            chosen.ok(),
            Some([("main".to_string(), Access::Read)].into_iter().collect())
        );
        assert_eq!(chosen_levels(&choices(&[]), &offered), Err(Choice::Empty));
        for tampered in [
            &[("grant.main", "on")][..],
            &[("grant.other", "on"), ("level.other", "write")],
            &[("grant.main", "yes"), ("level.main", "read")],
            &[("level.nowhere", "read")],
            &[("level.other", "write")],
            &[("grant.main", "on"), ("level.main", "admin")],
            &[("anything", "1")],
        ] {
            assert_eq!(
                chosen_levels(&choices(tampered), &offered),
                Err(Choice::Tampered),
                "{tampered:?}"
            );
        }
    }
}

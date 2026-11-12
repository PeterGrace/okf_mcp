//! Passkeys (spec "Passkeys", plan Decision 13): WebAuthn registration from
//! an emailed setup link or a signed-in console session, and discoverable
//! ("usernameless") sign-in. The relying party id is `public_url`'s host
//! and the origin is `public_url`; webauthn-rs always requires user
//! verification.
//!
//! A ceremony's server-side state waits in memory for at most five
//! minutes, keyed by a random id in the `okf_passkey` cookie, and is taken
//! (single use) by the request that finishes it. Like pending consent
//! pages, it is per process.
//!
//! A sign-in checks the assertion and starts the session in one
//! transaction that holds the user's row and the passkey's row
//! (`store::passkeys::lock_for_sign_in`). So the stored counter only moves
//! forward, and no removal of the passkey or password reset can commit
//! between the check and the insert: one that commits first is seen by the
//! check, and a reset that waits ends the session.

use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::extract::State;
use axum::http::request::Parts;
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Extension, Json, Router};
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use serde::Deserialize;
use webauthn_rs::prelude::{
    CredentialID, DiscoverableAuthentication, Passkey, PasskeyRegistration, PublicKeyCredential,
    RegisterPublicKeyCredential, Url, Uuid, Webauthn, WebauthnBuilder, WebauthnError,
};
use webauthn_rs_proto::ResidentKeyRequirement;

use crate::app::App;
use crate::auth::{CredentialKind, Principal};
use crate::config::Config;
use crate::error::OkfError;
use crate::http::api::api_headers;
use crate::http::console_session::{
    append_all, client_ip, cookie, secure_cookies, session_user, set_cookie, ConsoleSession,
    SameSite,
};
use crate::http::limits::ip_key;
use crate::http::pages::safe_return_to;
use crate::http::signin::{audit, cross_site, notify_method_added, start_in, SignIn, LINK_EXPIRED};
use crate::http::{timeout_layer, AppError};
use crate::keys;
use crate::oauth::expiring::ExpiringMap;
use crate::store::email_tokens::{self, Purpose};
use crate::store::passkeys::{self as passkey_rows, SessionRegistration};
use crate::store::sign_in_events::{Method, Outcome};
use crate::store::users::UserAuth;

/// The ceremony cookie: `SameSite=Strict`, five minutes.
pub const CEREMONY_COOKIE: &str = "okf_passkey";
/// How long a ceremony may take (spec: a five-minute cookie).
pub const CEREMONY_TTL: Duration = Duration::from_secs(5 * 60);
/// The most ceremonies in progress at once (they start unauthenticated).
pub const MAX_CEREMONIES: usize = 10_000;
/// The longest passkey name, in characters.
pub const MAX_NAME_CHARS: usize = 64;

/// Every refused passkey sign-in says this, whatever the reason.
const SIGN_IN_FAILED: &str = "Passkey sign-in failed; try again";
/// A registration webauthn-rs refused.
const REGISTRATION_FAILED: &str = "The passkey could not be registered; try again";
/// No ceremony, an expired one, or one of another kind (410).
const CEREMONY_EXPIRED: &str = "The passkey request expired; start again";
/// A request another site's page sent (403).
const CROSS_SITE: &str = "passkey requests can only come from this site";

/// The WebAuthn relying party and the ceremonies in progress, held by `App`.
pub struct Passkeys {
    webauthn: Webauthn,
    ceremonies: ExpiringMap<Ceremony>,
    // The ceremony id each console session's registration holds, keyed by
    // the hash of the session id: a session has at most one at a time.
    by_session: ExpiringMap<String>,
}

impl Passkeys {
    /// The relying party for `public_url`.
    ///
    /// # Arguments
    /// * `config` - the server's settings.
    ///
    /// # Returns
    /// `None` without `public_url`, or when its host is an address (WebAuthn
    /// needs a domain; logged at `info`).
    ///
    /// # Errors
    /// [`OkfError::Config`] if `public_url` does not parse or webauthn-rs
    /// refuses it.
    pub fn new(config: &Config) -> Result<Option<Self>, OkfError> {
        let Some(public_url) = config.public_url.as_deref() else {
            return Ok(None);
        };
        // A closure that builds the error, so the two failures below share it.
        let error = |message: String| OkfError::Config {
            path: "public_url".to_string(),
            message,
        };
        let origin = Url::parse(public_url).map_err(|e| error(format!("{public_url:?}: {e}")))?;
        // `domain()` is `None` for an address host such as 127.0.0.1.
        let Some(rp_id) = origin.domain() else {
            tracing::info!(
                "passkeys are off: public_url's host is an address, and WebAuthn needs a domain"
            );
            return Ok(None);
        };
        let webauthn = WebauthnBuilder::new(rp_id, &origin)
            .and_then(|builder| builder.rp_name("okf").build())
            .map_err(|e| error(format!("WebAuthn refuses {public_url:?}: {e}")))?;
        Ok(Some(Self {
            webauthn,
            ceremonies: ExpiringMap::new(CEREMONY_TTL, MAX_CEREMONIES),
            by_session: ExpiringMap::new(CEREMONY_TTL, MAX_CEREMONIES),
        }))
    }
}

/// A ceremony in progress. No `Debug`: it holds the challenges and, for a
/// setup link, the emailed token.
enum Ceremony {
    /// Registering a passkey (boxed: much larger than a sign-in).
    Register(Box<Registration>),
    /// A discoverable sign-in.
    SignIn {
        state: DiscoverableAuthentication,
        return_to: String,
    },
}

/// A registration's state and what to do once it finishes.
struct Registration {
    state: PasskeyRegistration,
    user_id: String,
    pending: Pending,
}

/// What a registration was started with.
struct Pending {
    name: String,
    via: RegisterVia,
    return_to: String,
}

/// Who asked for a registration.
enum RegisterVia {
    /// The setup page, with the emailed token (used at finish).
    Setup { token: String },
    /// A signed-in session, whose other sessions end unless kept.
    Session {
        session_id: String,
        keep_sessions: bool,
    },
}

/// The open routes (no credential), merged only when passkeys are on.
///
/// # Returns
/// The router; every response is `no-store` and `nosniff`.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/auth/passkey/setup/start", post(setup_start))
        .route("/auth/passkey/setup/finish", post(setup_finish))
        .route("/auth/passkey/signin/start", post(sign_in_start))
        .route("/auth/passkey/signin/finish", post(sign_in_finish))
        .layer(timeout_layer())
        .layer(middleware::map_response(api_headers))
}

/// The signed-in registration routes, merged into `/api/v1` (behind
/// `require_principal`, so a cookie write needs its CSRF header); 404
/// while passkeys are off.
///
/// # Returns
/// The router.
pub(crate) fn api_routes() -> Router<Arc<App>> {
    Router::new()
        .route("/me/passkeys/register/start", post(register_start))
        .route("/me/passkeys/register/finish", post(register_finish))
}

/// The passkey state, or 404 while passkeys are off.
fn enabled(app: &App) -> Result<&Passkeys, OkfError> {
    app.passkeys().ok_or_else(|| OkfError::NotFound {
        path: "passkeys".to_string(),
    })
}

/// The 410 for a missing, expired, used or mismatched ceremony.
fn expired() -> OkfError {
    OkfError::Gone {
        reason: CEREMONY_EXPIRED.to_string(),
    }
}

/// Logs why webauthn-rs refused, and answers with one message.
fn refused(error: &WebauthnError, message: &str) -> OkfError {
    tracing::debug!(%error, "WebAuthn refused");
    OkfError::Unauthenticated {
        reason: message.to_string(),
    }
}

/// A passkey name: trimmed, 1 to 64 characters, no control characters
/// (a NUL, which Postgres refuses, included).
fn passkey_name(raw: &str) -> Result<String, OkfError> {
    let name = raw.trim();
    let length = name.chars().count();
    if length == 0 || length > MAX_NAME_CHARS || name.chars().any(char::is_control) {
        return Err(OkfError::LimitExceeded {
            limit: "passkey_name",
            reason: format!(
                "a passkey name is 1 to {MAX_NAME_CHARS} characters, with no control characters"
            ),
        });
    }
    Ok(name.to_string())
}

/// The WebAuthn user handle: the user's UUID (`users.id` is
/// `gen_random_uuid()::text`).
fn user_handle(user_id: &str) -> Result<Uuid, OkfError> {
    Uuid::parse_str(user_id).map_err(|_| OkfError::Refused {
        reason: "this user's id is not a UUID, which a passkey needs".to_string(),
    })
}

/// 429 for the per-address limit, as JSON.
fn too_many() -> Response {
    (
        StatusCode::TOO_MANY_REQUESTS,
        Json(serde_json::json!({
            "error": "TooManyRequests",
            "message": "Too many attempts from this address; wait a few minutes.",
        })),
    )
        .into_response()
}

/// Screens an open route's request: 403 when another site's page sent it
/// (login CSRF, as the sign-in forms check), 429 past the per-address
/// sign-in limit (shared with password sign-in).
///
/// # Returns
/// The refusal to send, if any.
fn screen(app: &App, parts: &Parts, limited: bool) -> Option<Response> {
    if cross_site(&parts.headers) {
        let error = OkfError::Forbidden {
            reason: CROSS_SITE.to_string(),
        };
        return Some(AppError(error).into_response());
    }
    let ip = client_ip(parts, &app.config().auth);
    let admitted = !limited
        || app
            .sign_in_limits()
            .sign_in_per_ip
            .try_take(&ip_key(ip), Instant::now());
    (!admitted).then(too_many)
}

/// The 503 for a full ceremony map.
fn busy() -> OkfError {
    OkfError::Busy {
        reason: "too many passkey requests in progress; try again shortly".to_string(),
    }
}

/// The console session a registration ceremony belongs to, as the key of
/// `Passkeys::by_session` (the hash, not the raw id).
fn session_key(ceremony: &Ceremony) -> Option<String> {
    let Ceremony::Register(registration) = ceremony else {
        return None;
    };
    match &registration.pending.via {
        RegisterVia::Session { session_id, .. } => {
            Some(URL_SAFE_NO_PAD.encode(keys::hash(session_id)))
        }
        RegisterVia::Setup { .. } => None,
    }
}

/// Stores a ceremony and returns the cookie that names it. Each actor
/// holds at most one ceremony at a time, so no one can grow the map
/// beyond the per-address limit on the open starts:
/// - the ceremony the request's old cookie names is dropped (a browser
///   that starts again replaces its previous one);
/// - a console session's new registration replaces that session's previous
///   one, cookie or not.
///
/// # Arguments
/// * `app` - the shared state (for `Secure`).
/// * `passkeys` - the ceremonies.
/// * `headers` - the request's headers (its old ceremony cookie).
/// * `ceremony` - the state to keep.
/// * `now` - the current instant.
///
/// # Errors
/// [`OkfError::Busy`] when [`MAX_CEREMONIES`] are in progress;
/// [`OkfError::Io`] from the random source.
fn remember(
    app: &App,
    passkeys: &Passkeys,
    headers: &HeaderMap,
    ceremony: Ceremony,
    now: Instant,
) -> Result<HeaderMap, OkfError> {
    if let Some(old) = cookie(headers, CEREMONY_COOKIE) {
        passkeys.ceremonies.remove(old);
    }
    let session = session_key(&ceremony);
    // A server-chosen random id, so no client can pick (or replace) another
    // browser's entry.
    let id = keys::generate_with("")?;
    if !passkeys.ceremonies.insert(id.clone(), ceremony, now) {
        return Err(busy());
    }
    if let Some(session) = session {
        // Find-or-create and swap under one lock (`std::mem::replace` puts
        // the new id in and hands back the old one), so of two concurrent
        // starts by one session each drops what it replaced and one
        // ceremony is left.
        let replaced = passkeys.by_session.update_or_insert_sliding(
            &session,
            now,
            || id.clone(),
            |held| std::mem::replace(held, id.clone()),
        );
        match replaced {
            Some(previous) if previous != id => passkeys.ceremonies.remove(&previous),
            Some(_) => {}
            None => {
                passkeys.ceremonies.remove(&id);
                return Err(busy());
            }
        }
    }
    let mut headers = HeaderMap::new();
    headers.insert(
        header::SET_COOKIE,
        set_cookie(
            CEREMONY_COOKIE,
            &id,
            CEREMONY_TTL,
            SameSite::Strict,
            secure_cookies(app),
        ),
    );
    Ok(headers)
}

/// Takes (single use) the ceremony the cookie names.
///
/// # Returns
/// The ceremony and the header that clears its cookie.
///
/// # Errors
/// [`OkfError::Gone`] without a cookie or a live ceremony.
fn take_ceremony(
    app: &App,
    passkeys: &Passkeys,
    headers: &HeaderMap,
    now: Instant,
) -> Result<(Ceremony, HeaderMap), OkfError> {
    let id = cookie(headers, CEREMONY_COOKIE).ok_or_else(expired)?;
    let ceremony = passkeys.ceremonies.take(id, now).ok_or_else(expired)?;
    let mut clear = HeaderMap::new();
    clear.insert(
        header::SET_COOKIE,
        set_cookie(
            CEREMONY_COOKIE,
            "",
            Duration::ZERO,
            SameSite::Strict,
            secure_cookies(app),
        ),
    );
    Ok((ceremony, clear))
}

/// Starts a registration for `user`: the challenge (asking for a
/// discoverable credential) and the ceremony cookie.
async fn begin_registration(
    app: &App,
    passkeys: &Passkeys,
    headers: &HeaderMap,
    user: &UserAuth,
    pending: Pending,
) -> Result<Response, OkfError> {
    let handle = user_handle(&user.id)?;
    let existing: Vec<CredentialID> = passkey_rows::for_user(app.pool(), &user.id)
        .await?
        .into_iter()
        .map(|(_, passkey)| passkey.cred_id().clone())
        .collect();
    let account = user.email.as_deref().unwrap_or(&user.name);
    let (mut challenge, state) = passkeys
        .webauthn
        .start_passkey_registration(
            handle,
            account,
            &user.name,
            (!existing.is_empty()).then_some(existing),
        )
        .map_err(|error| refused(&error, REGISTRATION_FAILED))?;
    // webauthn-rs asks for `residentKey: "discouraged"`; discoverable
    // sign-in needs the credential kept on the authenticator.
    if let Some(selection) = challenge.public_key.authenticator_selection.as_mut() {
        selection.resident_key = Some(ResidentKeyRequirement::Required);
    }
    let ceremony = Ceremony::Register(Box::new(Registration {
        state,
        user_id: user.id.clone(),
        pending,
    }));
    let cookie = remember(app, passkeys, headers, ceremony, Instant::now())?;
    Ok((cookie, Json(challenge)).into_response())
}

/// A verified registration: the new passkey, what it was started with, its
/// owner and the header that clears the ceremony cookie.
struct Registered {
    passkey: Passkey,
    pending: Pending,
    user_id: String,
    clear: HeaderMap,
}

/// Takes the registration the cookie names and verifies the credential
/// (no database work, so no transaction is open meanwhile).
///
/// # Errors
/// [`OkfError::Gone`] without a live registration ceremony;
/// [`OkfError::Unauthenticated`] for a credential that does not verify.
fn finish_registration(
    app: &App,
    passkeys: &Passkeys,
    headers: &HeaderMap,
    credential: &RegisterPublicKeyCredential,
) -> Result<Registered, OkfError> {
    let (ceremony, clear) = take_ceremony(app, passkeys, headers, Instant::now())?;
    let Ceremony::Register(registration) = ceremony else {
        return Err(expired());
    };
    // `*registration` moves the struct out of its `Box`.
    let Registration {
        state,
        user_id,
        pending,
    } = *registration;
    let passkey = passkeys
        .webauthn
        .finish_passkey_registration(credential, &state)
        .map_err(|error| refused(&error, REGISTRATION_FAILED))?;
    Ok(Registered {
        passkey,
        pending,
        user_id,
        clear,
    })
}

/// `POST /auth/passkey/setup/start`'s body. No `Debug`: it holds the token.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SetupStart {
    token: String,
    name: String,
    #[serde(default)]
    return_to: Option<String>,
}

/// `POST /auth/passkey/setup/start`: a registration challenge for the
/// setup link's user. The link is checked, not used.
///
/// # Errors
/// 403 from another site; 404 with passkeys off; 410 for a bad link; 422
/// for a bad name; 429 past the per-address limit.
async fn setup_start(
    State(app): State<Arc<App>>,
    parts: Parts,
    Json(body): Json<SetupStart>,
) -> Result<Response, AppError> {
    let passkeys = enabled(&app)?;
    if let Some(refusal) = screen(&app, &parts, true) {
        return Ok(refusal);
    }
    let name = passkey_name(&body.name)?;
    let user =
        email_tokens::peek(app.pool(), &body.token, Purpose::Setup, app.clock().now()).await?;
    let pending = Pending {
        name,
        via: RegisterVia::Setup { token: body.token },
        return_to: safe_return_to(body.return_to.as_deref()),
    };
    Ok(begin_registration(&app, passkeys, &parts.headers, &user, pending).await?)
}

/// `POST /auth/passkey/setup/finish`: stores the passkey, uses the link and
/// signs in, all in one transaction, so a failure anywhere leaves the link
/// usable and no passkey or session behind. Answers
/// `{"redirect": <return_to>}`.
///
/// # Errors
/// 403 from another site; 410 for an expired ceremony or a used link; 401
/// for a credential that does not verify; 409 for a passkey already
/// registered.
async fn setup_finish(
    State(app): State<Arc<App>>,
    parts: Parts,
    Json(credential): Json<RegisterPublicKeyCredential>,
) -> Result<Response, AppError> {
    let passkeys = enabled(&app)?;
    if let Some(refusal) = screen(&app, &parts, false) {
        return Ok(refusal);
    }
    let Registered {
        passkey,
        pending,
        user_id,
        clear: mut headers,
    } = finish_registration(&app, passkeys, &parts.headers, &credential)?;
    let RegisterVia::Setup { token } = pending.via else {
        return Err(expired().into());
    };
    let set_up = SetUp {
        token: &token,
        user_id: &user_id,
        passkey: &passkey,
        name: &pending.name,
    };
    // The event is recorded in the link's transaction: nothing runs after
    // its commit but building the response.
    let cookie = use_setup_link(&app, &parts, &set_up).await?;
    append_all(&mut headers, &cookie);
    Ok((
        headers,
        Json(serde_json::json!({ "redirect": pending.return_to })),
    )
        .into_response())
}

/// A verified setup-link registration, for [`use_setup_link`]. No `Debug`:
/// it holds the token.
struct SetUp<'a> {
    /// The emailed token.
    token: &'a str,
    /// The user the ceremony was started for.
    user_id: &'a str,
    /// The verified passkey.
    passkey: &'a Passkey,
    /// Its name.
    name: &'a str,
}

/// Uses the setup link, stores the passkey and starts the session in one
/// transaction. `redeem_in` locks the user's row first, then the token
/// (lock order), and the passkey and the session follow under that lock;
/// any error drops `tx`, which rolls everything back.
///
/// # Returns
/// The session's `Set-Cookie` header.
///
/// # Errors
/// [`OkfError::Gone`] for a link used, expired or now naming another user;
/// [`OkfError::Refused`] for a passkey already registered;
/// [`OkfError::Database`].
async fn use_setup_link(
    app: &App,
    parts: &Parts,
    set_up: &SetUp<'_>,
) -> Result<HeaderMap, OkfError> {
    let now = app.clock().now();
    let mut tx = app.pool().begin().await?;
    let user = email_tokens::redeem_in(&mut tx, set_up.token, Purpose::Setup, now).await?;
    if user.id != set_up.user_id {
        return Err(OkfError::Gone {
            reason: LINK_EXPIRED.to_string(),
        });
    }
    passkey_rows::add_in(&mut tx, &user.id, set_up.passkey, set_up.name, now).await?;
    let signing_in = SignIn {
        user: &user,
        method: Method::Setup,
        password: None,
    };
    let cookie = start_in(app, &mut tx, parts, &signing_in).await?;
    tx.commit().await?;
    tracing::info!(user = %user.id, "passkey added from a setup link");
    Ok(cookie)
}

/// `POST /auth/passkey/signin/start`'s body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SignInStart {
    #[serde(default)]
    return_to: Option<String>,
}

/// `POST /auth/passkey/signin/start`: a discoverable challenge.
///
/// # Errors
/// 403 from another site; 404 with passkeys off; 429 past the per-address
/// limit; 503 when too many ceremonies are in progress.
async fn sign_in_start(
    State(app): State<Arc<App>>,
    parts: Parts,
    Json(body): Json<SignInStart>,
) -> Result<Response, AppError> {
    let passkeys = enabled(&app)?;
    if let Some(refusal) = screen(&app, &parts, true) {
        return Ok(refusal);
    }
    let (mut challenge, state) = passkeys
        .webauthn
        .start_discoverable_authentication()
        .map_err(|error| refused(&error, SIGN_IN_FAILED))?;
    // A button press, not autofill: without conditional mediation the
    // browser shows its modal prompt.
    challenge.mediation = None;
    let return_to = safe_return_to(body.return_to.as_deref());
    let ceremony = Ceremony::SignIn { state, return_to };
    let headers = remember(&app, passkeys, &parts.headers, ceremony, Instant::now())?;
    Ok((headers, Json(challenge)).into_response())
}

/// What an assertion proved.
enum Verdict {
    /// Signed in: the session (and its event) is committed.
    SignedIn { cookie: HeaderMap },
    /// Refused, naming the credential's owner when it is known.
    Refused { user_id: Option<String> },
}

/// Checks an assertion and, if it holds, stores the raised counter and
/// starts the session, in one transaction that holds the user's row and
/// the passkey's (`passkey_rows::lock_for_sign_in`). The signature check
/// runs under those locks: it is one public-key verification (well under a
/// millisecond), and it is what makes the check and the insert one step.
///
/// # Errors
/// [`OkfError::Database`]; [`OkfError::Io`] from the random source.
async fn verify_and_start(
    app: &App,
    passkeys: &Passkeys,
    parts: &Parts,
    credential: &PublicKeyCredential,
    state: DiscoverableAuthentication,
) -> Result<Verdict, OkfError> {
    let Ok((handle, credential_id)) = passkeys
        .webauthn
        .identify_discoverable_authentication(credential)
    else {
        return Ok(Verdict::Refused { user_id: None });
    };
    // Every early return below drops `tx`, which rolls back and releases
    // the locks; only a sign-in that holds commits.
    let mut tx = app.pool().begin().await?;
    let Some((user, mut passkey)) = passkey_rows::lock_for_sign_in(&mut tx, credential_id).await?
    else {
        return Ok(Verdict::Refused { user_id: None });
    };
    // webauthn-rs does not check that the user handle names the
    // credential's owner; the handle must be exactly that existing user.
    if Uuid::parse_str(&user.id).ok() != Some(handle) {
        return Ok(Verdict::Refused {
            user_id: Some(user.id),
        });
    }
    let result = match passkeys
        .webauthn
        .finish_discoverable_authentication_v2(credential, state, &passkey)
    {
        Ok(result) => result,
        // The authenticator's counter did not move past the stored one:
        // a replayed assertion, or a cloned authenticator.
        Err(WebauthnError::CredentialPossibleCompromise) => {
            tracing::warn!(user = %user.id, "passkey refused: its counter did not move");
            return Ok(Verdict::Refused {
                user_id: Some(user.id),
            });
        }
        Err(error) => {
            tracing::debug!(%error, user = %user.id, "passkey assertion refused");
            return Ok(Verdict::Refused {
                user_id: Some(user.id),
            });
        }
    };
    // Raises the stored counter and syncs the backup flags; `None` only if
    // the result is for another credential, which `_v2` already refused.
    if passkey.update_credential(&result).is_none() {
        return Ok(Verdict::Refused {
            user_id: Some(user.id),
        });
    }
    passkey_rows::update_in(&mut tx, credential_id, &passkey, app.clock().now()).await?;
    let signing_in = SignIn {
        user: &user,
        method: Method::Passkey,
        password: None,
    };
    let cookie = start_in(app, &mut tx, parts, &signing_in).await?;
    tx.commit().await?;
    Ok(Verdict::SignedIn { cookie })
}

/// `POST /auth/passkey/signin/finish`: signs in. Answers
/// `{"redirect": <return_to>}` with the session cookie.
///
/// # Errors
/// 403 from another site; 410 for an expired or used ceremony; 401 for an
/// assertion that does not verify (recorded as a failed sign-in); 429 past
/// the per-address limit.
async fn sign_in_finish(
    State(app): State<Arc<App>>,
    parts: Parts,
    Json(credential): Json<PublicKeyCredential>,
) -> Result<Response, AppError> {
    let passkeys = enabled(&app)?;
    if let Some(refusal) = screen(&app, &parts, true) {
        return Ok(refusal);
    }
    let (ceremony, mut headers) = take_ceremony(&app, passkeys, &parts.headers, Instant::now())?;
    let Ceremony::SignIn { state, return_to } = ceremony else {
        return Err(expired().into());
    };
    match verify_and_start(&app, passkeys, &parts, &credential, state).await? {
        // The event is recorded in the sign-in's transaction: nothing runs
        // after its commit but building the response.
        Verdict::SignedIn { cookie } => {
            append_all(&mut headers, &cookie);
            Ok((headers, Json(serde_json::json!({ "redirect": return_to }))).into_response())
        }
        Verdict::Refused { user_id } => {
            let ip = client_ip(&parts, &app.config().auth);
            audit(
                &app,
                user_id.as_deref(),
                Method::Passkey,
                Outcome::Failed,
                ip,
            )
            .await;
            let error = OkfError::Unauthenticated {
                reason: SIGN_IN_FAILED.to_string(),
            };
            // The ceremony was used either way, so its cookie goes too.
            Ok((headers, AppError(error)).into_response())
        }
    }
}

/// The calling session's user id and session, or 403 for a key or grant.
fn signed_in(
    principal: &Principal,
    session: Option<Extension<ConsoleSession>>,
) -> Result<(String, ConsoleSession), OkfError> {
    match (&principal.kind, session) {
        (CredentialKind::Session { user_id }, Some(Extension(session))) => {
            Ok((user_id.clone(), session))
        }
        _ => Err(OkfError::Forbidden {
            reason: "adding a passkey needs a console session".to_string(),
        }),
    }
}

/// `POST /me/passkeys/register/start`'s body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterStart {
    name: String,
    /// Keep the user's other sessions (the account page's checkbox).
    #[serde(default)]
    keep_sessions: bool,
}

/// `POST /api/v1/me/passkeys/register/start`.
///
/// # Errors
/// 403 for a key or grant; 404 with passkeys off; 422 for a bad name.
async fn register_start(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    session: Option<Extension<ConsoleSession>>,
    headers: HeaderMap,
    Json(body): Json<RegisterStart>,
) -> Result<Response, AppError> {
    let (user_id, session) = signed_in(&principal, session)?;
    let passkeys = enabled(&app)?;
    let name = passkey_name(&body.name)?;
    let user = session_user(&app, &user_id)
        .await?
        .ok_or_else(|| OkfError::Unauthenticated {
            reason: "this session's user no longer exists".to_string(),
        })?;
    let pending = Pending {
        name,
        via: RegisterVia::Session {
            session_id: session.id,
            keep_sessions: body.keep_sessions,
        },
        return_to: "/ui/#/account".to_string(),
    };
    Ok(begin_registration(&app, passkeys, &headers, &user, pending).await?)
}

/// `POST /api/v1/me/passkeys/register/finish`: stores the passkey, ends the
/// user's other sessions unless kept (in the same transaction, which
/// refuses a session ended meanwhile), and mails the notice. 201 with
/// `{"id", "name"}`.
///
/// # Errors
/// 403 for a key, a grant or another session's ceremony; 410 for an expired
/// ceremony; 401 for a credential that does not verify or a session that
/// ended meanwhile; 409 for a passkey already registered.
async fn register_finish(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    session: Option<Extension<ConsoleSession>>,
    headers: HeaderMap,
    Json(credential): Json<RegisterPublicKeyCredential>,
) -> Result<Response, AppError> {
    let (user_id, session) = signed_in(&principal, session)?;
    let passkeys = enabled(&app)?;
    let Registered {
        passkey,
        pending,
        user_id: owner,
        clear,
    } = finish_registration(&app, passkeys, &headers, &credential)?;
    let RegisterVia::Session {
        session_id,
        keep_sessions,
    } = pending.via
    else {
        return Err(expired().into());
    };
    if owner != user_id || session_id != session.id {
        return Err(OkfError::Forbidden {
            reason: "this passkey request belongs to another session".to_string(),
        }
        .into());
    }
    let registration = SessionRegistration {
        user_id: &user_id,
        session_id: &session_id,
        passkey: &passkey,
        name: &pending.name,
        keep_sessions,
        now: app.clock().now(),
    };
    let ended = store_registration(&app, &registration).await?;
    tracing::info!(user = %user_id, ended, "passkey added; other sessions ended unless kept");
    let body = serde_json::json!({
        "id": URL_SAFE_NO_PAD.encode(passkey.cred_id().as_slice()),
        "name": pending.name,
    });
    Ok((StatusCode::CREATED, clear, Json(body)).into_response())
}

/// Stores a session's passkey, ends the other sessions unless kept and
/// queues the notice, in one transaction; nothing runs after its commit
/// but building the response.
///
/// # Returns
/// How many other sessions ended.
///
/// # Errors
/// As `passkey_rows::add_from_session_in`; [`OkfError::Unauthenticated`]
/// once the user is gone.
async fn store_registration(
    app: &App,
    registration: &SessionRegistration<'_>,
) -> Result<u64, OkfError> {
    // Read before the transaction (the key cache, else the database), for
    // the notice's address.
    let user = session_user(app, registration.user_id)
        .await?
        .ok_or_else(|| OkfError::Unauthenticated {
            reason: "this session's user no longer exists".to_string(),
        })?;
    let mut tx = app.pool().begin().await?;
    let ended = passkey_rows::add_from_session_in(&mut tx, registration).await?;
    notify_method_added(app, &mut tx, &user, "a passkey").await?;
    tx.commit().await?;
    Ok(ended)
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use chrono::Utc;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;
    use webauthn_authenticator_rs::prelude::WebauthnAuthenticator;
    use webauthn_authenticator_rs::softpasskey::SoftPasskey;
    use webauthn_rs::prelude::{CreationChallengeResponse, RequestChallengeResponse};

    use super::*;
    use crate::http::api::testing::{
        allow, fail_on, keys as test_keys, login_app, login_config, user_with_password,
        wait_for_lock_waiters, Browser, Payload, Reply, MAIN, PASSWORD,
    };
    use crate::http::console_session::COOKIE;
    use crate::http::router;
    use crate::mcp::testing::{app_with, TestApp};
    use crate::store::passwords::{self, NewPassword};
    use crate::store::sign_in_events::{self, Method, Outcome};
    use crate::store::sign_in_methods;

    /// `login_config`'s origin, which the software authenticator signs for.
    const ORIGIN: &str = "https://okf.example.com";

    fn origin() -> Url {
        Url::parse(ORIGIN).expect("the origin")
    }

    async fn setup_token(test: &TestApp, user: &UserAuth) -> String {
        email_tokens::issue(
            test.app.pool(),
            &user.id,
            Purpose::Setup,
            Duration::from_secs(3600),
            Utc::now(),
        )
        .await
        .expect("issue a setup token")
    }

    /// Registers `soft` through the setup link, as `passkey.js` does.
    async fn register_by_setup(
        test: &TestApp,
        browser: &mut Browser,
        soft: &mut SoftPasskey,
        token: &str,
    ) -> Reply {
        let body = json!({ "token": token, "name": "Laptop" });
        let start = browser
            .send(
                test,
                "POST",
                "/auth/passkey/setup/start",
                Payload::Json(body),
            )
            .await;
        assert_eq!(start.status, StatusCode::OK, "{}", start.text());
        let line = start
            .set_cookie(CEREMONY_COOKIE)
            .expect("a ceremony cookie");
        assert!(line.contains("SameSite=Strict"), "{line}");
        assert!(line.contains("Max-Age=300"), "{line}");
        let challenge = start.json();
        assert_eq!(
            challenge["publicKey"]["authenticatorSelection"]["residentKey"],
            "required"
        );
        let challenge: CreationChallengeResponse =
            serde_json::from_value(challenge).expect("a challenge");
        let credential = soft
            .do_registration(origin(), challenge)
            .expect("the authenticator registers");
        let credential = serde_json::to_value(&credential).expect("the credential as JSON");
        browser
            .send(
                test,
                "POST",
                "/auth/passkey/setup/finish",
                Payload::Json(credential),
            )
            .await
    }

    /// Alice with a setup-registered passkey, and the authenticator.
    async fn alice_with_passkey(test: &TestApp, password: Option<&str>) -> (UserAuth, SoftPasskey) {
        let alice = user_with_password(test, "alice", "alice@example.com", password).await;
        let token = setup_token(test, &alice).await;
        let mut soft = SoftPasskey::new(true);
        let registered = register_by_setup(test, &mut Browser::default(), &mut soft, &token).await;
        assert_eq!(registered.status, StatusCode::OK, "{}", registered.text());
        (alice, soft)
    }

    /// The assertion for a discoverable sign-in, signed as `handle`'s user
    /// and patched as a browser's own discovery would fill it (the
    /// software authenticator can neither discover nor send a handle).
    async fn assertion_as(
        test: &TestApp,
        browser: &mut Browser,
        soft: &mut SoftPasskey,
        owner: &UserAuth,
        handle: &str,
        return_to: &str,
    ) -> serde_json::Value {
        let start = browser
            .send(
                test,
                "POST",
                "/auth/passkey/signin/start",
                Payload::Json(json!({ "return_to": return_to })),
            )
            .await;
        assert_eq!(start.status, StatusCode::OK, "{}", start.text());
        let mut challenge = start.json();
        assert!(
            challenge.get("mediation").is_none(),
            "a modal prompt, not autofill"
        );
        let stored = passkey_rows::list(test.app.pool(), &owner.id)
            .await
            .expect("list");
        challenge["publicKey"]["allowCredentials"] =
            json!([{ "type": "public-key", "id": stored[0].id }]);
        let challenge: RequestChallengeResponse =
            serde_json::from_value(challenge).expect("a challenge");
        let signed = soft
            .do_authentication(origin(), challenge)
            .expect("the authenticator signs");
        let mut signed = serde_json::to_value(&signed).expect("the assertion as JSON");
        let handle = Uuid::parse_str(handle).expect("user ids are UUIDs");
        signed["response"]["userHandle"] = json!(URL_SAFE_NO_PAD.encode(handle.as_bytes()));
        signed
    }

    async fn assertion(
        test: &TestApp,
        browser: &mut Browser,
        soft: &mut SoftPasskey,
        user: &UserAuth,
        return_to: &str,
    ) -> serde_json::Value {
        assertion_as(test, browser, soft, user, &user.id, return_to).await
    }

    async fn finish(test: &TestApp, browser: &mut Browser, assertion: serde_json::Value) -> Reply {
        browser
            .send(
                test,
                "POST",
                "/auth/passkey/signin/finish",
                Payload::Json(assertion),
            )
            .await
    }

    /// The stored signature counter of alice's only passkey.
    async fn stored_counter(test: &TestApp, user: &UserAuth) -> serde_json::Value {
        let listed = passkey_rows::list(test.app.pool(), &user.id)
            .await
            .expect("list");
        let credential_id = URL_SAFE_NO_PAD.decode(&listed[0].id).expect("base64url");
        let (_, stored) = passkey_rows::find(test.app.pool(), &credential_id)
            .await
            .expect("find")
            .expect("the passkey");
        serde_json::to_value(&stored).expect("json")["cred"]["counter"].clone()
    }

    async fn sessions_of(test: &TestApp, user: &UserAuth) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM user_sessions WHERE user_id = $1")
            .bind(&user.id)
            .fetch_one(test.app.pool())
            .await
            .expect("count the sessions")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_setup_link_registers_a_passkey_that_then_signs_in_without_an_email() {
        let test = login_app().await;
        let alice = user_with_password(&test, "alice", "alice@example.com", None).await;
        let token = setup_token(&test, &alice).await;
        let mut soft = SoftPasskey::new(true);
        let mut setup = Browser::default();

        let registered = register_by_setup(&test, &mut setup, &mut soft, &token).await;

        assert_eq!(registered.status, StatusCode::OK, "{}", registered.text());
        assert_eq!(registered.json()["redirect"], "/ui/");
        assert!(setup.cookie(COOKIE).is_some(), "setting up signs in");
        assert_eq!(
            setup.cookie(CEREMONY_COOKIE),
            None,
            "the ceremony cookie is cleared"
        );
        let used = email_tokens::peek(test.app.pool(), &token, Purpose::Setup, Utc::now()).await;
        assert!(used.is_err(), "the setup link is used");
        let listed = passkey_rows::list(test.app.pool(), &alice.id)
            .await
            .expect("list");
        assert_eq!(listed[0].name, "Laptop");

        for round in 1..=2 {
            let mut browser = Browser::default();
            let signed = assertion(&test, &mut browser, &mut soft, &alice, "/ui/#/keys").await;
            let reply = finish(&test, &mut browser, signed).await;
            assert_eq!(
                reply.status,
                StatusCode::OK,
                "round {round}: {}",
                reply.text()
            );
            assert_eq!(reply.json()["redirect"], "/ui/#/keys");
            let session = browser
                .send(&test, "GET", "/api/v1/session", Payload::Empty)
                .await;
            assert_eq!(session.json()["user"], "alice");
            // The stored counter follows the authenticator's (1, then 2).
            assert_eq!(stored_counter(&test, &alice).await, round);
        }
        let events = sign_in_events::recent(test.app.pool(), "alice", 5)
            .await
            .expect("events");
        assert!(matches!(
            (&events[0].method, &events[0].outcome),
            (Method::Passkey, Outcome::Ok)
        ));
        assert!(matches!(
            (&events[2].method, &events[2].outcome),
            (Method::Setup, Outcome::Ok)
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_counter_that_did_not_move_is_refused_and_recorded() {
        let test = login_app().await;
        let (alice, mut soft) = alice_with_passkey(&test, None).await;
        sqlx::query(
            "UPDATE user_passkeys SET passkey = jsonb_set(passkey, '{cred,counter}', '1000')",
        )
        .execute(test.app.pool())
        .await
        .expect("inflate the stored counter");
        let mut browser = Browser::default();

        let signed = assertion(&test, &mut browser, &mut soft, &alice, "/ui/").await;
        let reply = finish(&test, &mut browser, signed).await;

        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{}", reply.text());
        assert_eq!(reply.json()["message"], SIGN_IN_FAILED);
        assert_eq!(browser.cookie(COOKIE), None);
        assert_eq!(stored_counter(&test, &alice).await, 1000, "never lowered");
        let events = sign_in_events::recent(test.app.pool(), "alice", 5)
            .await
            .expect("events");
        assert!(matches!(
            (&events[0].method, &events[0].outcome),
            (Method::Passkey, Outcome::Failed)
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_user_handle_naming_another_user_is_refused() {
        let test = login_app().await;
        let (alice, mut soft) = alice_with_passkey(&test, None).await;
        let bob = user_with_password(&test, "bob", "bob@example.com", Some(PASSWORD)).await;
        let mut browser = Browser::default();

        let signed = assertion_as(&test, &mut browser, &mut soft, &alice, &bob.id, "/ui/").await;
        let reply = finish(&test, &mut browser, signed).await;

        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{}", reply.text());
        assert_eq!(browser.cookie(COOKIE), None);
        assert_eq!(sessions_of(&test, &bob).await, 0);
        let events = sign_in_events::recent(test.app.pool(), "alice", 5)
            .await
            .expect("events");
        assert!(matches!(
            (&events[0].method, &events[0].outcome),
            (Method::Passkey, Outcome::Failed)
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_ceremony_works_once_and_only_with_its_cookie() {
        let test = login_app().await;
        let (alice, mut soft) = alice_with_passkey(&test, None).await;
        let mut browser = Browser::default();
        let signed = assertion(&test, &mut browser, &mut soft, &alice, "/ui/").await;
        let ceremony = browser
            .cookie(CEREMONY_COOKIE)
            .expect("a ceremony")
            .to_string();

        let without = finish(&test, &mut Browser::default(), signed.clone()).await;
        let first = finish(&test, &mut browser, signed.clone()).await;
        let replayed = finish(
            &test,
            &mut Browser::with_cookie(CEREMONY_COOKIE, &ceremony),
            signed,
        )
        .await;

        assert_eq!(without.status, StatusCode::GONE);
        assert_eq!(first.status, StatusCode::OK, "{}", first.text());
        assert_eq!(replayed.status, StatusCode::GONE);
        assert_eq!(browser.cookie(CEREMONY_COOKIE), None, "cleared once used");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_ceremony_expires_after_five_minutes_and_is_of_one_kind() {
        let test = login_app().await;
        let passkeys = test.app.passkeys().expect("passkeys are on");
        let start = Instant::now();
        let ceremony = |passkeys: &Passkeys| {
            let (_, state) = passkeys
                .webauthn
                .start_discoverable_authentication()
                .expect("a challenge");
            Ceremony::SignIn {
                state,
                return_to: "/ui/".to_string(),
            }
        };
        // The request a browser sends back with the cookie `remember` set.
        let presenting = |set: HeaderMap| {
            let line = set[header::SET_COOKIE].to_str().expect("text").to_string();
            let pair = line.split(';').next().expect("a pair").to_string();
            let mut headers = HeaderMap::new();
            headers.insert(header::COOKIE, pair.parse().expect("a header"));
            headers
        };
        let late = presenting(
            remember(
                &test.app,
                passkeys,
                &HeaderMap::new(),
                ceremony(passkeys),
                start,
            )
            .expect("remember"),
        );
        let on_time = presenting(
            remember(
                &test.app,
                passkeys,
                &HeaderMap::new(),
                ceremony(passkeys),
                start,
            )
            .expect("remember"),
        );

        let after = take_ceremony(&test.app, passkeys, &late, start + CEREMONY_TTL);
        let within = take_ceremony(&test.app, passkeys, &on_time, start + CEREMONY_TTL / 2);

        assert!(matches!(after, Err(OkfError::Gone { .. })));
        let (taken, clear) = within.expect("a live ceremony");
        assert!(matches!(taken, Ceremony::SignIn { .. }));
        assert!(clear[header::SET_COOKIE]
            .to_str()
            .expect("text")
            .contains("Max-Age=0"));
        // A sign-in ceremony does not finish a registration.
        let mut browser = Browser::default();
        let signed = browser
            .send(
                &test,
                "POST",
                "/auth/passkey/signin/start",
                Payload::Json(json!({})),
            )
            .await;
        assert_eq!(signed.status, StatusCode::OK);
        let credential = json!({
            "id": "AAAA", "rawId": "AAAA", "type": "public-key", "extensions": {},
            "response": { "attestationObject": "AAAA", "clientDataJSON": "AAAA" },
        });
        let crossed = browser
            .send(
                &test,
                "POST",
                "/auth/passkey/setup/finish",
                Payload::Json(credential),
            )
            .await;
        assert_eq!(crossed.status, StatusCode::GONE, "{}", crossed.text());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_hostile_return_to_becomes_the_console() {
        let test = login_app().await;
        let (alice, mut soft) = alice_with_passkey(&test, None).await;

        for hostile in [
            "//evil.example",
            "https://evil.example",
            "/\\evil.example",
            "javascript:alert(1)",
            "%2F%2Fevil.example",
        ] {
            let mut browser = Browser::default();
            let signed = assertion(&test, &mut browser, &mut soft, &alice, hostile).await;
            let reply = finish(&test, &mut browser, signed).await;
            assert_eq!(reply.status, StatusCode::OK, "{hostile}: {}", reply.text());
            assert_eq!(reply.json()["redirect"], "/ui/", "{hostile}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_hostile_return_to_on_the_setup_page_becomes_the_console() {
        let test = login_app().await;
        let alice = user_with_password(&test, "alice", "alice@example.com", None).await;
        let token = setup_token(&test, &alice).await;
        let mut soft = SoftPasskey::new(true);
        let mut browser = Browser::default();
        let body = json!({ "token": token, "name": "Laptop", "return_to": "//evil.example" });

        let start = browser
            .send(
                &test,
                "POST",
                "/auth/passkey/setup/start",
                Payload::Json(body),
            )
            .await;
        let challenge: CreationChallengeResponse =
            serde_json::from_value(start.json()).expect("a challenge");
        let credential = soft
            .do_registration(origin(), challenge)
            .expect("the authenticator registers");
        let done = browser
            .send(
                &test,
                "POST",
                "/auth/passkey/setup/finish",
                Payload::Json(serde_json::to_value(&credential).expect("json")),
            )
            .await;

        assert_eq!(done.status, StatusCode::OK, "{}", done.text());
        assert_eq!(done.json()["redirect"], "/ui/");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_passkey_routes_share_the_per_address_limit() {
        let test = login_app().await;
        let alice = user_with_password(&test, "alice", "alice@example.com", None).await;
        let token = setup_token(&test, &alice).await;
        let mut browser = Browser::default();
        let start = || Payload::Json(json!({}));
        // oneshot requests carry no address, so they share one bucket.
        for attempt in 1..=30 {
            let reply = browser
                .send(&test, "POST", "/auth/passkey/signin/start", start())
                .await;
            assert_eq!(reply.status, StatusCode::OK, "attempt {attempt}");
        }

        let sign_in = browser
            .send(&test, "POST", "/auth/passkey/signin/start", start())
            .await;
        let finish = browser
            .send(
                &test,
                "POST",
                "/auth/passkey/signin/finish",
                Payload::Json(json!({
                    "id": "AAAA", "rawId": "AAAA", "type": "public-key", "extensions": {},
                    "response": {
                        "authenticatorData": "AAAA", "clientDataJSON": "AAAA",
                        "signature": "AAAA", "userHandle": null,
                    },
                })),
            )
            .await;
        let setup = browser
            .send(
                &test,
                "POST",
                "/auth/passkey/setup/start",
                Payload::Json(json!({ "token": token, "name": "Laptop" })),
            )
            .await;

        assert_eq!(sign_in.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(finish.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(setup.status, StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(sign_in.json()["error"], "TooManyRequests");
    }

    /// Sends `uri` with the `Sec-Fetch-Site` another site's page would
    /// carry.
    async fn cross_site_post(test: &TestApp, uri: &str, body: serde_json::Value) -> StatusCode {
        let request = Request::builder()
            .method("POST")
            .uri(uri)
            .header("content-type", "application/json")
            .header("sec-fetch-site", "cross-site")
            .body(Body::from(body.to_string()))
            .expect("a request");
        router(Arc::clone(&test.app), &CancellationToken::new())
            .oneshot(request)
            .await
            .expect("the router answers")
            .status()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_passkey_request_from_another_site_is_refused() {
        let test = login_app().await;
        let alice = user_with_password(&test, "alice", "alice@example.com", None).await;
        let token = setup_token(&test, &alice).await;
        let credential = json!({
            "id": "AAAA", "rawId": "AAAA", "type": "public-key", "extensions": {},
            "response": { "attestationObject": "AAAA", "clientDataJSON": "AAAA" },
        });
        let assertion = json!({
            "id": "AAAA", "rawId": "AAAA", "type": "public-key", "extensions": {},
            "response": {
                "authenticatorData": "AAAA", "clientDataJSON": "AAAA",
                "signature": "AAAA", "userHandle": null,
            },
        });

        let statuses = [
            cross_site_post(
                &test,
                "/auth/passkey/setup/start",
                json!({ "token": token, "name": "Laptop" }),
            )
            .await,
            cross_site_post(&test, "/auth/passkey/setup/finish", credential).await,
            cross_site_post(&test, "/auth/passkey/signin/start", json!({})).await,
            cross_site_post(&test, "/auth/passkey/signin/finish", assertion).await,
        ];

        assert_eq!(statuses, [StatusCode::FORBIDDEN; 4]);
        let link = email_tokens::peek(test.app.pool(), &token, Purpose::Setup, Utc::now()).await;
        assert!(link.is_ok(), "the link is untouched");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_setup_that_fails_stores_no_passkey_and_keeps_the_link() {
        let test = login_app().await;
        let alice = user_with_password(&test, "alice", "alice@example.com", None).await;
        let token = setup_token(&test, &alice).await;
        let pool = test.app.pool();
        // The session insert fails, after the link was used and the passkey
        // stored in the same transaction.
        fail_on(pool, "BEFORE INSERT ON user_sessions FOR EACH ROW").await;
        let mut soft = SoftPasskey::new(true);

        let failed = register_by_setup(&test, &mut Browser::default(), &mut soft, &token).await;
        let link = email_tokens::peek(pool, &token, Purpose::Setup, Utc::now()).await;
        let stored = passkey_rows::list(pool, &alice.id).await.expect("list");
        allow(pool, "user_sessions").await;
        let retried = register_by_setup(&test, &mut Browser::default(), &mut soft, &token).await;

        assert_eq!(failed.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(link.is_ok(), "the link was not used");
        assert!(stored.is_empty(), "no passkey was stored");
        assert_eq!(retried.status, StatusCode::OK, "{}", retried.text());
    }

    /// The trigger that makes ending the browser's old session fail.
    const FAIL_SESSION_DELETE: &str = "BEFORE DELETE ON user_sessions FOR EACH STATEMENT";

    #[tokio::test(flavor = "multi_thread")]
    async fn a_passkey_sign_in_whose_old_session_cannot_end_changes_nothing() {
        let test = login_app().await;
        let (alice, mut soft) = alice_with_passkey(&test, Some(PASSWORD)).await;
        let pool = test.app.pool();
        let mut browser = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let old = browser.cookie(COOKIE).expect("a session").to_string();
        let before = sessions_of(&test, &alice).await;
        fail_on(pool, FAIL_SESSION_DELETE).await;

        let signed = assertion(&test, &mut browser, &mut soft, &alice, "/ui/").await;
        let failed = finish(&test, &mut browser, signed).await;
        let counter = stored_counter(&test, &alice).await;
        let after_failure = sessions_of(&test, &alice).await;
        allow(pool, "user_sessions").await;
        let signed = assertion(&test, &mut browser, &mut soft, &alice, "/ui/").await;
        let retried = finish(&test, &mut browser, signed).await;
        let session = browser
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;

        // All or nothing: the counter, the new session and the old one's
        // end commit together.
        assert_eq!(failed.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(failed.set_cookie(COOKIE).is_none());
        assert_eq!(
            counter, 0,
            "the registration's counter, not the failed sign-in's"
        );
        assert_eq!(after_failure, before);
        assert_eq!(retried.status, StatusCode::OK, "{}", retried.text());
        assert_ne!(browser.cookie(COOKIE), Some(old.as_str()));
        assert_eq!(session.json()["user"], "alice");
        assert_eq!(
            sessions_of(&test, &alice).await,
            before,
            "the old one ended"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_passkey_setup_whose_old_session_cannot_end_keeps_the_link() {
        let test = login_app().await;
        let alice = user_with_password(&test, "alice", "alice@example.com", None).await;
        user_with_password(&test, "bob", "bob@example.com", Some(PASSWORD)).await;
        let token = setup_token(&test, &alice).await;
        let pool = test.app.pool();
        // The browser is still signed in as someone else.
        let mut browser = Browser::signed_in(&test, "bob@example.com", PASSWORD).await;
        let mut soft = SoftPasskey::new(true);
        fail_on(pool, FAIL_SESSION_DELETE).await;

        let failed = register_by_setup(&test, &mut browser, &mut soft, &token).await;
        let link = email_tokens::peek(pool, &token, Purpose::Setup, Utc::now()).await;
        let stored = passkey_rows::list(pool, &alice.id).await.expect("list");
        allow(pool, "user_sessions").await;
        let retried = register_by_setup(&test, &mut browser, &mut soft, &token).await;
        let session = browser
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;

        assert_eq!(failed.status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(failed.set_cookie(COOKIE).is_none());
        assert!(link.is_ok(), "the link was not used");
        assert!(stored.is_empty(), "no passkey was stored");
        assert_eq!(retried.status, StatusCode::OK, "{}", retried.text());
        assert_eq!(session.json()["user"], "alice");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn one_actors_repeated_starts_hold_one_ceremony() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let passkeys = test.app.passkeys().expect("passkeys are on");
        let mut browser = Browser::default();
        let anonymous = || Payload::Json(json!({}));

        // A browser that starts again (its cookie kept) replaces its ceremony.
        for _ in 0..5 {
            let reply = browser
                .send(&test, "POST", "/auth/passkey/signin/start", anonymous())
                .await;
            assert_eq!(reply.status, StatusCode::OK);
        }
        let after_one_browser = passkeys.ceremonies.len();
        let other = Browser::default()
            .send(&test, "POST", "/auth/passkey/signin/start", anonymous())
            .await;
        let after_another = passkeys.ceremonies.len();
        // A session holds one registration, even when it drops the cookie.
        let session = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        for _ in 0..5 {
            let mut cookieless = Browser::with_cookie(COOKIE, session.cookie(COOKIE).expect("id"));
            cookieless.csrf.clone_from(&session.csrf);
            let reply = cookieless
                .send(
                    &test,
                    "POST",
                    "/api/v1/me/passkeys/register/start",
                    Payload::Json(json!({ "name": "Phone" })),
                )
                .await;
            assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
        }
        let after_one_session = passkeys.ceremonies.len();
        let mut second = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let started = second
            .send(
                &test,
                "POST",
                "/api/v1/me/passkeys/register/start",
                Payload::Json(json!({ "name": "Laptop" })),
            )
            .await;

        assert_eq!(after_one_browser, 1);
        assert_eq!(other.status, StatusCode::OK, "others can still start");
        assert_eq!(after_another, 2);
        assert_eq!(after_one_session, 3);
        assert_eq!(started.status, StatusCode::OK, "others can still start");
        assert_eq!(passkeys.ceremonies.len(), 4);
    }

    /// Holds `table` in `ACCESS EXCLUSIVE` mode until the returned
    /// transaction ends, so every statement that touches it waits.
    async fn lock_table(
        pool: &sqlx::PgPool,
        table: &str,
    ) -> sqlx::Transaction<'static, sqlx::Postgres> {
        let mut holder = pool.begin().await.expect("begin");
        // `AssertSqlSafe`: `table` is a literal of the calling test.
        sqlx::query(sqlx::AssertSqlSafe(format!(
            "LOCK TABLE {table} IN ACCESS EXCLUSIVE MODE"
        )))
        .execute(&mut *holder)
        .await
        .expect("lock the table");
        holder
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_sign_in_waiting_on_the_event_table_commits_nothing_until_it_returns_its_cookie() {
        let test = Arc::new(login_app().await);
        let (alice, mut soft) = alice_with_passkey(&test, None).await;
        let before = sessions_of(&test, &alice).await;
        let mut browser = Browser::default();
        let signed = assertion(&test, &mut browser, &mut soft, &alice, "/ui/").await;
        let pool = test.app.pool().clone();
        let holder = lock_table(&pool, "sign_in_events").await;
        let sign_in = tokio::spawn({
            let test = Arc::clone(&test);
            async move {
                let reply = finish(&test, &mut browser, signed).await;
                (reply, browser)
            }
        });
        wait_for_lock_waiters(&pool, 1).await;
        // Blocked on the event, inside its transaction: nothing committed.
        let while_blocked = sessions_of(&test, &alice).await;

        holder.commit().await.expect("release the table");
        let (reply, mut browser) = sign_in.await.expect("the sign-in task");
        let session = browser
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;

        assert_eq!(while_blocked, before);
        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
        assert!(reply.set_cookie(COOKIE).is_some());
        assert_eq!(session.json()["user"], "alice");
        let events = sign_in_events::recent(test.app.pool(), "alice", 1)
            .await
            .expect("events");
        assert!(matches!(
            (&events[0].method, &events[0].outcome),
            (Method::Passkey, Outcome::Ok)
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_registration_waiting_on_the_mail_queue_commits_nothing_until_it_answers() {
        let test = Arc::new(login_app().await);
        let alice = user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let mut browser = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let mut soft = SoftPasskey::new(true);
        let credential =
            registration_from_session(&test, &mut browser, &mut soft, json!({ "name": "Phone" }))
                .await;
        let pool = test.app.pool().clone();
        let holder = lock_table(&pool, "apalis.jobs").await;
        let finish = tokio::spawn({
            let test = Arc::clone(&test);
            async move {
                browser
                    .send(
                        &test,
                        "POST",
                        "/api/v1/me/passkeys/register/finish",
                        Payload::Json(credential),
                    )
                    .await
            }
        });
        wait_for_lock_waiters(&pool, 1).await;
        let while_blocked = passkey_rows::list(&pool, &alice.id).await.expect("list");

        holder.commit().await.expect("release the queue");
        let reply = finish.await.expect("the registration task");

        assert!(
            while_blocked.is_empty(),
            "nothing committed while the notice waited"
        );
        assert_eq!(reply.status, StatusCode::CREATED, "{}", reply.text());
        assert_eq!(
            passkey_rows::list(&pool, &alice.id)
                .await
                .expect("list")
                .len(),
            1
        );
        let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM apalis.jobs")
            .fetch_one(&pool)
            .await
            .expect("count");
        assert_eq!(queued, 1, "the notice committed with the passkey");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_notice_that_cannot_be_queued_does_not_undo_the_passkey() {
        let test = login_app().await;
        let alice = user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let mut browser = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let mut soft = SoftPasskey::new(true);
        let pool = test.app.pool();
        fail_on(pool, "BEFORE INSERT ON apalis.jobs FOR EACH STATEMENT").await;

        let done =
            register_from_session(&test, &mut browser, &mut soft, json!({ "name": "Phone" })).await;
        allow(pool, "apalis.jobs").await;

        assert_eq!(done.status, StatusCode::CREATED, "{}", done.text());
        assert_eq!(
            passkey_rows::list(pool, &alice.id)
                .await
                .expect("list")
                .len(),
            1
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_sign_in_queued_on_a_passkey_removal_gets_no_session() {
        let test = Arc::new(login_app().await);
        let (alice, mut soft) = alice_with_passkey(&test, Some(PASSWORD)).await;
        let before = sessions_of(&test, &alice).await;
        let mut browser = Browser::default();
        let signed = assertion(&test, &mut browser, &mut soft, &alice, "/ui/").await;
        let pool = test.app.pool().clone();
        // A removal in progress, as `passkeys::remove` runs it: the user's
        // row locked first, then the passkey deleted, not yet committed.
        let mut removal = pool.begin().await.expect("begin");
        sign_in_methods::count(&mut removal, &alice.id)
            .await
            .expect("lock alice");
        sqlx::query("DELETE FROM user_passkeys WHERE user_id = $1")
            .bind(&alice.id)
            .execute(&mut *removal)
            .await
            .expect("delete the passkey");
        // `tokio::spawn` runs the sign-in on its own task, like
        // `asyncio.create_task`; `move` hands it owned copies.
        let sign_in = tokio::spawn({
            let test = Arc::clone(&test);
            async move {
                let reply = finish(&test, &mut browser, signed).await;
                (reply, browser)
            }
        });
        wait_for_lock_waiters(&pool, 1).await;

        removal.commit().await.expect("remove the passkey");
        let (reply, browser) = sign_in.await.expect("the sign-in task");

        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{}", reply.text());
        assert_eq!(browser.cookie(COOKIE), None);
        assert_eq!(sessions_of(&test, &alice).await, before, "no new session");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_sign_in_ahead_of_a_reset_has_its_session_ended_by_it() {
        let test = Arc::new(login_app().await);
        let (alice, mut soft) = alice_with_passkey(&test, Some(PASSWORD)).await;
        let mut browser = Browser::default();
        let signed = assertion(&test, &mut browser, &mut soft, &alice, "/ui/").await;
        let pool = test.app.pool().clone();
        // Hold the passkey's row (as another sign-in with it would), so the
        // sign-in stops holding alice's row, and the reset queues behind it.
        let mut holder = pool.begin().await.expect("begin");
        sqlx::query("SELECT 1 FROM user_passkeys WHERE user_id = $1 FOR UPDATE")
            .bind(&alice.id)
            .execute(&mut *holder)
            .await
            .expect("hold the passkey");
        let sign_in = tokio::spawn({
            let test = Arc::clone(&test);
            async move {
                let reply = finish(&test, &mut browser, signed).await;
                (reply, browser)
            }
        });
        wait_for_lock_waiters(&pool, 1).await;
        let reset = tokio::spawn({
            let pool = pool.clone();
            let user_id = alice.id.clone();
            async move { passwords::reset(&pool, &user_id, "amber-quarry-signal-77", Utc::now()).await }
        });
        wait_for_lock_waiters(&pool, 2).await;

        holder.commit().await.expect("release the passkey");
        let (reply, mut browser) = sign_in.await.expect("the sign-in task");
        reset.await.expect("the reset task").expect("reset");
        let session = browser
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;

        assert_eq!(reply.status, StatusCode::OK, "{}", reply.text());
        assert_eq!(
            session.status,
            StatusCode::UNAUTHORIZED,
            "the reset ended it"
        );
        assert_eq!(sessions_of(&test, &alice).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_signed_in_user_adds_a_passkey_and_their_other_sessions_end() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let mut here = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let mut elsewhere = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let mut soft = SoftPasskey::new(true);

        let done =
            register_from_session(&test, &mut here, &mut soft, json!({ "name": "Phone" })).await;

        assert_eq!(done.status, StatusCode::CREATED, "{}", done.text());
        assert_eq!(done.json()["name"], "Phone");
        assert_eq!(
            here.cookie(CEREMONY_COOKIE),
            None,
            "the ceremony cookie is cleared"
        );
        let still = here
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;
        let ended = elsewhere
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;
        assert_eq!(still.status, StatusCode::OK);
        assert_eq!(ended.status, StatusCode::UNAUTHORIZED);
        let queued: i64 = sqlx::query_scalar("SELECT count(*) FROM apalis.jobs")
            .fetch_one(test.app.pool())
            .await
            .expect("count");
        assert_eq!(queued, 1, "the method-added notice");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn keep_sessions_keeps_the_other_sessions() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let mut here = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let mut elsewhere = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let mut soft = SoftPasskey::new(true);
        let body = json!({ "name": "Phone", "keep_sessions": true });

        let done = register_from_session(&test, &mut here, &mut soft, body).await;

        assert_eq!(done.status, StatusCode::CREATED, "{}", done.text());
        let kept = elsewhere
            .send(&test, "GET", "/api/v1/session", Payload::Empty)
            .await;
        assert_eq!(kept.status, StatusCode::OK);
    }

    /// Starts a registration from `browser`'s session and has `soft` answer
    /// it; the finish request is returned unsent.
    async fn registration_from_session(
        test: &TestApp,
        browser: &mut Browser,
        soft: &mut SoftPasskey,
        body: serde_json::Value,
    ) -> serde_json::Value {
        let start = browser
            .send(
                test,
                "POST",
                "/api/v1/me/passkeys/register/start",
                Payload::Json(body),
            )
            .await;
        assert_eq!(start.status, StatusCode::OK, "{}", start.text());
        let challenge: CreationChallengeResponse =
            serde_json::from_value(start.json()).expect("a challenge");
        let credential = soft
            .do_registration(origin(), challenge)
            .expect("the authenticator registers");
        serde_json::to_value(&credential).expect("json")
    }

    async fn register_from_session(
        test: &TestApp,
        browser: &mut Browser,
        soft: &mut SoftPasskey,
        body: serde_json::Value,
    ) -> Reply {
        let credential = registration_from_session(test, browser, soft, body).await;
        browser
            .send(
                test,
                "POST",
                "/api/v1/me/passkeys/register/finish",
                Payload::Json(credential),
            )
            .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_passkey_added_by_a_session_a_reset_ended_is_refused() {
        let test = Arc::new(login_app().await);
        let alice = user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let mut browser = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let mut soft = SoftPasskey::new(true);
        let credential =
            registration_from_session(&test, &mut browser, &mut soft, json!({ "name": "Phone" }))
                .await;
        let pool = test.app.pool().clone();
        let new_password = NewPassword::hash("amber-quarry-signal-77")
            .await
            .expect("hash");
        // A reset that has locked alice's row but not yet ended her
        // sessions: the registration passes the session check of the
        // middleware, then queues on her row.
        let mut reset = pool.begin().await.expect("begin");
        sqlx::query("SELECT 1 FROM users WHERE id = $1 FOR UPDATE")
            .bind(&alice.id)
            .execute(&mut *reset)
            .await
            .expect("lock alice");
        let finish = tokio::spawn({
            let test = Arc::clone(&test);
            async move {
                browser
                    .send(
                        &test,
                        "POST",
                        "/api/v1/me/passkeys/register/finish",
                        Payload::Json(credential),
                    )
                    .await
            }
        });
        wait_for_lock_waiters(&pool, 1).await;

        passwords::reset_in(&mut reset, &alice.id, &new_password, Utc::now())
            .await
            .expect("reset");
        reset.commit().await.expect("commit the reset");
        let reply = finish.await.expect("the registration task");

        assert_eq!(reply.status, StatusCode::UNAUTHORIZED, "{}", reply.text());
        let stored = passkey_rows::list(&pool, &alice.id).await.expect("list");
        assert!(stored.is_empty(), "no passkey outlives the reset");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn registering_from_the_api_needs_a_session_and_its_csrf_token() {
        let test = login_app().await;
        user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let mut browser = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        browser.csrf = None;
        let mut keyed = Browser::default();
        keyed.bearer = Some(test_keys(&test).await.write);
        let body = || Payload::Json(json!({ "name": "Phone" }));

        let no_csrf = browser
            .send(&test, "POST", "/api/v1/me/passkeys/register/start", body())
            .await;
        let key = keyed
            .send(&test, "POST", "/api/v1/me/passkeys/register/start", body())
            .await;
        let key_finish = keyed
            .send(
                &test,
                "POST",
                "/api/v1/me/passkeys/register/finish",
                Payload::Json(json!({})),
            )
            .await;

        assert_eq!(no_csrf.status, StatusCode::FORBIDDEN);
        assert_eq!(key.status, StatusCode::FORBIDDEN);
        assert_ne!(key_finish.status, StatusCode::CREATED);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_ceremony_of_another_session_is_refused() {
        let test = login_app().await;
        let alice = user_with_password(&test, "alice", "alice@example.com", Some(PASSWORD)).await;
        let mut first = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let second = Browser::signed_in(&test, "alice@example.com", PASSWORD).await;
        let mut soft = SoftPasskey::new(true);
        let credential =
            registration_from_session(&test, &mut first, &mut soft, json!({ "name": "Phone" }))
                .await;
        // The second session presents the first one's ceremony cookie.
        let cookies = format!(
            "{COOKIE}={}; {CEREMONY_COOKIE}={}",
            second.cookie(COOKIE).expect("a session"),
            first.cookie(CEREMONY_COOKIE).expect("a ceremony"),
        );
        let request = Request::builder()
            .method("POST")
            .uri("/api/v1/me/passkeys/register/finish")
            .header("content-type", "application/json")
            .header("cookie", cookies)
            .header(
                crate::http::console_session::CSRF_HEADER,
                second.csrf.as_deref().expect("a token"),
            )
            .body(Body::from(credential.to_string()))
            .expect("a request");

        let reply = router(Arc::clone(&test.app), &CancellationToken::new())
            .oneshot(request)
            .await
            .expect("the router answers");

        assert_eq!(reply.status(), StatusCode::FORBIDDEN);
        let stored = passkey_rows::list(test.app.pool(), &alice.id)
            .await
            .expect("list");
        assert!(stored.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_bad_name_is_422() {
        let test = login_app().await;
        let alice = user_with_password(&test, "alice", "alice@example.com", None).await;
        let token = setup_token(&test, &alice).await;

        for name in ["  ".to_string(), "a\u{0}b".to_string(), "x".repeat(65)] {
            let reply = Browser::default()
                .send(
                    &test,
                    "POST",
                    "/auth/passkey/setup/start",
                    Payload::Json(json!({ "token": token, "name": name })),
                )
                .await;
            assert_eq!(reply.status, StatusCode::UNPROCESSABLE_ENTITY, "{name:?}");
            assert_eq!(reply.json()["error"], "LimitExceeded", "{name:?}");
        }
        assert_eq!(passkey_name(&"x".repeat(64)).expect("64 is fine").len(), 64);
        assert_eq!(passkey_name(" Laptop ").expect("trimmed"), "Laptop");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn passkeys_are_off_for_an_address_public_url() {
        let test = app_with(
            &[("main", MAIN)],
            Config {
                public_url: Some("http://127.0.0.1:8080".to_string()),
                ..Config::default()
            },
        )
        .await;
        let mut browser = Browser::default();

        let start = browser
            .send(
                &test,
                "POST",
                "/auth/passkey/signin/start",
                Payload::Json(json!({})),
            )
            .await;
        let page = browser.send(&test, "GET", "/signin", Payload::Empty).await;

        assert!(test.app.passkeys().is_none());
        assert_eq!(start.status, StatusCode::NOT_FOUND);
        assert!(!page.text().contains("signin-passkey"));
        assert!(!page.text().contains("<script"));
        assert!(page.text().contains("signin-form"), "passwords still work");
        assert!(Passkeys::new(&login_config()).expect("a domain").is_some());
        assert!(Passkeys::new(&Config::default()).expect("no url").is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_sign_in_and_setup_pages_offer_a_passkey_and_load_its_script() {
        let test = login_app().await;
        let alice = user_with_password(&test, "alice", "alice@example.com", None).await;
        let token = setup_token(&test, &alice).await;
        let mut browser = Browser::default();

        let signin = browser
            .send(&test, "GET", "/signin", Payload::Empty)
            .await
            .text();
        let setup = browser
            .send(
                &test,
                "GET",
                &format!("/account/setup?token={token}"),
                Payload::Empty,
            )
            .await
            .text();
        let script = browser
            .send(&test, "GET", "/ui/js/signin/passkey.js", Payload::Empty)
            .await;

        for html in [&signin, &setup] {
            assert!(
                html.contains(r#"<script type="module" src="/ui/js/signin/passkey.js"></script>"#)
            );
        }
        assert!(signin.contains(r#"id="signin-passkey""#));
        assert!(setup.contains(r#"id="setup-passkey""#));
        assert_eq!(script.status, StatusCode::OK);
        assert!(script.text().contains("./webauthn.js"));
    }
}

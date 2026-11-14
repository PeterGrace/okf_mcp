//! `/api/v1/me/...` (spec "Self-service"): what a signed-in person does for
//! themselves. They manage their own keys and OAuth grants, change their
//! password, manage their passkeys and linked identities, and see their
//! sessions, recent sign-ins, teams and ceiling.
//!
//! Access:
//! - every route but `GET /me` needs a console session
//!   ([`CredentialKind::Session`]); a bearer key or OAuth token is a 403,
//!   since a key must not mint keys for itself;
//! - `GET /me` also answers a key that belongs to a user;
//! - a route acts only on the session user's own rows: anyone else's key,
//!   grant, passkey, identity or session is a 404, like an unknown id.
//!
//! Self-issued keys (Decisions 3 and 4) are never admin or reporters, name
//! their catalogs, stay within the ceiling, the key limit and the lifetime
//! limit, and belong to the signed-in user. Each refusal is a 422
//! [`OkfError::LimitExceeded`] naming the limit.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{delete, get, patch, put};
use axum::{Extension, Json, Router};
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::auth::{CredentialKind, Principal};
use crate::clock::format_timestamp;
use crate::error::OkfError;
use crate::http::api::keys::{parse_access, CreatedKeyView, KeyView};
use crate::http::api::settle;
use crate::http::console_session::ConsoleSession;
use crate::http::passkeys::passkey_name;
use crate::http::signin::notify_method_added;
use crate::http::AppError;
use crate::store::ceiling::{self, Ceiling};
use crate::store::keys::{
    self as key_rows, Access, CatalogGrant, Grants, KeyKind, KeyRecord, NewKey,
};
use crate::store::passwords::{self, NewPassword, SessionChange, SignInOutcome};
use crate::store::{identities, passkeys, sign_in_events, user_sessions, users};

/// How many recent sign-ins `GET /me/sign-ins` and the admin's
/// `GET /users/{name}/sign-ins` show.
pub(crate) const RECENT_SIGN_INS: i64 = 50;

/// The signed-in user a self-service request acts for.
// `Copy`: two borrowed strings, cheap to pass by value.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SessionUser<'a> {
    /// `users.id`, which rows reference.
    id: &'a str,
    /// The user's name, which the store's by-name functions take.
    name: &'a str,
}

/// The session user behind `principal`.
///
/// # Arguments
/// * `principal` - the caller, from `require_principal`.
///
/// # Returns
/// The user's id and name.
///
/// # Errors
/// [`OkfError::Forbidden`] for a bearer key or OAuth token.
fn session_user(principal: &Principal) -> Result<SessionUser<'_>, OkfError> {
    // A tuple match checks both at once; every combination is listed, so a
    // new credential kind fails to compile here until someone decides.
    match (&principal.kind, principal.user.as_deref()) {
        (CredentialKind::Session { user_id }, Some(name)) => Ok(SessionUser { id: user_id, name }),
        (
            CredentialKind::ApiKey | CredentialKind::OAuth { .. } | CredentialKind::Session { .. },
            _,
        ) => Err(needs_session()),
    }
}

/// The 403 for a self-service route called with a key or an OAuth token.
fn needs_session() -> OkfError {
    OkfError::Forbidden {
        reason: "this needs a console session: sign in at /signin \
                 (a key cannot manage keys for itself)"
            .to_string(),
    }
}

/// The session user behind `principal`, and the console session the
/// request came with (for routes that act on "the other sessions").
///
/// # Arguments
/// * `principal` - the caller, from `require_principal`.
/// * `session` - the request's [`ConsoleSession`] extension, if any.
///
/// # Returns
/// The user and the session.
///
/// # Errors
/// [`OkfError::Forbidden`] for a bearer key or OAuth token.
fn session_and_user(
    principal: &Principal,
    session: Option<Extension<ConsoleSession>>,
) -> Result<(SessionUser<'_>, ConsoleSession), OkfError> {
    let user = session_user(principal)?;
    // `Option<Extension<T>>` is `None` when the middleware put no such
    // extension in the request; a session principal always has one.
    let Some(Extension(session)) = session else {
        return Err(needs_session());
    };
    Ok((user, session))
}

/// One catalog of a ceiling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CeilingEntry {
    /// The catalog's name.
    pub catalog: String,
    /// `read` or `write`.
    pub access: &'static str,
}

/// A ceiling as the API shows it: one entry per catalog, by name.
///
/// # Arguments
/// * `ceiling` - the user's ceiling.
///
/// # Returns
/// The entries, sorted by catalog (a `BTreeMap` iterates in key order).
pub(crate) fn ceiling_view(ceiling: Ceiling) -> Vec<CeilingEntry> {
    ceiling
        .into_iter()
        .map(|(catalog, access)| CeilingEntry {
            catalog,
            access: access.as_str(),
        })
        .collect()
}

/// `GET /me`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AccountView {
    /// The user's name.
    pub user: String,
    /// Free text, possibly empty.
    pub display_name: String,
    /// Where setup, reset and notice mail goes.
    pub email: Option<String>,
    /// An admin user (its console session acts as an admin).
    pub admin: bool,
    /// Whether a password is set.
    pub has_password: bool,
    /// The user's teams, sorted, its personal team included.
    pub teams: Vec<String>,
    /// What its self-issued credentials may reach.
    pub ceiling: Vec<CeilingEntry>,
    /// Whether the caller is a console session (`false` for a key).
    pub session: bool,
    /// Whether password sign-in is on (the `password_sign_in` setting), so
    /// the account page can say why a password does not sign in.
    pub password_sign_in: bool,
    /// The self-issued key limits, so My keys can offer only what the API
    /// accepts.
    pub limits: AccountLimits,
}

/// The self-issued key limits (the policy settings).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct AccountLimits {
    /// The most live self-issued keys (`max_keys_per_user`).
    pub max_keys_per_user: u32,
    /// The longest expiry in seconds (`max_key_lifetime`), or `None` for no
    /// limit. Seconds, so the console needs no duration parser.
    pub max_key_lifetime_secs: Option<u64>,
}

/// `GET /me/keys`' answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct OwnKeys {
    /// The user's API keys, revoked ones included.
    pub keys: Vec<KeyView>,
}

/// `GET /me/grants`' answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct OwnGrants {
    /// The user's OAuth grants, revoked ones included.
    pub grants: Vec<KeyView>,
}

/// One passkey, never its public key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PasskeyView {
    /// Its credential id, unpadded base64url (the `{id}` of the other routes).
    pub id: String,
    /// The name its owner gave it.
    pub name: String,
    /// When it was registered.
    pub created_at: String,
    /// When it last signed in.
    pub last_used_at: Option<String>,
}

impl From<passkeys::PasskeyInfo> for PasskeyView {
    fn from(info: passkeys::PasskeyInfo) -> Self {
        // Destructured, so a new field fails to compile here until someone
        // decides whether the API shows it.
        let passkeys::PasskeyInfo {
            id,
            name,
            created_at,
            last_used_at,
        } = info;
        Self {
            id,
            name,
            created_at: format_timestamp(created_at),
            last_used_at: last_used_at.map(format_timestamp),
        }
    }
}

/// `GET /me/passkeys`' answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct PasskeyList {
    /// The user's passkeys, oldest first.
    pub passkeys: Vec<PasskeyView>,
}

/// One linked OIDC identity, never its subject.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct IdentityView {
    /// The provider's id (the `{provider}` of the unlink route).
    pub provider: String,
    /// The provider's email when it was linked (may be empty).
    pub email_at_link: String,
    /// When it was linked.
    pub created_at: String,
    /// When it last signed in.
    pub last_used_at: Option<String>,
}

impl From<identities::IdentityInfo> for IdentityView {
    fn from(info: identities::IdentityInfo) -> Self {
        let identities::IdentityInfo {
            provider,
            email_at_link,
            created_at,
            last_used_at,
        } = info;
        Self {
            provider,
            email_at_link,
            created_at: format_timestamp(created_at),
            last_used_at: last_used_at.map(format_timestamp),
        }
    }
}

/// A configured OIDC provider, never its secret.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ProviderView {
    /// The provider's id (the `{provider}` of the link and unlink routes).
    pub id: String,
    /// Its button label.
    pub label: String,
}

/// `GET /me/identities`' answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct IdentityList {
    /// The user's identities, by provider.
    pub identities: Vec<IdentityView>,
    /// Every configured provider, for the account page's Link buttons.
    pub providers: Vec<ProviderView>,
}

/// One console session, never its id or CSRF token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SessionView {
    /// The first 12 hex characters of the stored hash: enough to revoke
    /// it, never enough to sign in.
    pub id_prefix: String,
    /// When it started.
    pub created_at: String,
    /// When it was last used (to the minute).
    pub last_seen_at: String,
    /// The address it started from, if known.
    pub ip: Option<String>,
    /// The browser it started in.
    pub user_agent: String,
    /// Whether it is the session making this request.
    pub current: bool,
}

impl From<user_sessions::SessionInfo> for SessionView {
    fn from(info: user_sessions::SessionInfo) -> Self {
        let user_sessions::SessionInfo {
            id_prefix,
            created_at,
            last_seen_at,
            ip,
            user_agent,
            current,
        } = info;
        Self {
            id_prefix,
            created_at: format_timestamp(created_at),
            last_seen_at: format_timestamp(last_seen_at),
            // `IpAddr` prints as `192.0.2.1` or an IPv6 literal.
            ip: ip.map(|address| address.to_string()),
            user_agent,
            current,
        }
    }
}

/// A list of sessions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SessionList {
    /// The sessions, most recently used first.
    pub sessions: Vec<SessionView>,
}

/// One sign-in attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SignInView {
    /// `password`, `passkey`, `oidc` or `setup`.
    pub method: &'static str,
    /// `ok`, `failed` or `locked`.
    pub outcome: &'static str,
    /// The address it came from, if known.
    pub ip: Option<String>,
    /// When it happened.
    pub at: String,
}

impl From<sign_in_events::SignInEvent> for SignInView {
    fn from(event: sign_in_events::SignInEvent) -> Self {
        let sign_in_events::SignInEvent {
            method,
            outcome,
            ip,
            at,
        } = event;
        Self {
            method: method.as_str(),
            outcome: outcome.as_str(),
            ip: ip.map(|address| address.to_string()),
            at: format_timestamp(at),
        }
    }
}

/// A list of sign-in attempts.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SignInList {
    /// Newest first.
    pub sign_ins: Vec<SignInView>,
}

/// One catalog and level in `POST /me/keys`' body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct CatalogLevelBody {
    /// The catalog's name.
    pub catalog: String,
    /// `read` or `write` (`admin` is refused as over the ceiling).
    pub access: String,
}

/// `POST /me/keys`' body. There is no `all` and no `maintenance_report`:
/// `deny_unknown_fields` makes either a 422.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateKeyBody {
    /// Grouping name, `[A-Za-z0-9._-]+`.
    name: String,
    /// The catalogs, each with its level.
    catalogs: Vec<CatalogLevelBody>,
    /// The default catalog (empty or absent: none); must be one of `catalogs`.
    default_catalog: Option<String>,
    /// The actor (empty or absent: the name).
    actor: Option<String>,
    /// RFC 3339 expiry (empty or absent: the lifetime limit, or none).
    expires_at: Option<String>,
}

/// The rename bodies of `PATCH /me/keys/{id}` and `PATCH /me/passkeys/{id}`.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameBody {
    /// The new name.
    name: String,
}

/// `PUT /me/password`'s body.
// `Debug` by hand (Python's `__repr__`): neither password may reach a log
// line.
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordBody {
    /// The password now set; required when there is one.
    current_password: Option<String>,
    /// The new password (12 to 1024 characters, not a common one).
    new_password: String,
    /// Keep the user's other sessions (default: end them).
    #[serde(default)]
    keep_sessions: bool,
}

impl std::fmt::Debug for PasswordBody {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PasswordBody")
            .field("current_password", &"<redacted>")
            .field("new_password", &"<redacted>")
            .field("keep_sessions", &self.keep_sessions)
            .finish()
    }
}

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/me", get(account))
        .route("/me/keys", get(list_keys).post(create_key))
        .route("/me/keys/{id}", patch(rename_key).delete(revoke_key))
        .route("/me/grants", get(list_grants))
        .route("/me/grants/{id}", delete(revoke_grant))
        .route("/me/password", put(change_password))
        .route("/me/passkeys", get(list_passkeys))
        .route(
            "/me/passkeys/{id}",
            patch(rename_passkey).delete(remove_passkey),
        )
        .route("/me/identities", get(list_identities))
        .route("/me/identities/{provider}", delete(unlink_identity))
        .route("/me/sessions", get(list_sessions))
        .route("/me/sessions/{id_prefix}", delete(revoke_session))
        .route("/me/sign-ins", get(list_sign_ins))
}

/// The levels a create body asks for, by catalog.
///
/// # Arguments
/// * `entries` - the body's `catalogs`.
///
/// # Returns
/// Each catalog's level.
///
/// # Errors
/// [`OkfError::InvalidPath`] (400) for no catalogs, a catalog given twice,
/// or an unknown level.
pub(super) fn requested_levels(
    entries: &[CatalogLevelBody],
) -> Result<BTreeMap<String, Access>, OkfError> {
    if entries.is_empty() {
        return Err(OkfError::InvalidPath {
            path: "catalogs".to_string(),
            reason: "name at least one catalog".to_string(),
        });
    }
    let mut levels = BTreeMap::new();
    for entry in entries {
        let access = parse_access(&entry.access)?;
        // `insert` returns the value it replaced, like `dict.setdefault`
        // inverted: `Some` means the catalog was already there.
        if levels.insert(entry.catalog.clone(), access).is_some() {
            return Err(OkfError::InvalidPath {
                path: "catalogs".to_string(),
                reason: format!("{:?} is given twice", entry.catalog),
            });
        }
    }
    Ok(levels)
}

/// Refuses levels above the ceiling (the ceiling never holds `admin`, so
/// an admin level is always refused).
///
/// # Arguments
/// * `levels` - what the key asks for.
/// * `ceiling` - the user's ceiling.
///
/// # Errors
/// [`OkfError::LimitExceeded`] with `limit: "ceiling"`, naming the first
/// catalog that is outside it or above its level.
pub(super) fn within_ceiling(
    levels: &BTreeMap<String, Access>,
    ceiling: &Ceiling,
) -> Result<(), OkfError> {
    for (catalog, &wanted) in levels {
        match ceiling.get(catalog) {
            Some(&allowed) if wanted <= allowed => {}
            Some(&allowed) => {
                return Err(OkfError::LimitExceeded {
                    limit: "ceiling",
                    reason: format!("your access to {catalog} is at most {}", allowed.as_str()),
                })
            }
            None => {
                return Err(OkfError::LimitExceeded {
                    limit: "ceiling",
                    reason: format!("{catalog} is not among the catalogs you may use"),
                })
            }
        }
    }
    Ok(())
}

/// A new self-issued key's expiry.
///
/// # Arguments
/// * `requested` - the body's `expires_at` (RFC 3339; empty means none).
/// * `max_lifetime` - the `max_key_lifetime` setting.
/// * `now` - the server clock.
///
/// # Returns
/// The requested expiry; with none requested, `now + max_lifetime` when the
/// setting is set, else no expiry.
///
/// # Errors
/// [`OkfError::InvalidPath`] (400) for a malformed or past time;
/// [`OkfError::LimitExceeded`] with `limit: "max_key_lifetime"` past the limit.
pub(super) fn key_expiry(
    requested: Option<&str>,
    max_lifetime: Option<std::time::Duration>,
    now: DateTime<Utc>,
) -> Result<Option<DateTime<Utc>>, OkfError> {
    // `transpose` turns `Option<Result<T, E>>` into `Result<Option<T>, E>`,
    // so `?` can apply to the parse only when there was something to parse.
    let requested = requested
        .filter(|text| !text.is_empty())
        .map(|text| {
            DateTime::parse_from_rfc3339(text)
                .map(|at| at.with_timezone(&Utc))
                .map_err(|_| OkfError::InvalidPath {
                    path: "expires_at".to_string(),
                    reason: format!("must be an RFC 3339 time, not {text:?}"),
                })
        })
        .transpose()?;
    if requested.is_some_and(|at| at <= now) {
        return Err(OkfError::InvalidPath {
            path: "expires_at".to_string(),
            reason: "must be in the future".to_string(),
        });
    }
    // `None` when the setting is unset, or too large to represent as a
    // date: either way there is no limit.
    let latest = max_lifetime
        .and_then(|max| TimeDelta::from_std(max).ok())
        .and_then(|max| now.checked_add_signed(max));
    match (requested, latest) {
        (Some(at), Some(latest)) if at > latest => Err(OkfError::LimitExceeded {
            limit: "max_key_lifetime",
            reason: format!(
                "a new key may live until {} at the latest",
                format_timestamp(latest)
            ),
        }),
        (Some(at), _) => Ok(Some(at)),
        (None, latest) => Ok(latest),
    }
}

/// The user's own credential of `kind` with this id.
///
/// # Arguments
/// * `app` - the application.
/// * `user` - the session user.
/// * `kind` - an API key or an OAuth grant.
/// * `id` - the credential's id.
///
/// # Returns
/// Its record.
///
/// # Errors
/// [`OkfError::NotFound`] for an unknown id or another user's credential
/// (worded alike); [`OkfError::Database`].
async fn own(
    app: &App,
    user: SessionUser<'_>,
    kind: KeyKind,
    id: &str,
) -> Result<KeyRecord, OkfError> {
    key_rows::list(app.pool())
        .await?
        .into_iter()
        .find(|record| {
            record.id == id && record.kind == kind && record.user_id.as_deref() == Some(user.id)
        })
        .ok_or_else(|| OkfError::NotFound {
            path: format!(
                "me/{}/{id}",
                if kind == KeyKind::Key {
                    "keys"
                } else {
                    "grants"
                }
            ),
        })
}

/// Every credential of `kind` belonging to the user.
///
/// # Arguments
/// * `app` - the application.
/// * `user` - the session user.
/// * `kind` - API keys or OAuth grants.
///
/// # Returns
/// Their views, revoked ones included, in the store's order.
///
/// # Errors
/// [`OkfError::Database`].
async fn owned(app: &App, user: SessionUser<'_>, kind: KeyKind) -> Result<Vec<KeyView>, OkfError> {
    Ok(key_rows::list(app.pool())
        .await?
        .into_iter()
        .filter(|record| record.kind == kind && record.user_id.as_deref() == Some(user.id))
        .map(KeyView::from)
        .collect())
}

/// `GET /me`: the account behind the caller: a session's user, or the user
/// a key belongs to.
///
/// # Errors
/// 404 for a key that belongs to no user; 500 for a database failure.
async fn account(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
) -> Result<Json<AccountView>, AppError> {
    let Some(name) = principal.user.as_deref() else {
        return Err(OkfError::NotFound {
            path: "me".to_string(),
        }
        .into());
    };
    let record = users::find(app.pool(), name).await?;
    let ceiling = ceiling::ceiling_of(app.pool(), name).await?;
    let policy = app.settings();
    Ok(Json(AccountView {
        user: record.name,
        display_name: record.display_name,
        email: record.email,
        admin: record.is_admin,
        has_password: record.has_password,
        teams: record.teams,
        ceiling: ceiling_view(ceiling),
        session: matches!(principal.kind, CredentialKind::Session { .. }),
        password_sign_in: policy.password_sign_in,
        limits: AccountLimits {
            max_keys_per_user: policy.max_keys_per_user,
            max_key_lifetime_secs: policy.max_key_lifetime.map(|lifetime| lifetime.as_secs()),
        },
    }))
}

/// `GET /me/keys`: the user's API keys.
///
/// # Errors
/// 403 for a bearer caller; 500 for a database failure.
async fn list_keys(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
) -> Result<Json<OwnKeys>, AppError> {
    let user = session_user(&principal)?;
    Ok(Json(OwnKeys {
        keys: owned(&app, user, KeyKind::Key).await?,
    }))
}

/// `POST /me/keys`: creates a self-issued key and returns its secret, once.
///
/// # Errors
/// 403 for a bearer caller; 400 for no catalogs, a repeated catalog, an
/// unknown level or a bad expiry; 409 for a bad or taken name or a default
/// catalog that is not granted; 422 above the ceiling, past the key limit
/// or the lifetime limit, or for an unknown body field such as `all`.
async fn create_key(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Json(body): Json<CreateKeyBody>,
) -> Result<Json<CreatedKeyView>, AppError> {
    let user = session_user(&principal)?;
    let levels = requested_levels(&body.catalogs)?;
    let ceiling = ceiling::ceiling_of(app.pool(), user.name).await?;
    within_ceiling(&levels, &ceiling)?;
    let now = app.clock().now();
    // The cached policy (Task 5), not a query per request.
    let policy = app.settings();
    let expires_at = key_expiry(body.expires_at.as_deref(), policy.max_key_lifetime, now)?;
    let default_catalog = body
        .default_catalog
        .as_deref()
        .filter(|name| !name.is_empty());
    let actor = body
        .actor
        .as_deref()
        .map(str::trim)
        .filter(|actor| !actor.is_empty())
        .unwrap_or(&body.name);
    let names: Vec<String> = levels.keys().cloned().collect();
    let grants: Vec<CatalogGrant> = levels
        .iter()
        .map(|(catalog, &access)| CatalogGrant {
            catalog: catalog.clone(),
            access: Some(access),
        })
        .collect();
    // The key's own level is its highest catalog level (Decision 1);
    // `requested_levels` refused an empty list, so the fallback never shows.
    let access = levels.values().copied().max().unwrap_or(Access::Read);
    let new = NewKey {
        name: &body.name,
        access,
        maintenance_report: false,
        grants: Grants::Catalogs(&names),
        default_catalog,
        actor,
        user: user.name,
        self_issued: true,
        expires_at,
        levels: &grants,
    };
    let created =
        key_rows::create_self_issued(app.pool(), &new, policy.max_keys_per_user, now).await?;
    tracing::info!(user = %user.name, key = %created.id, "self-issued key created");
    settle(&app).await;
    let key = own(&app, user, KeyKind::Key, &created.id).await?;
    Ok(Json(CreatedKeyView {
        key: key.into(),
        secret: created.key,
    }))
}

/// `PATCH /me/keys/{id}`: renames one of the user's live keys.
///
/// # Errors
/// 403 for a bearer caller; 404 for another user's key or an unknown id;
/// 409 for a bad or taken name or a revoked key; 422 without `name`.
async fn rename_key(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(id): Path<String>,
    Json(body): Json<RenameBody>,
) -> Result<Json<KeyView>, AppError> {
    let user = session_user(&principal)?;
    let target = own(&app, user, KeyKind::Key, &id).await?;
    key_rows::rename_by_id(app.pool(), &target.id, &body.name).await?;
    Ok(Json(
        own(&app, user, KeyKind::Key, &target.id).await?.into(),
    ))
}

/// `DELETE /me/keys/{id}`: revokes one of the user's keys; it
/// authenticates nothing from the next request on.
///
/// # Returns
/// The key as it now is.
///
/// # Errors
/// 403 for a bearer caller; 404 for another user's key or an unknown id;
/// 409 if it is already revoked.
async fn revoke_key(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(id): Path<String>,
) -> Result<Json<KeyView>, AppError> {
    revoke_own(&app, &principal, KeyKind::Key, &id).await
}

/// `GET /me/grants`: the user's OAuth grants.
///
/// # Errors
/// 403 for a bearer caller; 500 for a database failure.
async fn list_grants(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
) -> Result<Json<OwnGrants>, AppError> {
    let user = session_user(&principal)?;
    Ok(Json(OwnGrants {
        grants: owned(&app, user, KeyKind::OAuth).await?,
    }))
}

/// `DELETE /me/grants/{id}`: revokes one of the user's grants, and with it
/// every token issued under it.
///
/// # Errors
/// As [`revoke_key`].
async fn revoke_grant(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(id): Path<String>,
) -> Result<Json<KeyView>, AppError> {
    revoke_own(&app, &principal, KeyKind::OAuth, &id).await
}

/// The body of [`revoke_key`] and [`revoke_grant`]: revokes the session
/// user's own credential and reloads the key cache, so it stops working at
/// once.
///
/// # Arguments
/// * `app` - the application.
/// * `principal` - the caller.
/// * `kind` - an API key or an OAuth grant.
/// * `id` - the credential's id.
///
/// # Returns
/// The credential as it now is.
///
/// # Errors
/// 403 for a bearer caller; 404 for another user's credential or an
/// unknown id; 409 if it is already revoked.
async fn revoke_own(
    app: &App,
    principal: &Principal,
    kind: KeyKind,
    id: &str,
) -> Result<Json<KeyView>, AppError> {
    let user = session_user(principal)?;
    let target = own(app, user, kind, id).await?;
    key_rows::revoke_by_id(app.pool(), &target.id, app.clock().now()).await?;
    tracing::info!(user = %user.name, credential = %target.id, "self-service revoke");
    settle(app).await;
    Ok(Json(own(app, user, kind, &target.id).await?.into()))
}

/// `PUT /me/password`: sets or changes the user's password. The current
/// password is required when one is set. The change, the end of the
/// user's other sessions (unless `keep_sessions`) and the notice mail are
/// one transaction, which also refuses a session that ended, or a
/// password that changed, after the current password was checked.
///
/// # Returns
/// 204.
///
/// # Errors
/// 403 for a bearer caller, a wrong current password, a locked account or
/// a password that changed meanwhile (never 401 for these, which the
/// console reads as "signed out"); 401 once this session has ended; 422
/// for a password the policy refuses (`limit: "password"`) or a body
/// without `new_password`.
async fn change_password(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    session: Option<Extension<ConsoleSession>>,
    Json(body): Json<PasswordBody>,
) -> Result<StatusCode, AppError> {
    let (user, session) = session_and_user(&principal, session)?;
    // The cheap policy check first, so a refused password costs no argon2
    // check and counts no failure.
    passwords::check_policy(&body.new_password)?;
    let now = app.clock().now();
    let record = users::find(app.pool(), user.name).await?;
    let replaces = if record.has_password {
        let current = body.current_password.as_deref().unwrap_or_default();
        match passwords::verify_user(app.pool(), user.id, current, now).await? {
            SignInOutcome::Ok(verified) => Some(verified.stamp),
            SignInOutcome::Failed => {
                return Err(OkfError::Forbidden {
                    reason: "the current password is incorrect".to_string(),
                }
                .into())
            }
            SignInOutcome::Locked => {
                return Err(OkfError::Forbidden {
                    reason: "too many wrong passwords; try again in a few minutes".to_string(),
                }
                .into())
            }
        }
    } else {
        None
    };
    // Hashed before the transaction opens, never while it holds the lock.
    let password = NewPassword::hash(&body.new_password).await?;
    // For the notice's address, from the database: the key cache can be a
    // poll interval behind an email change, and the notice must reach the
    // address the account has now.
    let account = users::find_auth(app.pool(), user.id)
        .await?
        .ok_or_else(|| OkfError::Unauthenticated {
            reason: "this session's user no longer exists".to_string(),
        })?;
    let change = SessionChange {
        user_id: user.id,
        session_id: &session.id,
        replaces: replaces.as_ref(),
        password: &password,
        keep_sessions: body.keep_sessions,
        now,
    };
    let ended = store_password_change(&app, &change, &account).await?;
    tracing::info!(user = %user.name, ended, "password changed from the account page");
    Ok(StatusCode::NO_CONTENT)
}

/// Stores a password change, ends the other sessions unless kept and
/// queues the notice, in one transaction; nothing runs after its commit
/// but building the response.
///
/// # Arguments
/// * `app` - the application.
/// * `change` - the change.
/// * `account` - the user, for the notice's address.
///
/// # Returns
/// How many other sessions ended.
///
/// # Errors
/// As [`passwords::change_from_session_in`]; [`OkfError::Database`].
async fn store_password_change(
    app: &App,
    change: &SessionChange<'_>,
    account: &users::UserAuth,
) -> Result<u64, OkfError> {
    let mut tx = app.pool().begin().await?;
    let ended = passwords::change_from_session_in(&mut tx, change).await?;
    notify_method_added(app, &mut tx, account, "a new password").await?;
    tx.commit().await?;
    Ok(ended)
}

/// `GET /me/passkeys`: the user's passkeys.
///
/// # Errors
/// 403 for a bearer caller; 500 for a database failure.
async fn list_passkeys(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
) -> Result<Json<PasskeyList>, AppError> {
    let user = session_user(&principal)?;
    let passkeys = passkeys::list(app.pool(), user.id)
        .await?
        .into_iter()
        .map(PasskeyView::from)
        .collect();
    Ok(Json(PasskeyList { passkeys }))
}

/// `PATCH /me/passkeys/{id}`: renames one of the user's passkeys.
///
/// # Returns
/// 204.
///
/// # Errors
/// 403 for a bearer caller; 404 for an unknown id or another user's
/// passkey; 422 for a name that is empty, over 64 characters or holds a
/// control character (`limit: "passkey_name"`).
async fn rename_passkey(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(id): Path<String>,
    Json(body): Json<RenameBody>,
) -> Result<StatusCode, AppError> {
    let user = session_user(&principal)?;
    let name = passkey_name(&body.name)?;
    passkeys::rename(app.pool(), user.id, &id, &name).await?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /me/passkeys/{id}`: removes one of the user's passkeys. The
/// last-method check runs under the user's row lock, so two removals at
/// once cannot both pass it.
///
/// # Returns
/// 204.
///
/// # Errors
/// 403 for a bearer caller; 404 for an unknown id or another user's
/// passkey; 409 when it is the user's last sign-in method.
async fn remove_passkey(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(id): Path<String>,
) -> Result<StatusCode, AppError> {
    let user = session_user(&principal)?;
    passkeys::remove(app.pool(), user.id, &id).await?;
    tracing::info!(user = %user.name, "passkey removed from the account page");
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /me/identities`: the user's linked OIDC identities, and the
/// configured providers.
///
/// # Errors
/// 403 for a bearer caller; 500 for a database failure.
async fn list_identities(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
) -> Result<Json<IdentityList>, AppError> {
    let user = session_user(&principal)?;
    let identities = identities::list(app.pool(), user.id)
        .await?
        .into_iter()
        .map(IdentityView::from)
        .collect();
    // `oidc()` is `None` without providers; `labels()` lists them by id.
    let providers = app.oidc().map_or_else(Vec::new, |oidc| {
        oidc.labels()
            .map(|(id, label)| ProviderView {
                id: id.to_string(),
                label: label.to_string(),
            })
            .collect()
    });
    Ok(Json(IdentityList {
        identities,
        providers,
    }))
}

/// `DELETE /me/identities/{provider}`: unlinks the user's identity at that
/// provider. The last-method check runs under the user's row lock.
///
/// # Returns
/// 204.
///
/// # Errors
/// 403 for a bearer caller; 404 when none is linked there (another
/// user's included); 409 when it is the user's last sign-in method.
async fn unlink_identity(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(provider): Path<String>,
) -> Result<StatusCode, AppError> {
    let user = session_user(&principal)?;
    identities::unlink(app.pool(), user.id, &provider).await?;
    tracing::info!(user = %user.name, %provider, "identity unlinked from the account page");
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /me/sessions`: the user's console sessions, the current one marked.
///
/// # Errors
/// 403 for a bearer caller; 500 for a database failure.
async fn list_sessions(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    session: Option<Extension<ConsoleSession>>,
) -> Result<Json<SessionList>, AppError> {
    let (user, session) = session_and_user(&principal, session)?;
    let sessions = user_sessions::list(app.pool(), user.id, Some(&session.id))
        .await?
        .into_iter()
        .map(SessionView::from)
        .collect();
    Ok(Json(SessionList { sessions }))
}

/// `DELETE /me/sessions/{id_prefix}`: ends one of the user's sessions (the
/// current one too, which signs this browser out).
///
/// # Returns
/// 204.
///
/// # Errors
/// 403 for a bearer caller; 404 for a malformed prefix or one that matches
/// none of the user's sessions (another user's included); 409 for a prefix
/// that matches several.
async fn revoke_session(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(id_prefix): Path<String>,
) -> Result<StatusCode, AppError> {
    let user = session_user(&principal)?;
    user_sessions::revoke_by_prefix(app.pool(), user.id, &id_prefix).await?;
    tracing::info!(user = %user.name, %id_prefix, "session ended from the account page");
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /me/sign-ins`: the user's recent sign-in attempts, newest first.
///
/// # Errors
/// 403 for a bearer caller; 500 for a database failure.
async fn list_sign_ins(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
) -> Result<Json<SignInList>, AppError> {
    let user = session_user(&principal)?;
    let sign_ins = sign_in_events::recent(app.pool(), user.name, RECENT_SIGN_INS)
        .await?
        .into_iter()
        .map(SignInView::from)
        .collect();
    Ok(Json(SignInList { sign_ins }))
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeDelta, Utc};
    use serde_json::json;

    use std::sync::Arc;
    use std::time::Duration;

    use axum::http::StatusCode;
    use base64::engine::general_purpose::URL_SAFE_NO_PAD;
    use base64::Engine as _;
    use tokio::time::timeout;

    use super::{key_expiry, requested_levels, within_ceiling, CatalogLevelBody, PasswordBody};
    use crate::db::{TEST_TEAM, TEST_USER};
    use crate::error::OkfError;
    use crate::http::api::testing::{
        allow, app, fail_on, keys, send, wait_for_lock_waiters, Browser, Payload, Reply,
    };
    use crate::http::console_session::COOKIE;
    use crate::mail::queue as mail_queue;
    use crate::mcp::testing::TestApp;
    use crate::store::audit::AuditActor;
    use crate::store::ceiling::{self, Ceiling};
    use crate::store::keys::{insert_legacy_test_key, Access, Grants};
    use crate::store::oauth::clients::ClientKind;
    use crate::store::oauth::codes::{self as code_rows, insert_test_client, NewSignInCode};
    use crate::store::passkeys::{self, testing::passkey_for};
    use crate::store::settings::{self, SettingKey};
    use crate::store::sign_in_events::{self, Method, Outcome};
    use crate::store::users::{self, NewUser};
    use crate::store::{identities, passwords};

    /// Creates user `name` with these entitlements, polls, and signs it in.
    async fn member(test: &TestApp, name: &str, grants: &[(&str, Access)]) -> Browser {
        let pool = test.app.pool();
        users::create(
            pool,
            &NewUser {
                name,
                display_name: "",
            },
            Utc::now(),
        )
        .await
        .expect("create the user");
        for (catalog, access) in grants {
            ceiling::entitle(pool, name, catalog, *access, Utc::now())
                .await
                .expect("entitle");
        }
        test.app.poll().await.expect("poll the user in");
        Browser::session_for(test, name).await
    }

    /// How long a race test waits for another task or a lock.
    const BOUND: Duration = Duration::from_secs(30);

    /// The `users.id` of `name`.
    async fn user_id(test: &TestApp, name: &str) -> String {
        sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(name)
            .fetch_one(test.app.pool())
            .await
            .expect("the user's id")
    }

    /// Gives `user_id` a software passkey called `name`.
    ///
    /// # Returns
    /// Its API id (unpadded base64url).
    async fn add_passkey(test: &TestApp, user_id: &str, name: &str) -> String {
        let passkey = passkey_for(user_id);
        passkeys::add(test.app.pool(), user_id, &passkey, name, Utc::now())
            .await
            .expect("add the passkey");
        URL_SAFE_NO_PAD.encode(passkey.cred_id().as_slice())
    }

    /// How many passkeys and identities `user_id` has.
    async fn methods_of(test: &TestApp, user_id: &str) -> (i64, i64) {
        sqlx::query_as(
            "SELECT (SELECT count(*) FROM user_passkeys WHERE user_id = $1), \
                    (SELECT count(*) FROM user_identities WHERE user_id = $1)",
        )
        .bind(user_id)
        .fetch_one(test.app.pool())
        .await
        .expect("count the methods")
    }

    /// The stored password hash of `user_id`.
    async fn hash_of(test: &TestApp, user_id: &str) -> Option<String> {
        sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1")
            .bind(user_id)
            .fetch_one(test.app.pool())
            .await
            .expect("read the hash")
    }

    /// `PUT /api/v1/me/password` with this body.
    async fn put_password(test: &TestApp, browser: &mut Browser, body: serde_json::Value) -> Reply {
        browser
            .send(test, "PUT", "/api/v1/me/password", Payload::Json(body))
            .await
    }

    /// A create body for one catalog at one level.
    fn one(name: &str, catalog: &str, access: &str) -> serde_json::Value {
        json!({"name": name, "catalogs": [{"catalog": catalog, "access": access}]})
    }

    #[tokio::test]
    async fn every_self_service_route_refuses_a_bearer_key() {
        let test = app().await;
        let keys = keys(&test).await;
        let rows: [(&str, &str, Option<serde_json::Value>); 6] = [
            ("GET", "/api/v1/me/keys", None),
            ("POST", "/api/v1/me/keys", Some(one("k", "main", "read"))),
            ("PATCH", "/api/v1/me/keys/x", Some(json!({"name": "y"}))),
            ("DELETE", "/api/v1/me/keys/x", None),
            ("GET", "/api/v1/me/grants", None),
            ("DELETE", "/api/v1/me/grants/x", None),
        ];
        for (method, uri, body) in rows {
            let (status, answer) = send(&test, method, uri, Some(&keys.admin), body).await;
            assert_eq!(status.as_u16(), 403, "{method} {uri}: {answer}");
        }
    }

    #[tokio::test]
    async fn a_key_may_read_its_users_account_and_an_unassigned_key_gets_404() {
        let test = app().await;
        let legacy = insert_legacy_test_key(
            test.app.pool(),
            "legacy",
            Access::Read,
            "legacy",
            Utc::now(),
        )
        .await;
        // The legacy insert bumps no generation; `keys` bumps and polls both in.
        let keys = keys(&test).await;

        let (status, me) = send(&test, "GET", "/api/v1/me", Some(&keys.read), None).await;
        let (unassigned, _) = send(&test, "GET", "/api/v1/me", Some(&legacy), None).await;

        assert_eq!(status.as_u16(), 200, "{me}");
        assert_eq!(me["user"], TEST_USER);
        assert_eq!(me["session"], false);
        assert_eq!(me["teams"], json!([TEST_TEAM]));
        assert_eq!(unassigned.as_u16(), 404);
    }

    #[tokio::test]
    async fn a_session_creates_lists_renames_and_revokes_its_own_key() {
        let test = app().await;
        let mut alice = member(&test, "alice", &[("main", Access::Write)]).await;

        let (created, body) = alice
            .send(
                &test,
                "POST",
                "/api/v1/me/keys",
                Payload::Json(one("laptop", "main", "write")),
            )
            .await
            .parts();
        let secret = body["secret"]
            .as_str()
            .expect("the secret, once")
            .to_string();
        let id = body["key"]["id"].as_str().expect("an id").to_string();
        let (_, whoami) = send(&test, "GET", "/api/v1/whoami", Some(&secret), None).await;
        let (_, listed) = alice
            .send(&test, "GET", "/api/v1/me/keys", Payload::Empty)
            .await
            .parts();
        let (renamed, view) = alice
            .send(
                &test,
                "PATCH",
                &format!("/api/v1/me/keys/{id}"),
                Payload::Json(json!({"name": "desk"})),
            )
            .await
            .parts();
        let (revoked, gone) = alice
            .send(
                &test,
                "DELETE",
                &format!("/api/v1/me/keys/{id}"),
                Payload::Empty,
            )
            .await
            .parts();
        let (refused, _) = send(&test, "GET", "/api/v1/whoami", Some(&secret), None).await;

        assert_eq!(created.as_u16(), 200, "{body}");
        assert_eq!(body["key"]["self_issued"], true);
        assert_eq!(body["key"]["access"], "write");
        assert_eq!(body["key"]["actor"], "laptop");
        assert_eq!(body["key"]["user"], "alice");
        assert_eq!(whoami["user"], "alice");
        assert_eq!(whoami["catalogs"], json!(["main"]));
        // Only alice's keys: the fixture user's are not listed.
        assert_eq!(listed["keys"].as_array().map(Vec::len), Some(1), "{listed}");
        // The secret is shown once: no later answer holds it.
        for later in [&listed, &view, &gone] {
            assert!(!later.to_string().contains(&secret), "{later}");
        }
        assert_eq!(renamed.as_u16(), 200, "{view}");
        assert_eq!(view["name"], "desk");
        assert_eq!(revoked.as_u16(), 200);
        assert!(gone["revoked_at"].is_string());
        assert_eq!(refused.as_u16(), 401);
    }

    #[tokio::test]
    async fn another_users_key_is_a_404_for_a_session() {
        let test = app().await;
        let keys = keys(&test).await;
        let mut alice = member(&test, "alice", &[("main", Access::Read)]).await;
        let (_, listed) = send(&test, "GET", "/api/v1/keys", Some(&keys.admin), None).await;
        let reader = listed["keys"]
            .as_array()
            .expect("a list")
            .iter()
            .find(|key| key["name"] == "reader")
            .expect("the fixture read key")["id"]
            .as_str()
            .expect("an id")
            .to_string();

        let (renamed, _) = alice
            .send(
                &test,
                "PATCH",
                &format!("/api/v1/me/keys/{reader}"),
                Payload::Json(json!({"name": "mine"})),
            )
            .await
            .parts();
        let (revoked, _) = alice
            .send(
                &test,
                "DELETE",
                &format!("/api/v1/me/keys/{reader}"),
                Payload::Empty,
            )
            .await
            .parts();

        assert_eq!(renamed.as_u16(), 404);
        assert_eq!(revoked.as_u16(), 404);
    }

    #[tokio::test]
    async fn a_key_above_the_ceiling_is_a_422_naming_the_limit() {
        let test = app().await;
        let mut alice = member(&test, "alice", &[("main", Access::Read)]).await;

        let (write, too_high) = alice
            .send(
                &test,
                "POST",
                "/api/v1/me/keys",
                Payload::Json(one("a", "main", "write")),
            )
            .await
            .parts();
        let (other, outside) = alice
            .send(
                &test,
                "POST",
                "/api/v1/me/keys",
                Payload::Json(one("b", "other", "read")),
            )
            .await
            .parts();
        let (admin, _) = alice
            .send(
                &test,
                "POST",
                "/api/v1/me/keys",
                Payload::Json(one("c", "main", "admin")),
            )
            .await
            .parts();
        let (all, _) = alice
            .send(
                &test,
                "POST",
                "/api/v1/me/keys",
                Payload::Json(json!({"name": "d", "all": true, "catalogs": []})),
            )
            .await
            .parts();
        let (none, _) = alice
            .send(
                &test,
                "POST",
                "/api/v1/me/keys",
                Payload::Json(json!({"name": "e", "catalogs": []})),
            )
            .await
            .parts();

        let mut for_bob = one("f", "main", "read");
        for_bob["user"] = json!("bob");
        let (other_user, _) = alice
            .send(&test, "POST", "/api/v1/me/keys", Payload::Json(for_bob))
            .await
            .parts();
        let mut reporter = one("g", "main", "read");
        reporter["maintenance_report"] = json!(true);
        let (reporting, _) = alice
            .send(&test, "POST", "/api/v1/me/keys", Payload::Json(reporter))
            .await
            .parts();
        let (_, listed) = alice
            .send(&test, "GET", "/api/v1/me/keys", Payload::Empty)
            .await
            .parts();

        assert_eq!(write.as_u16(), 422, "{too_high}");
        assert_eq!(too_high["error"], "LimitExceeded");
        assert_eq!(too_high["limit"], "ceiling");
        assert_eq!(other.as_u16(), 422, "{outside}");
        assert_eq!(outside["limit"], "ceiling");
        assert_eq!(admin.as_u16(), 422, "never an admin key");
        assert_eq!(all.as_u16(), 422, "no all-catalogs self-issued key");
        assert_eq!(none.as_u16(), 400, "a key names a catalog");
        assert_eq!(
            other_user.as_u16(),
            422,
            "a key belongs to the signed-in user"
        );
        assert_eq!(reporting.as_u16(), 422, "never a maintenance reporter");
        assert_eq!(listed["keys"], json!([]), "nothing was created");
    }

    #[tokio::test]
    async fn the_key_limit_counts_live_self_issued_keys() {
        let test = app().await;
        let mut alice = member(&test, "alice", &[("main", Access::Read)]).await;
        settings::set(
            test.app.pool(),
            SettingKey::MaxKeysPerUser,
            "1",
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .expect("set the limit");
        // The poll loop's next pass, run now: the running app picks the
        // setting up from `app.settings()` without a restart.
        test.app.poll().await.expect("poll the setting in");

        let (first, body) = alice
            .send(
                &test,
                "POST",
                "/api/v1/me/keys",
                Payload::Json(one("a", "main", "read")),
            )
            .await
            .parts();
        let (second, refused) = alice
            .send(
                &test,
                "POST",
                "/api/v1/me/keys",
                Payload::Json(one("b", "main", "read")),
            )
            .await
            .parts();
        let id = body["key"]["id"].as_str().expect("an id").to_string();
        alice
            .send(
                &test,
                "DELETE",
                &format!("/api/v1/me/keys/{id}"),
                Payload::Empty,
            )
            .await
            .parts();
        let (third, _) = alice
            .send(
                &test,
                "POST",
                "/api/v1/me/keys",
                Payload::Json(one("c", "main", "read")),
            )
            .await
            .parts();
        settings::set(
            test.app.pool(),
            SettingKey::MaxKeysPerUser,
            "2",
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .expect("raise the limit");
        test.app.poll().await.expect("poll the setting in");
        let (_, me) = alice
            .send(&test, "GET", "/api/v1/me", Payload::Empty)
            .await
            .parts();
        let (fourth, _) = alice
            .send(
                &test,
                "POST",
                "/api/v1/me/keys",
                Payload::Json(one("d", "main", "read")),
            )
            .await
            .parts();

        assert_eq!(first.as_u16(), 200, "{body}");
        assert_eq!(second.as_u16(), 422);
        assert_eq!(refused["error"], "LimitExceeded");
        assert_eq!(refused["limit"], "max_keys_per_user");
        assert_eq!(third.as_u16(), 200, "a revoked key frees its place");
        assert_eq!(me["limits"]["max_keys_per_user"], 2);
        assert_eq!(fourth.as_u16(), 200, "a raised limit applies at once");
    }

    #[tokio::test]
    async fn the_lifetime_limit_caps_and_defaults_the_expiry() {
        let test = app().await;
        let mut alice = member(&test, "alice", &[("main", Access::Read)]).await;
        settings::set(
            test.app.pool(),
            SettingKey::MaxKeyLifetime,
            "24h",
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .expect("set the lifetime");
        test.app.poll().await.expect("poll the setting in");
        let later = |hours: i64| {
            (Utc::now() + TimeDelta::hours(hours))
                .to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
        };

        let (defaulted, body) = alice
            .send(
                &test,
                "POST",
                "/api/v1/me/keys",
                Payload::Json(one("a", "main", "read")),
            )
            .await
            .parts();
        let mut long = one("b", "main", "read");
        long["expires_at"] = json!(later(48));
        let (too_long, refused) = alice
            .send(&test, "POST", "/api/v1/me/keys", Payload::Json(long))
            .await
            .parts();
        let mut short = one("c", "main", "read");
        short["expires_at"] = json!(later(2));
        let (within, kept) = alice
            .send(&test, "POST", "/api/v1/me/keys", Payload::Json(short))
            .await
            .parts();
        let mut past = one("d", "main", "read");
        past["expires_at"] = json!(later(-1));
        let (in_the_past, _) = alice
            .send(&test, "POST", "/api/v1/me/keys", Payload::Json(past))
            .await
            .parts();

        assert_eq!(defaulted.as_u16(), 200, "{body}");
        let expires: DateTime<Utc> = body["key"]["expires_at"]
            .as_str()
            .expect("an expiry")
            .parse()
            .expect("RFC 3339");
        let left = expires - Utc::now();
        assert!(
            left > TimeDelta::hours(23) && left <= TimeDelta::hours(24),
            "{left}"
        );
        assert_eq!(too_long.as_u16(), 422);
        assert_eq!(refused["limit"], "max_key_lifetime");
        assert_eq!(within.as_u16(), 200, "{kept}");
        assert_eq!(in_the_past.as_u16(), 400);
    }

    #[tokio::test]
    async fn an_admin_session_is_capped_for_its_own_keys() {
        let test = app().await;
        users::create(
            test.app.pool(),
            &NewUser {
                name: "alice",
                display_name: "",
            },
            Utc::now(),
        )
        .await
        .expect("create alice");
        ceiling::entitle(test.app.pool(), "alice", "main", Access::Read, Utc::now())
            .await
            .expect("entitle");
        users::set_admin(test.app.pool(), "alice", true, true)
            .await
            .expect("make alice admin");
        test.app.poll().await.expect("poll");
        let mut alice = Browser::session_for(&test, "alice").await;

        let (admin_route, _) = alice
            .send(&test, "GET", "/api/v1/keys", Payload::Empty)
            .await
            .parts();
        let (capped, refused) = alice
            .send(
                &test,
                "POST",
                "/api/v1/me/keys",
                Payload::Json(one("a", "main", "write")),
            )
            .await
            .parts();

        assert_eq!(
            admin_route.as_u16(),
            200,
            "an admin session acts as an admin"
        );
        assert_eq!(capped.as_u16(), 422);
        assert_eq!(refused["limit"], "ceiling");
    }

    #[tokio::test]
    async fn grants_list_and_revoke_only_the_users_own() {
        let test = app().await;
        let mut alice = member(&test, "alice", &[("main", Access::Read)]).await;
        let client = insert_test_client(
            test.app.pool(),
            "test-client",
            ClientKind::Dcr,
            "https://client.example.com/callback",
        )
        .await;
        let mut grants = Vec::new();
        for (name, user) in [("phone", "alice"), ("tablet", TEST_USER)] {
            let catalogs = ["main".to_string()];
            let code = code_rows::create(
                test.app.pool(),
                &NewSignInCode {
                    name,
                    access: Access::Read,
                    grants: Grants::Catalogs(&catalogs),
                    default_catalog: None,
                    actor: name,
                    expires_at: Utc::now() + TimeDelta::hours(1),
                    user,
                },
                Utc::now(),
            )
            .await
            .expect("mint a code");
            grants.push(
                code_rows::redeem(test.app.pool(), &code.id, &client, Utc::now())
                    .await
                    .expect("redeem"),
            );
        }
        test.app.poll().await.expect("poll the grants in");

        let (_, listed) = alice
            .send(&test, "GET", "/api/v1/me/grants", Payload::Empty)
            .await
            .parts();
        let (foreign, _) = alice
            .send(
                &test,
                "DELETE",
                &format!("/api/v1/me/grants/{}", grants[1]),
                Payload::Empty,
            )
            .await
            .parts();
        let (own, view) = alice
            .send(
                &test,
                "DELETE",
                &format!("/api/v1/me/grants/{}", grants[0]),
                Payload::Empty,
            )
            .await
            .parts();

        let names: Vec<&str> = listed["grants"]
            .as_array()
            .expect("a list")
            .iter()
            .filter_map(|grant| grant["id"].as_str())
            .collect();
        assert_eq!(names, [grants[0].as_str()]);
        assert_eq!(foreign.as_u16(), 404);
        assert_eq!(own.as_u16(), 200, "{view}");
        assert!(view["revoked_at"].is_string());
        assert!(test.app.keys().find_grant(&grants[0]).is_none());
        assert!(test.app.keys().find_grant(&grants[1]).is_some());
    }

    #[tokio::test]
    async fn the_account_shows_teams_ceiling_and_password_state() {
        let test = app().await;
        let mut alice = member(
            &test,
            "alice",
            &[("main", Access::Read), ("other", Access::Write)],
        )
        .await;
        users::set_email(test.app.pool(), "alice", Some("alice@example.com"))
            .await
            .expect("email");

        let (status, me) = alice
            .send(&test, "GET", "/api/v1/me", Payload::Empty)
            .await
            .parts();

        assert_eq!(status.as_u16(), 200, "{me}");
        assert_eq!(me["user"], "alice");
        assert_eq!(me["email"], "alice@example.com");
        assert_eq!(me["admin"], false);
        assert_eq!(me["has_password"], false);
        assert_eq!(me["session"], true);
        assert_eq!(me["password_sign_in"], true);
        assert_eq!(
            me["limits"],
            json!({"max_keys_per_user": 20, "max_key_lifetime_secs": null})
        );
        assert_eq!(me["teams"], json!(["alice"]));
        assert_eq!(
            me["ceiling"],
            json!([
                {"catalog": "main", "access": "read"},
                {"catalog": "other", "access": "write"},
            ])
        );
    }

    #[test]
    fn requested_levels_refuse_an_empty_or_repeated_list() {
        let body = |entries: &[(&str, &str)]| -> Vec<CatalogLevelBody> {
            entries
                .iter()
                .map(|(catalog, access)| CatalogLevelBody {
                    catalog: catalog.to_string(),
                    access: access.to_string(),
                })
                .collect()
        };

        let empty = requested_levels(&body(&[]));
        let twice = requested_levels(&body(&[("main", "read"), ("main", "write")]));
        let bad = requested_levels(&body(&[("main", "owner")]));
        let good = requested_levels(&body(&[("main", "write"), ("other", "read")])).expect("valid");

        assert!(
            matches!(empty, Err(OkfError::InvalidPath { .. })),
            "{empty:?}"
        );
        assert!(
            matches!(twice, Err(OkfError::InvalidPath { .. })),
            "{twice:?}"
        );
        assert!(matches!(bad, Err(OkfError::InvalidPath { .. })), "{bad:?}");
        assert_eq!(good.get("main"), Some(&Access::Write));
        assert_eq!(good.get("other"), Some(&Access::Read));
    }

    #[test]
    fn within_ceiling_names_the_first_catalog_over_it() {
        let ceiling: Ceiling = [("main".to_string(), Access::Read)].into_iter().collect();
        let wanted = |catalog: &str, access| -> std::collections::BTreeMap<String, Access> {
            [(catalog.to_string(), access)].into_iter().collect()
        };

        assert!(within_ceiling(&wanted("main", Access::Read), &ceiling).is_ok());
        for refused in [
            wanted("main", Access::Write),
            wanted("main", Access::Admin),
            wanted("other", Access::Read),
        ] {
            let error = within_ceiling(&refused, &ceiling).expect_err("over the ceiling");
            assert!(
                matches!(
                    error,
                    OkfError::LimitExceeded {
                        limit: "ceiling",
                        ..
                    }
                ),
                "{error:?}"
            );
        }
    }

    #[test]
    fn key_expiry_defaults_to_and_is_capped_by_the_lifetime() {
        let now: DateTime<Utc> = "2026-11-12T12:00:00Z".parse().expect("time");
        let day = Some(std::time::Duration::from_secs(24 * 3600));

        assert_eq!(key_expiry(None, None, now).expect("none"), None);
        assert_eq!(
            key_expiry(None, day, now).expect("defaulted"),
            Some(now + TimeDelta::hours(24))
        );
        assert_eq!(
            key_expiry(Some("2026-11-12T18:00:00Z"), day, now).expect("within"),
            Some(now + TimeDelta::hours(6))
        );
        assert!(matches!(
            key_expiry(Some("2026-11-14T12:00:00Z"), day, now),
            Err(OkfError::LimitExceeded {
                limit: "max_key_lifetime",
                ..
            })
        ));
        assert!(matches!(
            key_expiry(Some("2026-11-12T11:00:00Z"), None, now),
            Err(OkfError::InvalidPath { .. })
        ));
        assert!(matches!(
            key_expiry(Some("tomorrow"), None, now),
            Err(OkfError::InvalidPath { .. })
        ));
        // An empty field is "no expiry asked for", as the console sends it.
        assert_eq!(key_expiry(Some(""), None, now).expect("empty"), None);
    }

    #[tokio::test]
    async fn every_account_route_refuses_a_bearer_key() {
        let test = app().await;
        let keys = keys(&test).await;
        let rows: [(&str, &str, Option<serde_json::Value>); 9] = [
            (
                "PUT",
                "/api/v1/me/password",
                Some(json!({"new_password": "a long new password"})),
            ),
            ("GET", "/api/v1/me/passkeys", None),
            ("PATCH", "/api/v1/me/passkeys/x", Some(json!({"name": "y"}))),
            ("DELETE", "/api/v1/me/passkeys/x", None),
            ("GET", "/api/v1/me/identities", None),
            ("DELETE", "/api/v1/me/identities/example", None),
            ("GET", "/api/v1/me/sessions", None),
            ("DELETE", "/api/v1/me/sessions/abcdef012345", None),
            ("GET", "/api/v1/me/sign-ins", None),
        ];
        for (method, uri, body) in rows {
            let (status, answer) = send(&test, method, uri, Some(&keys.admin), body).await;
            assert_eq!(status.as_u16(), 403, "{method} {uri}: {answer}");
        }
    }

    #[tokio::test]
    async fn changing_the_password_needs_the_current_one_and_revokes_other_sessions() {
        let test = app().await;
        let mut first = member(&test, "alice", &[]).await;
        let mut second = Browser::session_for(&test, "alice").await;
        let id = user_id(&test, "alice").await;
        users::set_email(test.app.pool(), "alice", Some("alice@example.com"))
            .await
            .expect("email");
        passwords::set(test.app.pool(), &id, "correct horse battery", Utc::now())
            .await
            .expect("password");
        let change =
            |current: &str, new: &str| json!({"current_password": current, "new_password": new});

        let (missing, _) = put_password(
            &test,
            &mut first,
            json!({"new_password": "a long new password"}),
        )
        .await
        .parts();
        let (wrong, _) = put_password(
            &test,
            &mut first,
            change("not it at all", "a long new password"),
        )
        .await
        .parts();
        let (weak, refused) =
            put_password(&test, &mut first, change("correct horse battery", "short"))
                .await
                .parts();
        let unchanged =
            passwords::verify_user(test.app.pool(), &id, "correct horse battery", Utc::now())
                .await
                .expect("verify");
        let (changed, _) = put_password(
            &test,
            &mut first,
            change("correct horse battery", "a long new password"),
        )
        .await
        .parts();
        let (kept, _) = first
            .send(&test, "GET", "/api/v1/me", Payload::Empty)
            .await
            .parts();
        let (ended, _) = second
            .send(&test, "GET", "/api/v1/me", Payload::Empty)
            .await
            .parts();
        let queued = mail_queue::list(test.app.pool()).await.expect("the queue");
        let now_set =
            passwords::verify_user(test.app.pool(), &id, "a long new password", Utc::now())
                .await
                .expect("verify");

        assert_eq!(missing.as_u16(), 403, "a missing current password");
        assert_eq!(wrong.as_u16(), 403, "a wrong current password, never 401");
        assert_eq!(weak.as_u16(), 422);
        assert_eq!(refused["limit"], "password");
        assert!(
            matches!(unchanged, passwords::SignInOutcome::Ok(_)),
            "refusals change nothing"
        );
        assert_eq!(changed.as_u16(), 204);
        assert_eq!(kept.as_u16(), 200, "the session that changed it stays");
        assert_eq!(ended.as_u16(), 401, "every other session is revoked");
        assert!(matches!(now_set, passwords::SignInOutcome::Ok(_)));
        assert!(
            queued
                .iter()
                .any(|mail| mail.to == "alice@example.com" && mail.template == "method_added"),
            "{queued:?}"
        );
    }

    #[tokio::test]
    async fn a_locked_account_cannot_change_its_password_and_stays_signed_in() {
        let test = app().await;
        let mut alice = member(&test, "alice", &[]).await;
        let id = user_id(&test, "alice").await;
        passwords::set(test.app.pool(), &id, "correct horse battery", Utc::now())
            .await
            .expect("password");
        sqlx::query("UPDATE users SET locked_until = now() + interval '1 hour' WHERE id = $1")
            .bind(&id)
            .execute(test.app.pool())
            .await
            .expect("lock alice");
        let before = hash_of(&test, &id).await;

        // The right current password, but the account is locked.
        let (locked, body) = put_password(
            &test,
            &mut alice,
            json!({"current_password": "correct horse battery", "new_password": "a long new password"}),
        )
        .await
        .parts();
        let (still, _) = alice
            .send(&test, "GET", "/api/v1/me", Payload::Empty)
            .await
            .parts();

        assert_eq!(locked.as_u16(), 403, "never 401: {body}");
        assert_eq!(
            hash_of(&test, &id).await,
            before,
            "the password is unchanged"
        );
        assert_eq!(still.as_u16(), 200, "the session stays");
    }

    #[tokio::test]
    async fn a_first_password_needs_no_current_one_and_keep_sessions_keeps_them() {
        let test = app().await;
        let mut first = member(&test, "alice", &[]).await;
        let mut second = Browser::session_for(&test, "alice").await;

        let (set, _) = put_password(
            &test,
            &mut first,
            json!({"new_password": "a long first password", "keep_sessions": true}),
        )
        .await
        .parts();
        let (_, me) = first
            .send(&test, "GET", "/api/v1/me", Payload::Empty)
            .await
            .parts();
        let (other, _) = second
            .send(&test, "GET", "/api/v1/me", Payload::Empty)
            .await
            .parts();

        assert_eq!(set.as_u16(), 204);
        assert_eq!(me["has_password"], true);
        assert_eq!(other.as_u16(), 200);
    }

    #[tokio::test]
    async fn a_password_change_that_fails_part_way_changes_nothing() {
        let test = app().await;
        let mut first = member(&test, "alice", &[]).await;
        let mut second = Browser::session_for(&test, "alice").await;
        let id = user_id(&test, "alice").await;
        passwords::set(test.app.pool(), &id, "correct horse battery", Utc::now())
            .await
            .expect("password");
        let before = hash_of(&test, &id).await;
        // Ending the other sessions fails: the new password must not stay.
        fail_on(
            test.app.pool(),
            "BEFORE DELETE ON user_sessions FOR EACH ROW",
        )
        .await;

        let failed = put_password(
            &test,
            &mut first,
            json!({"current_password": "correct horse battery", "new_password": "a long new password"}),
        )
        .await;
        allow(test.app.pool(), "user_sessions").await;
        let (other, _) = second
            .send(&test, "GET", "/api/v1/me", Payload::Empty)
            .await
            .parts();

        assert_eq!(
            failed.status,
            StatusCode::INTERNAL_SERVER_ERROR,
            "{}",
            failed.text()
        );
        assert_eq!(hash_of(&test, &id).await, before, "the old password stands");
        assert_eq!(other.as_u16(), 200, "no session ended");
    }

    #[test]
    fn a_password_body_debug_shows_neither_password() {
        let body: PasswordBody = serde_json::from_value(json!({
            "current_password": "old secret words",
            "new_password": "new secret words",
            "keep_sessions": true,
        }))
        .expect("a body");

        let shown = format!("{body:?}");

        assert!(shown.contains("keep_sessions: true"), "{shown}");
        assert!(!shown.contains("secret"), "{shown}");
    }

    #[tokio::test]
    async fn sessions_list_marks_the_current_one_and_revoke_by_prefix() {
        let test = app().await;
        let mut first = member(&test, "alice", &[]).await;
        let mut second = Browser::session_for(&test, "alice").await;

        let (_, listed) = first
            .send(&test, "GET", "/api/v1/me/sessions", Payload::Empty)
            .await
            .parts();
        let sessions = listed["sessions"].as_array().expect("a list").clone();
        let other = sessions
            .iter()
            .find(|session| session["current"] == false)
            .expect("the other session")["id_prefix"]
            .as_str()
            .expect("a prefix")
            .to_string();
        let (short, _) = first
            .send(
                &test,
                "DELETE",
                &format!("/api/v1/me/sessions/{}", &other[..6]),
                Payload::Empty,
            )
            .await
            .parts();
        // A fixed prefix with uppercase A-F: an uppercased real prefix can be
        // all digits (about 0.36% of the time), and then it is valid.
        let (upper, _) = first
            .send(
                &test,
                "DELETE",
                "/api/v1/me/sessions/ABCDEF012345",
                Payload::Empty,
            )
            .await
            .parts();
        let (revoked, _) = first
            .send(
                &test,
                "DELETE",
                &format!("/api/v1/me/sessions/{other}"),
                Payload::Empty,
            )
            .await
            .parts();
        let (ended, _) = second
            .send(&test, "GET", "/api/v1/me", Payload::Empty)
            .await
            .parts();
        let (_, after) = first
            .send(&test, "GET", "/api/v1/me/sessions", Payload::Empty)
            .await
            .parts();

        assert_eq!(sessions.len(), 2);
        assert_eq!(sessions.iter().filter(|s| s["current"] == true).count(), 1);
        // Exactly these fields: never the id or the CSRF token.
        let mut fields: Vec<&str> = sessions[0]
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        fields.sort_unstable();
        assert_eq!(
            fields,
            [
                "created_at",
                "current",
                "id_prefix",
                "ip",
                "last_seen_at",
                "user_agent"
            ]
        );
        let text = listed.to_string();
        for secret in [first.cookie(COOKIE), first.csrf.as_deref()] {
            assert!(!text.contains(secret.expect("a secret")), "{text}");
        }
        assert_eq!(short.as_u16(), 404, "a short prefix");
        assert_eq!(upper.as_u16(), 404, "only lowercase hex");
        assert_eq!(revoked.as_u16(), 204);
        assert_eq!(ended.as_u16(), 401);
        assert_eq!(after["sessions"].as_array().map(Vec::len), Some(1));
    }

    #[tokio::test]
    async fn a_session_prefix_shared_by_two_sessions_is_refused() {
        let test = app().await;
        let mut alice = member(&test, "alice", &[]).await;
        let id = user_id(&test, "alice").await;
        for last in [1_u8, 2] {
            let mut hash = vec![0xcd_u8; 32];
            hash[31] = last;
            sqlx::query(
                "INSERT INTO user_sessions \
                     (id_hash, user_id, csrf_token, created_at, last_seen_at, expires_at) \
                 VALUES ($1, $2, '', now(), now(), now() + interval '1 day')",
            )
            .bind(hash)
            .bind(&id)
            .execute(test.app.pool())
            .await
            .expect("insert a session");
        }

        let (status, body) = alice
            .send(
                &test,
                "DELETE",
                "/api/v1/me/sessions/cdcdcdcdcdcd",
                Payload::Empty,
            )
            .await
            .parts();
        let (_, listed) = alice
            .send(&test, "GET", "/api/v1/me/sessions", Payload::Empty)
            .await
            .parts();

        assert_eq!(status.as_u16(), 409, "{body}");
        assert_eq!(
            listed["sessions"].as_array().map(Vec::len),
            Some(3),
            "none ended"
        );
    }

    #[tokio::test]
    async fn another_users_passkey_identity_and_session_are_404s() {
        let test = app().await;
        let mut alice = member(&test, "alice", &[]).await;
        let mut bob = member(&test, "bob", &[]).await;
        let bob_id = user_id(&test, "bob").await;
        passwords::set(
            test.app.pool(),
            &bob_id,
            "a long first password",
            Utc::now(),
        )
        .await
        .expect("password");
        let bobs_passkey = add_passkey(&test, &bob_id, "Bob's phone").await;
        identities::link(
            test.app.pool(),
            &bob_id,
            "example",
            "sub-b",
            "bob@example.com",
            Utc::now(),
        )
        .await
        .expect("link");
        let (_, bobs) = bob
            .send(&test, "GET", "/api/v1/me/sessions", Payload::Empty)
            .await
            .parts();
        let bobs_prefix = bobs["sessions"][0]["id_prefix"]
            .as_str()
            .expect("a prefix")
            .to_string();

        let (rename, _) = alice
            .send(
                &test,
                "PATCH",
                &format!("/api/v1/me/passkeys/{bobs_passkey}"),
                Payload::Json(json!({"name": "mine now"})),
            )
            .await
            .parts();
        let (remove, _) = alice
            .send(
                &test,
                "DELETE",
                &format!("/api/v1/me/passkeys/{bobs_passkey}"),
                Payload::Empty,
            )
            .await
            .parts();
        let (unlink, _) = alice
            .send(
                &test,
                "DELETE",
                "/api/v1/me/identities/example",
                Payload::Empty,
            )
            .await
            .parts();
        let (revoke, _) = alice
            .send(
                &test,
                "DELETE",
                &format!("/api/v1/me/sessions/{bobs_prefix}"),
                Payload::Empty,
            )
            .await
            .parts();
        let (_, alices_passkeys) = alice
            .send(&test, "GET", "/api/v1/me/passkeys", Payload::Empty)
            .await
            .parts();
        let (bob_still, _) = bob
            .send(&test, "GET", "/api/v1/me", Payload::Empty)
            .await
            .parts();
        let (_, bobs_passkeys) = bob
            .send(&test, "GET", "/api/v1/me/passkeys", Payload::Empty)
            .await
            .parts();

        assert_eq!(rename.as_u16(), 404);
        assert_eq!(remove.as_u16(), 404);
        assert_eq!(unlink.as_u16(), 404);
        assert_eq!(revoke.as_u16(), 404);
        assert_eq!(alices_passkeys, json!({"passkeys": []}));
        assert_eq!(bob_still.as_u16(), 200, "bob's session lives");
        assert_eq!(bobs_passkeys["passkeys"][0]["name"], "Bob's phone");
        assert_eq!(methods_of(&test, &bob_id).await, (1, 1), "bob keeps both");
    }

    #[tokio::test]
    async fn passkeys_list_rename_and_remove_while_another_method_remains() {
        let test = app().await;
        let mut alice = member(&test, "alice", &[]).await;
        let id = user_id(&test, "alice").await;
        let laptop = add_passkey(&test, &id, "Laptop").await;
        let phone = add_passkey(&test, &id, "Phone").await;

        let (_, listed) = alice
            .send(&test, "GET", "/api/v1/me/passkeys", Payload::Empty)
            .await
            .parts();
        let (renamed, _) = alice
            .send(
                &test,
                "PATCH",
                &format!("/api/v1/me/passkeys/{laptop}"),
                Payload::Json(json!({"name": "  Work laptop  "})),
            )
            .await
            .parts();
        let (bad_name, bad) = alice
            .send(
                &test,
                "PATCH",
                &format!("/api/v1/me/passkeys/{laptop}"),
                Payload::Json(json!({"name": "a\u{0}b"})),
            )
            .await
            .parts();
        let (removed, _) = alice
            .send(
                &test,
                "DELETE",
                &format!("/api/v1/me/passkeys/{phone}"),
                Payload::Empty,
            )
            .await
            .parts();
        let (last, _) = alice
            .send(
                &test,
                "DELETE",
                &format!("/api/v1/me/passkeys/{laptop}"),
                Payload::Empty,
            )
            .await
            .parts();
        let (_, after) = alice
            .send(&test, "GET", "/api/v1/me/passkeys", Payload::Empty)
            .await
            .parts();

        let mut fields: Vec<&str> = listed["passkeys"][0]
            .as_object()
            .expect("an object")
            .keys()
            .map(String::as_str)
            .collect();
        fields.sort_unstable();
        assert_eq!(
            fields,
            ["created_at", "id", "last_used_at", "name"],
            "no public key"
        );
        assert_eq!(listed["passkeys"][0]["id"], laptop.as_str());
        assert_eq!(renamed.as_u16(), 204);
        assert_eq!(bad_name.as_u16(), 422);
        assert_eq!(bad["limit"], "passkey_name");
        assert_eq!(removed.as_u16(), 204);
        assert_eq!(last.as_u16(), 409, "the last sign-in method stays");
        assert_eq!(after["passkeys"][0]["name"], "Work laptop");
        assert_eq!(after["passkeys"].as_array().map(Vec::len), Some(1));
    }

    #[tokio::test]
    async fn sign_ins_list_the_users_recent_events_newest_first() {
        let test = app().await;
        let mut alice = member(&test, "alice", &[]).await;
        member(&test, "bob", &[]).await;
        let id = user_id(&test, "alice").await;
        let bob_id = user_id(&test, "bob").await;
        let earlier = Utc::now() - TimeDelta::minutes(5);
        let pool = test.app.pool();
        sign_in_events::record(
            pool,
            Some(&id),
            Method::Password,
            Outcome::Failed,
            None,
            earlier,
        )
        .await
        .expect("record");
        sign_in_events::record(
            pool,
            Some(&id),
            Method::Passkey,
            Outcome::Ok,
            None,
            Utc::now(),
        )
        .await
        .expect("record");
        sign_in_events::record(
            pool,
            Some(&bob_id),
            Method::Oidc,
            Outcome::Ok,
            None,
            Utc::now(),
        )
        .await
        .expect("record");

        let (status, body) = alice
            .send(&test, "GET", "/api/v1/me/sign-ins", Payload::Empty)
            .await
            .parts();

        assert_eq!(status.as_u16(), 200, "{body}");
        let events = body["sign_ins"].as_array().expect("a list");
        assert_eq!(events.len(), 2, "only alice's: {body}");
        assert_eq!(events[0]["method"], "passkey");
        assert_eq!(events[0]["outcome"], "ok");
        assert_eq!(events[1]["method"], "password");
        assert_eq!(events[1]["outcome"], "failed");
        assert_eq!(events[1]["ip"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn identities_unlink_unless_last_and_passkey_routes_answer_404_for_unknown_ids() {
        let test = app().await;
        let mut alice = member(&test, "alice", &[]).await;
        let id = user_id(&test, "alice").await;
        identities::link(
            test.app.pool(),
            &id,
            "example",
            "sub-1",
            "alice@example.com",
            Utc::now(),
        )
        .await
        .expect("link");

        let (_, listed) = alice
            .send(&test, "GET", "/api/v1/me/identities", Payload::Empty)
            .await
            .parts();
        let (last, _) = alice
            .send(
                &test,
                "DELETE",
                "/api/v1/me/identities/example",
                Payload::Empty,
            )
            .await
            .parts();
        passwords::set(test.app.pool(), &id, "a long first password", Utc::now())
            .await
            .expect("password");
        // A NUL, which Postgres refuses, is an unknown provider, not a 500.
        let (malformed, _) = alice
            .send(
                &test,
                "DELETE",
                "/api/v1/me/identities/ex%00ample",
                Payload::Empty,
            )
            .await
            .parts();
        let (unlinked, _) = alice
            .send(
                &test,
                "DELETE",
                "/api/v1/me/identities/example",
                Payload::Empty,
            )
            .await
            .parts();
        let (again, _) = alice
            .send(
                &test,
                "DELETE",
                "/api/v1/me/identities/example",
                Payload::Empty,
            )
            .await
            .parts();
        let (_, passkeys) = alice
            .send(&test, "GET", "/api/v1/me/passkeys", Payload::Empty)
            .await
            .parts();
        let (rename, _) = alice
            .send(
                &test,
                "PATCH",
                "/api/v1/me/passkeys/AAAA",
                Payload::Json(json!({"name": "phone"})),
            )
            .await
            .parts();
        let (remove, _) = alice
            .send(&test, "DELETE", "/api/v1/me/passkeys/AAAA", Payload::Empty)
            .await
            .parts();
        let (not_base64, _) = alice
            .send(
                &test,
                "DELETE",
                "/api/v1/me/passkeys/%00%21",
                Payload::Empty,
            )
            .await
            .parts();

        assert_eq!(listed["identities"][0]["provider"], "example");
        assert_eq!(
            listed["identities"][0]["email_at_link"],
            "alice@example.com"
        );
        assert!(listed["identities"][0].get("subject").is_none(), "{listed}");
        assert_eq!(
            listed["providers"],
            json!([]),
            "this app configures no provider"
        );
        assert_eq!(last.as_u16(), 409, "the last sign-in method stays");
        assert_eq!(malformed.as_u16(), 404);
        assert_eq!(unlinked.as_u16(), 204);
        assert_eq!(again.as_u16(), 404);
        assert_eq!(passkeys, json!({"passkeys": []}));
        assert_eq!(rename.as_u16(), 404);
        assert_eq!(remove.as_u16(), 404);
        assert_eq!(not_base64.as_u16(), 404);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn two_removals_at_once_leave_the_last_sign_in_method() {
        let test = Arc::new(app().await);
        let first = member(&test, "alice", &[]).await;
        let id = user_id(&test, "alice").await;
        let passkey = add_passkey(&test, &id, "Phone").await;
        identities::link(
            test.app.pool(),
            &id,
            "example",
            "sub-1",
            "alice@example.com",
            Utc::now(),
        )
        .await
        .expect("link");
        // Started before alice's row is held: a session insert takes that
        // row `FOR SHARE`, so it would wait on the holder below forever.
        let second = Browser::session_for(&test, "alice").await;
        let pool = test.app.pool().clone();
        // Hold alice's row, so both removals are queued on it before either
        // counts her methods: each would see two without the lock.
        let mut holder = pool.begin().await.expect("begin");
        sqlx::query("SELECT 1 FROM users WHERE id = $1 FOR NO KEY UPDATE")
            .bind(&id)
            .execute(&mut *holder)
            .await
            .expect("hold alice");
        // `tokio::spawn` runs each removal on its own task, like
        // `asyncio.create_task`; `move` hands each owned copies.
        let remove = |mut browser: Browser, uri: String| {
            let test = Arc::clone(&test);
            tokio::spawn(async move {
                browser
                    .send(&test, "DELETE", &uri, Payload::Empty)
                    .await
                    .status
            })
        };
        let passkey_removal = remove(first, format!("/api/v1/me/passkeys/{passkey}"));
        let identity_removal = remove(second, "/api/v1/me/identities/example".to_string());
        // Bounded, and the holder is released before any failure is
        // reported, so a broken lock order fails fast instead of hanging.
        let queued = timeout(BOUND, wait_for_lock_waiters(&pool, 2)).await;
        holder.commit().await.expect("release alice");
        queued.expect("both removals to queue on alice's row within 30 s");
        let passkey_status = timeout(BOUND, passkey_removal)
            .await
            .expect("the passkey removal to finish within 30 s")
            .expect("the passkey task");
        let identity_status = timeout(BOUND, identity_removal)
            .await
            .expect("the identity removal to finish within 30 s")
            .expect("the identity task");
        let mut statuses = [passkey_status.as_u16(), identity_status.as_u16()];
        statuses.sort_unstable();

        assert_eq!(statuses, [204, 409], "exactly one removal passes");
        let (passkeys, identities) = methods_of(&test, &id).await;
        assert_eq!(passkeys + identities, 1, "one way to sign in remains");
    }
}

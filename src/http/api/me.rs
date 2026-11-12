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
use axum::routing::{delete, get, patch};
use axum::{Extension, Json, Router};
use chrono::{DateTime, TimeDelta, Utc};
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::auth::{CredentialKind, Principal};
use crate::clock::format_timestamp;
use crate::error::OkfError;
use crate::http::api::keys::{parse_access, CreatedKeyView, KeyView};
use crate::http::api::settle;
use crate::http::AppError;
use crate::store::ceiling::{self, Ceiling};
use crate::store::keys::{
    self as key_rows, Access, CatalogGrant, Grants, KeyKind, KeyRecord, NewKey,
};
use crate::store::users;

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
        ) => Err(OkfError::Forbidden {
            reason: "this needs a console session: sign in at /signin \
                         (a key cannot manage keys for itself)"
                .to_string(),
        }),
    }
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

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/me", get(account))
        .route("/me/keys", get(list_keys).post(create_key))
        .route("/me/keys/{id}", patch(rename_key).delete(revoke_key))
        .route("/me/grants", get(list_grants))
        .route("/me/grants/{id}", delete(revoke_grant))
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

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeDelta, Utc};
    use serde_json::json;

    use super::{key_expiry, requested_levels, within_ceiling, CatalogLevelBody};
    use crate::db::{TEST_TEAM, TEST_USER};
    use crate::error::OkfError;
    use crate::http::api::testing::{app, keys, send, Browser, Payload};
    use crate::mcp::testing::TestApp;
    use crate::store::ceiling::{self, Ceiling};
    use crate::store::keys::{insert_legacy_test_key, Access, Grants};
    use crate::store::oauth::clients::ClientKind;
    use crate::store::oauth::codes::{self as code_rows, insert_test_client, NewSignInCode};
    use crate::store::settings::{self, SettingKey};
    use crate::store::users::{self, NewUser};

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
            "test",
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
            "test",
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
            "test",
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
}

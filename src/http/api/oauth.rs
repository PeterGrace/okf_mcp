//! `/api/v1/oauth/codes` (OAuth design "Sign-in codes"): what
//! `okf-mcp oauth code ...` does, over HTTP. Grants are listed and revoked
//! through `/api/v1/keys`, whose views carry `kind` and `client_name`.
//!
//! Access is the key area's (`api::keys`, M9 Task 2 "Security decisions"):
//! - every route needs an admin key;
//! - an admin manages only codes inside its reach (`manages_grants`); any
//!   other code is a 404, like an unknown id;
//! - a new code may name only catalogs the caller reaches, and `all: true`
//!   needs a caller that reaches every catalog;
//! - only `POST /oauth/codes` answers a code, once; nothing answers its hash.
//!
//! Codes are read from the database on every use, never cached, so these
//! handlers do not poll (M10 Task 4, Decision 4).

use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{delete, get, put};
use axum::{Extension, Json, Router};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::auth::Principal;
use crate::clock::format_timestamp;
use crate::error::OkfError;
use crate::http::api::keys::{manages_grants, parse_access, requested_grants, AssignUserBody};
use crate::http::api::require_admin;
use crate::http::AppError;
use crate::store::oauth::codes::{self, NewSignInCode, SignInCodeRecord};

/// One sign-in code as the API shows it: never the code or its hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SignInCodeView {
    /// Its immutable id (the `{id}` of `DELETE`).
    pub id: String,
    /// Its label.
    pub name: String,
    /// The grant's actor.
    pub actor: String,
    /// `read` or `write`.
    pub access: &'static str,
    /// The grant reaches every catalog.
    pub all_catalogs: bool,
    /// The grant's catalogs, sorted.
    pub catalogs: Vec<String>,
    /// The grant's default catalog.
    pub default_catalog: Option<String>,
    /// `live`, `expired`, `redeemed` or `revoked`, as of the request.
    pub state: &'static str,
    /// When it was minted.
    pub created_at: String,
    /// When it stops working unredeemed.
    pub expires_at: String,
    /// When it was redeemed.
    pub redeemed_at: Option<String>,
    /// The grant (key id) it became.
    pub grant_id: Option<String>,
    /// When it was revoked.
    pub revoked_at: Option<String>,
    /// The user it belongs to (`None`: unassigned, from before users existed).
    pub user: Option<String>,
}

impl SignInCodeView {
    /// The view of `record` at `now` (its state depends on the time).
    ///
    /// # Arguments
    /// * `record` - the stored code.
    /// * `now` - the request's time.
    ///
    /// # Returns
    /// The view, timestamps formatted.
    pub(crate) fn new(record: SignInCodeRecord, now: DateTime<Utc>) -> Self {
        let state = record.state(now).as_str();
        // Destructuring names every field: a field added to the record later
        // fails to compile here until someone decides whether the API shows it.
        let SignInCodeRecord {
            id,
            name,
            access,
            all_catalogs,
            catalogs,
            default_catalog,
            actor,
            created_at,
            expires_at,
            redeemed_at,
            grant_id,
            revoked_at,
            user,
        } = record;
        Self {
            id,
            name,
            actor,
            access: access.as_str(),
            all_catalogs,
            catalogs,
            default_catalog,
            state,
            created_at: format_timestamp(created_at),
            expires_at: format_timestamp(expires_at),
            redeemed_at: redeemed_at.map(format_timestamp),
            grant_id,
            revoked_at: revoked_at.map(format_timestamp),
            user,
        }
    }
}

/// `GET /oauth/codes`' answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SignInCodeList {
    /// The codes the caller manages, newest first, in every state.
    pub codes: Vec<SignInCodeView>,
}

/// `POST /oauth/codes`' answer: the new code's view and, this once, the code.
// `Debug` by hand, so a stray `{:?}` can never print the code.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CreatedSignInCodeView {
    /// The new code.
    pub code: SignInCodeView,
    /// The `okfsc_` code. Not stored; it cannot be shown again.
    pub secret: String,
}

impl std::fmt::Debug for CreatedSignInCodeView {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CreatedSignInCodeView")
            .field("code", &self.code)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// `POST /oauth/codes`' body. `deny_unknown_fields` turns a misspelt field
/// into a 422.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateCodeBody {
    /// Label, `[A-Za-z0-9._-]+`.
    name: String,
    /// The grant's actor (absent or blank: the code's name).
    actor: Option<String>,
    /// `read` or `write`.
    access: String,
    /// Grant every catalog.
    #[serde(default)]
    all: bool,
    /// The catalogs to grant (without `all`).
    #[serde(default)]
    catalogs: Vec<String>,
    /// The default catalog (empty or absent: none).
    default_catalog: Option<String>,
    /// How long it stays valid (`humantime`: `2h`, `3d`; absent or empty: 24h).
    expires_in: Option<String>,
    /// The user the code, and the grant it becomes, belongs to (required;
    /// must exist).
    user: String,
}

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/oauth/codes", get(list_codes).post(create_code))
        .route("/oauth/codes/{id}", delete(revoke_code))
        .route("/oauth/codes/{id}/user", put(assign_code_user))
}

/// The code with exactly this id, if `principal` is an admin that manages it.
///
/// # Arguments
/// * `app` - the application.
/// * `principal` - the caller.
/// * `id` - the code's id.
///
/// # Returns
/// The code's record.
///
/// # Errors
/// [`OkfError::Forbidden`] below admin; [`OkfError::NotFound`] for an unknown
/// id or a code the caller does not manage (worded alike); [`OkfError::Database`].
async fn managed_code(
    app: &App,
    principal: &Principal,
    id: &str,
) -> Result<SignInCodeRecord, OkfError> {
    require_admin(principal)?;
    codes::list(app.pool())
        .await?
        .into_iter()
        .find(|code| code.id == id && manages_grants(principal, code.all_catalogs, &code.catalogs))
        .ok_or_else(|| OkfError::NotFound {
            path: format!("oauth/codes/{id}"),
        })
}

/// `GET /oauth/codes`: the codes the caller manages.
///
/// # Errors
/// 403 below admin; 500 if the codes cannot be read.
async fn list_codes(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
) -> Result<Json<SignInCodeList>, AppError> {
    require_admin(&principal)?;
    let now = app.clock().now();
    let codes = codes::list(app.pool())
        .await?
        .into_iter()
        .filter(|code| manages_grants(&principal, code.all_catalogs, &code.catalogs))
        .map(|code| SignInCodeView::new(code, now))
        .collect();
    Ok(Json(SignInCodeList { codes }))
}

/// `POST /oauth/codes`: mints a code and returns it, once.
///
/// # Errors
/// 403 below admin, or `all` from a scoped caller; 404 for a catalog the
/// caller does not reach, or an unknown user; 400 for an unknown `access` or
/// an unparsable `expires_in`; 409 for `admin` access, a bad name, no
/// catalogs, `all` with `catalogs`, an ungranted default, or an expiry past
/// `oauth.code_max_expiry`; 422 for an unknown or missing field.
async fn create_code(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Json(body): Json<CreateCodeBody>,
) -> Result<Json<CreatedSignInCodeView>, AppError> {
    require_admin(&principal)?;
    let access = parse_access(&body.access)?;
    // `filter` turns `Some("")` (an empty form field) into `None`.
    let default_catalog = body
        .default_catalog
        .as_deref()
        .filter(|name| !name.is_empty());
    let grants = requested_grants(&principal, body.all, &body.catalogs, default_catalog)?;
    let requested = match body
        .expires_in
        .as_deref()
        .map(str::trim)
        .filter(|text| !text.is_empty())
    {
        None => codes::DEFAULT_EXPIRY,
        Some(text) => humantime_serde::re::humantime::parse_duration(text).map_err(|error| {
            OkfError::InvalidPath {
                path: "expires_in".to_string(),
                reason: error.to_string(),
            }
        })?,
    };
    let now = app.clock().now();
    let expires_at = codes::expiry(now, requested, app.config().oauth.code_max_expiry)?;
    let new = NewSignInCode {
        name: &body.name,
        access,
        grants,
        default_catalog,
        // The store trims the actor and treats a blank one as the code's
        // name, so an absent one is passed as blank.
        actor: body.actor.as_deref().unwrap_or(""),
        expires_at,
        user: &body.user,
    };
    let created = codes::create(app.pool(), &new, now).await?;
    let record = managed_code(&app, &principal, &created.id).await?;
    Ok(Json(CreatedSignInCodeView {
        code: SignInCodeView::new(record, now),
        secret: created.code,
    }))
}

/// `DELETE /oauth/codes/{id}`: revokes a code nobody has redeemed.
///
/// # Errors
/// 403 below admin; 404 for a code the caller does not manage; 409 if it is
/// redeemed or already revoked.
async fn revoke_code(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(id): Path<String>,
) -> Result<Json<SignInCodeView>, AppError> {
    let target = managed_code(&app, &principal, &id).await?;
    let now = app.clock().now();
    codes::revoke(app.pool(), &target.id, now).await?;
    let record = managed_code(&app, &principal, &target.id).await?;
    Ok(Json(SignInCodeView::new(record, now)))
}

/// `PUT /oauth/codes/{id}/user`: makes the code (in any state) belong to
/// `user`. A grant it already became keeps its own user; reassign that
/// through `PUT /keys/{id}/user`. Codes are not cached, and this bumps no
/// generation, so there is nothing to poll.
///
/// # Errors
/// 403 below admin; 404 for a code the caller does not manage, or an
/// unknown user; 422 for a body without `user`.
async fn assign_code_user(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(id): Path<String>,
    Json(body): Json<AssignUserBody>,
) -> Result<Json<SignInCodeView>, AppError> {
    let target = managed_code(&app, &principal, &id).await?;
    codes::assign_user(app.pool(), &target.id, &body.user).await?;
    let record = managed_code(&app, &principal, &target.id).await?;
    Ok(Json(SignInCodeView::new(record, app.clock().now())))
}

#[cfg(test)]
mod tests {
    use chrono::{TimeDelta, Utc};
    use serde_json::json;

    use crate::http::api::testing::{app, assert_matrix, keys, send, Row};
    use crate::mcp::testing::TestApp;
    use crate::store::audit::AuditActor;
    use crate::store::keys::{Access, Grants};
    use crate::store::oauth::codes::{self, CreatedSignInCode, NewSignInCode};

    /// Mints a code straight in the store, as `okf-mcp oauth code create` would.
    async fn stored(test: &TestApp, name: &str, catalog: &str) -> CreatedSignInCode {
        let grants = [catalog.to_string()];
        let new = NewSignInCode {
            name,
            access: Access::Read,
            grants: Grants::Catalogs(&grants),
            default_catalog: None,
            actor: "example-actor",
            expires_at: Utc::now() + TimeDelta::hours(1),
            user: crate::db::TEST_USER,
        };
        codes::create(test.app.pool(), &new, Utc::now())
            .await
            .expect("mint a code")
    }

    #[tokio::test]
    async fn code_routes_need_an_admin_that_reaches_the_code() {
        let test = app().await;
        let code = stored(&test, "target", "main").await.id;
        // Callers run read, write, stranger, admin; the DELETE succeeds last.
        assert_matrix(
            &test,
            &[
                Row {
                    method: "GET",
                    uri: "/api/v1/oauth/codes".into(),
                    body: None,
                    read: 403,
                    write: 403,
                    admin: 200,
                    stranger: 200,
                },
                Row {
                    method: "POST",
                    uri: "/api/v1/oauth/codes".into(),
                    body: Some(json!({"user": crate::db::TEST_USER, "name": "made", "actor": "example-actor",
                                      "access": "read", "catalogs": ["main"]})),
                    read: 403,
                    write: 403,
                    admin: 200,
                    stranger: 404,
                },
                Row {
                    method: "PUT",
                    uri: format!("/api/v1/oauth/codes/{code}/user"),
                    body: Some(json!({"user": crate::db::TEST_USER})),
                    read: 403,
                    write: 403,
                    admin: 200,
                    stranger: 404,
                },
                Row {
                    method: "DELETE",
                    uri: format!("/api/v1/oauth/codes/{code}"),
                    body: None,
                    read: 403,
                    write: 403,
                    admin: 200,
                    stranger: 404,
                },
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn create_returns_the_code_once_and_the_list_never_shows_it() {
        let test = app().await;
        let keys = keys(&test).await;
        let body = json!({"user": crate::db::TEST_USER, "name": "laptop", "actor": "example-actor", "access": "write",
                          "catalogs": ["main"], "default_catalog": "main", "expires_in": "2h"});

        let (status, created) = send(
            &test,
            "POST",
            "/api/v1/oauth/codes",
            Some(&keys.admin),
            Some(body),
        )
        .await;

        assert_eq!(status.as_u16(), 200, "{created}");
        let secret = created["secret"].as_str().expect("a secret").to_string();
        assert!(secret.starts_with(crate::keys::SIGN_IN_CODE_PREFIX));
        let code = &created["code"];
        assert_eq!(code["name"], "laptop");
        assert_eq!(code["actor"], "example-actor");
        assert_eq!(code["access"], "write");
        assert_eq!(code["all_catalogs"], false);
        assert_eq!(code["catalogs"], json!(["main"]));
        assert_eq!(code["default_catalog"], "main");
        assert_eq!(code["state"], "live");
        assert!(code["redeemed_at"].is_null() && code["grant_id"].is_null());
        assert!(code["revoked_at"].is_null());
        let created_at = chrono::DateTime::parse_from_rfc3339(code["created_at"].as_str().unwrap())
            .expect("an RFC 3339 created_at");
        let expires_at = chrono::DateTime::parse_from_rfc3339(code["expires_at"].as_str().unwrap())
            .expect("an RFC 3339 expires_at");
        assert_eq!(expires_at - created_at, TimeDelta::hours(2));
        let (status, listed) =
            send(&test, "GET", "/api/v1/oauth/codes", Some(&keys.admin), None).await;
        assert_eq!(status.as_u16(), 200);
        let text = listed.to_string();
        assert!(!text.contains(&secret));
        assert!(!text.contains("sha256") && !text.contains("secret"));
        assert_eq!(listed["codes"][0]["id"], code["id"]);
        // The code works on the consent page's lookup.
        let live = codes::find_live(test.app.pool(), &secret, Utc::now())
            .await
            .unwrap();
        assert_eq!(live.map(|record| record.name), Some("laptop".to_string()));
    }

    #[tokio::test]
    async fn create_refuses_admin_access_all_from_a_scoped_admin_and_long_expiries() {
        let test = app().await;
        let keys = keys(&test).await;
        let post = |body: serde_json::Value| {
            let test = &test;
            let key = keys.admin.clone();
            async move {
                send(test, "POST", "/api/v1/oauth/codes", Some(&key), Some(body))
                    .await
                    .0
                    .as_u16()
            }
        };
        let base = json!({"user": crate::db::TEST_USER, "name": "c", "actor": "example-actor", "access": "read",
                          "catalogs": ["main"]});
        let with = |field: &str, value: serde_json::Value| {
            let mut body = base.clone();
            body[field] = value;
            body
        };

        assert_eq!(post(with("access", json!("admin"))).await, 409);
        assert_eq!(post(with("access", json!("root"))).await, 400);
        assert_eq!(post(with("expires_in", json!("8d"))).await, 409);
        assert_eq!(post(with("expires_in", json!("soon"))).await, 400);
        assert_eq!(post(with("user", json!("nobody"))).await, 404);
        let mut no_user = base.clone();
        no_user.as_object_mut().unwrap().remove("user");
        assert_eq!(post(no_user).await, 422, "a code names its user");
        assert_eq!(post(with("name", json!("bad name"))).await, 409);
        assert_eq!(post(with("catalogs", json!([]))).await, 409);
        assert_eq!(
            post(with("all", json!(true))).await,
            409,
            "all and catalogs"
        );
        assert_eq!(
            post(json!({"user": crate::db::TEST_USER, "name": "c", "actor": "example-actor", "access": "read", "all": true}))
                .await,
            403,
            "a scoped admin cannot mint an all-catalogs code"
        );
        assert_eq!(post(with("catalogs", json!(["other"]))).await, 404);
        assert_eq!(post(with("default_catalog", json!("other"))).await, 404);
        assert_eq!(post(with("surprise", json!(1))).await, 422);
        assert!(codes::list(test.app.pool()).await.unwrap().is_empty());
        // An empty `expires_in` means the default.
        assert_eq!(post(with("expires_in", json!(""))).await, 200);
    }

    #[tokio::test]
    async fn a_scoped_admin_sees_and_revokes_only_codes_inside_its_reach() {
        let test = app().await;
        let keys = keys(&test).await;
        let mine = stored(&test, "mine", "main").await.id;
        let theirs = stored(&test, "theirs", "other").await.id;
        let delete = |id: String| {
            let test = &test;
            let key = keys.admin.clone();
            async move {
                send(
                    test,
                    "DELETE",
                    &format!("/api/v1/oauth/codes/{id}"),
                    Some(&key),
                    None,
                )
                .await
            }
        };

        let (_, listed) = send(&test, "GET", "/api/v1/oauth/codes", Some(&keys.admin), None).await;
        let (status, _) = delete(theirs.clone()).await;
        let (revoked, view) = delete(mine.clone()).await;
        let (again, _) = delete(mine.clone()).await;

        let ids: Vec<&str> = listed["codes"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|code| code["id"].as_str())
            .collect();
        assert_eq!(ids, [mine.as_str()]);
        assert_eq!(status.as_u16(), 404);
        assert_eq!(revoked.as_u16(), 200);
        assert_eq!(view["state"], "revoked");
        assert!(view["revoked_at"].is_string());
        assert_eq!(again.as_u16(), 409);
        let (_, stranger_view) = send(
            &test,
            "GET",
            "/api/v1/oauth/codes",
            Some(&keys.stranger),
            None,
        )
        .await;
        assert_eq!(stranger_view["codes"][0]["id"], theirs.as_str());
        assert_eq!(stranger_view["codes"][0]["state"], "live");
    }

    #[tokio::test]
    async fn an_absent_actor_is_the_name_and_a_code_is_reassigned() {
        let test = app().await;
        let keys = keys(&test).await;
        crate::store::users::create(
            test.app.pool(),
            &crate::store::users::NewUser {
                name: "alice",
                display_name: "",
            },
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .expect("create alice");
        let body = json!({"user": crate::db::TEST_USER, "name": "tablet", "access": "read",
                          "catalogs": ["main"]});

        let (created, view) = send(
            &test,
            "POST",
            "/api/v1/oauth/codes",
            Some(&keys.admin),
            Some(body),
        )
        .await;
        let uri = format!(
            "/api/v1/oauth/codes/{}/user",
            view["code"]["id"].as_str().expect("an id")
        );
        let (assigned, reassigned) = send(
            &test,
            "PUT",
            &uri,
            Some(&keys.admin),
            Some(json!({"user": "alice"})),
        )
        .await;
        let (unknown, error) = send(
            &test,
            "PUT",
            &uri,
            Some(&keys.admin),
            Some(json!({"user": "nobody"})),
        )
        .await;
        let (missing, _) = send(&test, "PUT", &uri, Some(&keys.admin), Some(json!({}))).await;

        assert_eq!(created.as_u16(), 200, "{view}");
        assert_eq!(view["code"]["actor"], "tablet");
        assert_eq!(view["code"]["user"], crate::db::TEST_USER);
        assert_eq!(assigned.as_u16(), 200, "{reassigned}");
        assert_eq!(reassigned["user"], "alice");
        assert_eq!(unknown.as_u16(), 404);
        assert_eq!(error["error"], "UnknownName");
        assert_eq!(missing.as_u16(), 422);
    }
}

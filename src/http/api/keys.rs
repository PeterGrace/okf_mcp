//! `/api/v1/keys` (web console design, "API": Keys): what `okf-mcp key ...`
//! does, over HTTP. The handlers use the same `store::keys` functions the CLI
//! uses, in their `*_by_id` form: a change is made to exactly the key whose
//! reach was checked, never to another one found by its name (a legacy id is
//! also a name, and the CLI's `*_target` functions resolve it as one).
//!
//! Access (M9 Task 2, "Security decisions"):
//! - every route needs an admin key;
//! - an admin manages only keys inside its reach: all of them if it reaches
//!   every catalog, else the keys that are not all-catalogs keys and whose
//!   every catalog it reaches. Any other key is a 404, like an unknown id;
//! - a new key, or a grant, may name only catalogs the caller reaches, and
//!   `all: true` needs a caller that reaches every catalog;
//! - no response but `POST /keys` holds a secret, and none holds a hash.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::{Path, State};
use axum::routing::{get, patch, post, put};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::auth::{Principal, Reach};
use crate::clock::format_timestamp;
use crate::error::OkfError;
use crate::http::api::{require_admin, settle};
use crate::http::AppError;
use crate::store::keys::{self as key_rows, Access, Grants, KeyRecord, NewKey};

/// One key as the API shows it: never its secret or its hash.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct KeyView {
    /// Its immutable id (the `{id}` of the other routes).
    pub id: String,
    /// Its grouping name.
    pub name: String,
    /// The actor its writes are recorded as.
    pub actor: String,
    /// `read`, `write` or `admin`.
    pub access: &'static str,
    /// `key` or `oauth` (a grant made by redeeming a sign-in code).
    pub kind: &'static str,
    /// A grant's client, as it names itself (`None` for a key).
    pub client_name: Option<String>,
    /// Whether it may submit maintenance reports.
    pub maintenance_report: bool,
    /// Granted every catalog, present and future.
    pub all_catalogs: bool,
    /// Granted catalogs, sorted (empty with `all_catalogs`).
    pub catalogs: Vec<String>,
    /// Each granted catalog's level (empty with `all_catalogs`).
    pub catalog_access: BTreeMap<String, &'static str>,
    /// Its default catalog.
    pub default_catalog: Option<String>,
    /// When it was created (`2026-11-09T12:00:00Z`).
    pub created_at: String,
    /// When it was revoked, if it was.
    pub revoked_at: Option<String>,
    /// When it stops working, if ever.
    pub expires_at: Option<String>,
    /// Made by its user in the console (capped by the user's ceiling)
    /// rather than by an admin.
    pub self_issued: bool,
    /// The user it belongs to (`None`: unassigned, from before users existed).
    pub user: Option<String>,
}

impl From<KeyRecord> for KeyView {
    fn from(record: KeyRecord) -> Self {
        // Destructuring names every field: `sha256: _` drops the hash on
        // purpose, and a field added to `KeyRecord` later fails to compile
        // here until someone decides whether the API may show it.
        let KeyRecord {
            id,
            name,
            sha256: _,
            access,
            maintenance_report,
            all_catalogs,
            catalogs,
            catalog_access,
            default_catalog,
            actor,
            created_at,
            revoked_at,
            expires_at,
            kind,
            // `client_id: _`: a URL or UUID the console has no use for.
            client_id: _,
            client_name,
            user,
            // The API names users; their ids stay internal.
            user_id: _,
            self_issued,
        } = record;
        Self {
            id,
            name,
            actor,
            access: access.as_str(),
            kind: kind.as_str(),
            client_name,
            maintenance_report,
            all_catalogs,
            catalogs,
            catalog_access: catalog_access
                .into_iter()
                .map(|(catalog, level)| (catalog, level.as_str()))
                .collect(),
            default_catalog,
            created_at: format_timestamp(created_at),
            // `Option::map` with a function name applies it to the value, if any.
            revoked_at: revoked_at.map(format_timestamp),
            expires_at: expires_at.map(format_timestamp),
            self_issued,
            user,
        }
    }
}

/// `GET /keys`' answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct KeyList {
    /// The keys the caller manages, revoked ones included, in the store's
    /// order (name, actor, creation).
    pub keys: Vec<KeyView>,
}

/// `POST /keys`' answer: the new key and, this once, its secret.
// `Debug` is written by hand (not derived) so that a stray `{:?}` in a log
// line can never print the secret.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CreatedKeyView {
    /// The new key.
    pub key: KeyView,
    /// The bearer secret. It is not stored and cannot be shown again.
    pub secret: String,
}

impl std::fmt::Debug for CreatedKeyView {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CreatedKeyView")
            .field("key", &self.key)
            .field("secret", &"<redacted>")
            .finish()
    }
}

/// `POST /keys`' body. `deny_unknown_fields` turns a misspelt field into a
/// 422 instead of silently ignoring it.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateKeyBody {
    /// Grouping name, `[A-Za-z0-9._-]+`.
    name: String,
    /// `read`, `write` or `admin`. A string, because `Access` has no serde
    /// derive; parsed with `Access::parse`.
    access: String,
    /// Grant every catalog, present and future.
    #[serde(default)]
    all: bool,
    /// The catalogs to grant (without `all`).
    #[serde(default)]
    catalogs: Vec<String>,
    /// The default catalog (empty or absent: none).
    default_catalog: Option<String>,
    /// The actor (empty or absent: the name, as the CLI does).
    actor: Option<String>,
    /// May submit maintenance reports.
    #[serde(default)]
    maintenance_report: bool,
    /// The user it belongs to (required; must exist).
    user: String,
}

/// `PATCH /keys/{id}`'s body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateKeyBody {
    /// The new maintenance-report flag.
    maintenance_report: bool,
}

/// `PUT /keys/{id}/user`'s and `PUT /oauth/codes/{id}/user`'s body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct AssignUserBody {
    /// The user the credential now belongs to (must exist).
    pub user: String,
}

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/keys", get(list_keys).post(create_key))
        .route("/keys/{id}", patch(update_key))
        .route("/keys/{id}/user", put(assign_user))
        .route("/keys/{id}/revoke", post(revoke_key))
        .route(
            "/keys/{id}/catalogs/{catalog}",
            put(grant_catalog).delete(ungrant_catalog),
        )
}

/// Whether `principal` manages a credential with these grants (module doc,
/// "Access"); keys and sign-in codes (`api::oauth`) share this rule.
///
/// # Arguments
/// * `principal` - the caller.
/// * `all_catalogs` - the credential reaches every catalog.
/// * `catalogs` - its named catalogs.
///
/// # Returns
/// `true` for a caller reaching every catalog; otherwise `true` only for a
/// credential that is not all-catalogs and whose every catalog the caller reaches.
pub(crate) fn manages_grants(
    principal: &Principal,
    all_catalogs: bool,
    catalogs: &[String],
) -> bool {
    match &principal.reach {
        Reach::All => true,
        Reach::Catalogs(_) => {
            !all_catalogs && catalogs.iter().all(|catalog| principal.reaches(catalog))
        }
    }
}

/// Whether `principal` may see and change `record` ([`manages_grants`]).
///
/// # Arguments
/// * `principal` - the caller.
/// * `record` - the key or grant.
///
/// # Returns
/// As [`manages_grants`] for the record's grants.
pub(crate) fn manages(principal: &Principal, record: &KeyRecord) -> bool {
    manages_grants(principal, record.all_catalogs, &record.catalogs)
}

/// The grants a caller asks a new credential to have, checked against its
/// reach: shared by `POST /keys` and `POST /oauth/codes`.
///
/// # Arguments
/// * `principal` - the caller (already checked to be admin).
/// * `all` - the body's `all`.
/// * `catalogs` - the body's `catalogs`.
/// * `default_catalog` - the body's default catalog, empty already mapped to `None`.
///
/// # Returns
/// The grants, borrowing `catalogs`.
///
/// # Errors
/// [`OkfError::Refused`] for `all` together with `catalogs`;
/// [`OkfError::Forbidden`] for `all` from a caller that does not reach every
/// catalog; [`OkfError::CatalogNotSelected`] for a catalog or default the
/// caller does not reach.
pub(crate) fn requested_grants<'a>(
    principal: &Principal,
    all: bool,
    catalogs: &'a [String],
    default_catalog: Option<&str>,
) -> Result<Grants<'a>, OkfError> {
    let grants = if all {
        if !catalogs.is_empty() {
            return Err(OkfError::Refused {
                reason: "give `all` or `catalogs`, not both".to_string(),
            });
        }
        if !matches!(principal.reach, Reach::All) {
            return Err(OkfError::Forbidden {
                reason: "only a key that reaches every catalog may grant every catalog".to_string(),
            });
        }
        Grants::All
    } else {
        for catalog in catalogs {
            principal.require(Access::Admin, catalog)?;
        }
        Grants::Catalogs(catalogs)
    };
    if let Some(name) = default_catalog {
        principal.require(Access::Admin, name)?;
    }
    Ok(grants)
}

/// Parses a body's `access` field.
///
/// # Arguments
/// * `text` - the field's value.
///
/// # Returns
/// The level.
///
/// # Errors
/// [`OkfError::InvalidPath`] (400) naming the `access` field.
pub(crate) fn parse_access(text: &str) -> Result<Access, OkfError> {
    Access::parse(text).ok_or_else(|| OkfError::InvalidPath {
        path: "access".to_string(),
        reason: format!("must be read, write or admin, not {text:?}"),
    })
}

/// The key with exactly this id, if `principal` is an admin that manages it.
///
/// # Errors
/// [`OkfError::Forbidden`] below admin; [`OkfError::NotFound`] for an
/// unknown id or a key the caller does not manage (worded alike);
/// [`OkfError::Database`].
async fn managed(app: &App, principal: &Principal, id: &str) -> Result<KeyRecord, OkfError> {
    require_admin(principal)?;
    key_rows::list(app.pool())
        .await?
        .into_iter()
        .find(|record| record.id == id && manages(principal, record))
        .ok_or_else(|| OkfError::NotFound {
            path: format!("keys/{id}"),
        })
}

/// Reloads the key cache at once (plan Decision 12), then answers with the
/// key as it now is. The change is already committed, so a failed reload is
/// logged, not returned: the background poll retries within `poll_interval`.
///
/// # Errors
/// As [`managed`].
async fn changed(app: &App, principal: &Principal, id: &str) -> Result<Json<KeyView>, AppError> {
    settle(app).await;
    Ok(Json(managed(app, principal, id).await?.into()))
}

/// `GET /keys`: the keys the caller manages.
///
/// # Errors
/// 403 below admin; 500 if the keys cannot be read.
async fn list_keys(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
) -> Result<Json<KeyList>, AppError> {
    require_admin(&principal)?;
    let keys = key_rows::list(app.pool())
        .await?
        .into_iter()
        .filter(|record| manages(&principal, record))
        .map(KeyView::from)
        .collect();
    Ok(Json(KeyList { keys }))
}

/// `POST /keys`: creates a key and returns its secret, once.
///
/// # Errors
/// 403 below admin, or `all: true` from a caller that does not reach every
/// catalog; 404 for a catalog the caller does not reach (or that does not
/// exist) or an unknown user; 400 for an unknown `access`; 409 for a bad or
/// taken name, no catalogs, `all` together with `catalogs`, or an ungranted
/// default; 422 for a body without `user`.
async fn create_key(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Json(body): Json<CreateKeyBody>,
) -> Result<Json<CreatedKeyView>, AppError> {
    require_admin(&principal)?;
    let access = parse_access(&body.access)?;
    // `filter` turns `Some("")` (an empty form field) into `None`.
    let default_catalog = body
        .default_catalog
        .as_deref()
        .filter(|name| !name.is_empty());
    let grants = requested_grants(&principal, body.all, &body.catalogs, default_catalog)?;
    let actor = body
        .actor
        .as_deref()
        .map(str::trim)
        .filter(|actor| !actor.is_empty())
        .unwrap_or(&body.name);
    let new = NewKey {
        name: &body.name,
        access,
        maintenance_report: body.maintenance_report,
        grants,
        default_catalog,
        actor,
        user: &body.user,
        self_issued: false,
        expires_at: None,
        levels: &[],
    };
    let created = key_rows::create_with_id(app.pool(), &new, app.clock().now()).await?;
    let Json(key) = changed(&app, &principal, &created.id).await?;
    Ok(Json(CreatedKeyView {
        key,
        secret: created.key,
    }))
}

/// `PATCH /keys/{id}`: sets the maintenance-report flag.
///
/// # Errors
/// 403 below admin; 404 for a key the caller does not manage; 409 for a
/// revoked key, or for enabling reporting on an OAuth grant.
async fn update_key(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(id): Path<String>,
    Json(body): Json<UpdateKeyBody>,
) -> Result<Json<KeyView>, AppError> {
    let target = managed(&app, &principal, &id).await?;
    key_rows::set_maintenance_report_by_id(app.pool(), &target.id, body.maintenance_report).await?;
    changed(&app, &principal, &target.id).await
}

/// `PUT /keys/{id}/user`: makes the key or grant (live or revoked) belong to
/// `user`, then reloads the key cache, so the next request it makes runs as
/// that user.
///
/// # Errors
/// 403 below admin; 404 for a key the caller does not manage, or an unknown
/// user; 422 for a body without `user`.
async fn assign_user(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(id): Path<String>,
    Json(body): Json<AssignUserBody>,
) -> Result<Json<KeyView>, AppError> {
    let target = managed(&app, &principal, &id).await?;
    key_rows::assign_user_by_id(app.pool(), &target.id, &body.user).await?;
    changed(&app, &principal, &target.id).await
}

/// `POST /keys/{id}/revoke`: the key authenticates nothing from its next
/// request on (the handler reloads the key cache).
///
/// # Errors
/// 403 below admin; 404 for a key the caller does not manage; 409 if it is
/// already revoked.
async fn revoke_key(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(id): Path<String>,
) -> Result<Json<KeyView>, AppError> {
    let target = managed(&app, &principal, &id).await?;
    key_rows::revoke_by_id(app.pool(), &target.id, app.clock().now()).await?;
    changed(&app, &principal, &target.id).await
}

/// `PUT /keys/{id}/catalogs/{catalog}`: grants a catalog (already granted:
/// no change).
///
/// # Errors
/// 403 below admin; 404 for a catalog the caller does not reach or a key it
/// does not manage; 409 for a revoked or all-catalogs key.
async fn grant_catalog(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    // Two path parameters arrive as a tuple, in the order of the pattern.
    Path((id, catalog)): Path<(String, String)>,
) -> Result<Json<KeyView>, AppError> {
    require_admin(&principal)?;
    principal.require(Access::Admin, &catalog)?;
    let target = managed(&app, &principal, &id).await?;
    key_rows::grant_by_id(app.pool(), &target.id, &catalog).await?;
    changed(&app, &principal, &target.id).await
}

/// `DELETE /keys/{id}/catalogs/{catalog}`: removes a grant, and the default
/// if it was that catalog (not granted: no change).
///
/// # Errors
/// As [`grant_catalog`].
async fn ungrant_catalog(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path((id, catalog)): Path<(String, String)>,
) -> Result<Json<KeyView>, AppError> {
    require_admin(&principal)?;
    principal.require(Access::Admin, &catalog)?;
    let target = managed(&app, &principal, &id).await?;
    key_rows::ungrant_by_id(app.pool(), &target.id, &catalog).await?;
    changed(&app, &principal, &target.id).await
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;

    use crate::http::api::testing::{app, assert_matrix, keys, send, Row};
    use crate::mcp::testing::TestApp;
    use crate::store::keys::{self as key_rows, Access, CreatedKey, Grants, NewKey};

    /// Creates a key straight in the store, as `okf-mcp key create` would,
    /// and polls so the server knows it.
    async fn stored(test: &TestApp, name: &str, access: Access, grants: Grants<'_>) -> CreatedKey {
        let new = NewKey {
            name,
            access,
            maintenance_report: false,
            grants,
            default_catalog: None,
            actor: name,
            user: crate::db::TEST_USER,
            self_issued: false,
            expires_at: None,
            levels: &[],
        };
        let created = key_rows::create_with_id(test.app.pool(), &new, Utc::now())
            .await
            .expect("create a key");
        test.app.poll().await.expect("poll the key in");
        created
    }

    fn main_only() -> Vec<String> {
        vec!["main".to_string()]
    }

    #[tokio::test]
    async fn key_routes_need_an_admin_that_reaches_the_key() {
        let test = app().await;
        let main = main_only();
        let first = stored(&test, "target-one", Access::Read, Grants::Catalogs(&main))
            .await
            .id;
        let second = stored(&test, "target-two", Access::Read, Grants::Catalogs(&main))
            .await
            .id;
        // Rows run read, write, stranger, admin; only admin succeeds, last.
        // `first` is revoked by its last row, so its other rows come before.
        assert_matrix(
            &test,
            &[
                Row {
                    method: "GET",
                    uri: "/api/v1/keys".into(),
                    body: None,
                    read: 403,
                    write: 403,
                    admin: 200,
                    stranger: 200,
                },
                Row {
                    method: "POST",
                    uri: "/api/v1/keys".into(),
                    body: Some(
                        json!({"user": crate::db::TEST_USER, "name": "made", "access": "read",
                                        "catalogs": ["main"], "default_catalog": "main"}),
                    ),
                    read: 403,
                    write: 403,
                    admin: 200,
                    stranger: 404,
                },
                Row {
                    method: "PUT",
                    uri: format!("/api/v1/keys/{first}/catalogs/main"),
                    body: None,
                    read: 403,
                    write: 403,
                    admin: 200,
                    stranger: 404,
                },
                Row {
                    method: "PUT",
                    uri: format!("/api/v1/keys/{first}/user"),
                    body: Some(json!({"user": crate::db::TEST_USER})),
                    read: 403,
                    write: 403,
                    admin: 200,
                    stranger: 404,
                },
                Row {
                    method: "PATCH",
                    uri: format!("/api/v1/keys/{first}"),
                    body: Some(json!({"maintenance_report": true})),
                    read: 403,
                    write: 403,
                    admin: 200,
                    stranger: 404,
                },
                Row {
                    method: "POST",
                    uri: format!("/api/v1/keys/{first}/revoke"),
                    body: None,
                    read: 403,
                    write: 403,
                    admin: 200,
                    stranger: 404,
                },
                Row {
                    method: "DELETE",
                    uri: format!("/api/v1/keys/{second}/catalogs/main"),
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

    /// M13 Task 3: the key routes that name a catalog check
    /// `require(Admin, ..)` on it (create with `catalogs` and
    /// `default_catalog`, grant, ungrant). Below admin `require_admin`
    /// refuses first, with `Forbidden`; an admin, who is exempt, still
    /// manages keys of a suspended catalog.
    #[tokio::test]
    async fn key_routes_on_a_suspended_catalog_stay_open_to_an_admin() {
        use crate::http::api::testing::{assert_answers, Expect};
        use crate::mcp::testing::suspend_catalogs;
        let test = app().await;
        let keys = keys(&test).await;
        let main = main_only();
        let first = stored(&test, "target-one", Access::Read, Grants::Catalogs(&main))
            .await
            .id;
        let second = stored(&test, "target-two", Access::Read, Grants::Catalogs(&main))
            .await
            .id;
        suspend_catalogs(&test, &["main"]).await;
        let create = || {
            Some(
                json!({"user": crate::db::TEST_USER, "name": "made", "access": "read",
                        "catalogs": ["main"], "default_catalog": "main"}),
            )
        };
        let grant = format!("/api/v1/keys/{first}/catalogs/main");
        let ungrant = format!("/api/v1/keys/{second}/catalogs/main");
        let expect = |method, uri, body, who, key, status, error| Expect {
            method,
            uri,
            body,
            key,
            who,
            status,
            error,
        };

        assert_answers(
            &test,
            &[
                expect(
                    "POST",
                    "/api/v1/keys",
                    create(),
                    "write",
                    &keys.write,
                    403,
                    Some("Forbidden"),
                ),
                expect(
                    "POST",
                    "/api/v1/keys",
                    create(),
                    "admin",
                    &keys.admin,
                    200,
                    None,
                ),
                expect(
                    "PUT",
                    &grant,
                    None,
                    "write",
                    &keys.write,
                    403,
                    Some("Forbidden"),
                ),
                expect("PUT", &grant, None, "admin", &keys.admin, 200, None),
                expect(
                    "DELETE",
                    &ungrant,
                    None,
                    "write",
                    &keys.write,
                    403,
                    Some("Forbidden"),
                ),
                expect("DELETE", &ungrant, None, "admin", &keys.admin, 200, None),
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn create_returns_the_secret_once_and_the_new_key_works() {
        let test = app().await;
        let keys = keys(&test).await;
        let body = json!({"user": crate::db::TEST_USER, "name": "fresh", "access": "write", "catalogs": ["main"],
                          "default_catalog": "main", "actor": "agent/fresh"});
        let (status, created) =
            send(&test, "POST", "/api/v1/keys", Some(&keys.admin), Some(body)).await;
        assert_eq!(status.as_u16(), 200, "{created}");
        let secret = created["secret"].as_str().expect("a secret").to_string();
        assert!(secret.starts_with(crate::keys::KEY_PREFIX));
        assert_eq!(created["key"]["name"], "fresh");
        assert_eq!(created["key"]["actor"], "agent/fresh");
        assert_eq!(created["key"]["access"], "write");
        assert_eq!(created["key"]["catalogs"], json!(["main"]));
        assert!(created["key"].get("sha256").is_none());

        // The poll after the change means the key works on the next request.
        let (status, me) = send(&test, "GET", "/api/v1/whoami", Some(&secret), None).await;
        assert_eq!(status.as_u16(), 200);
        assert_eq!(me["name"], "fresh");

        // The list never shows a secret or a hash.
        let (_, listed) = send(&test, "GET", "/api/v1/keys", Some(&keys.admin), None).await;
        let text = listed.to_string();
        assert!(!text.contains(&secret));
        assert!(!text.contains("sha256"));
        let names: Vec<&str> = listed["keys"]
            .as_array()
            .expect("a list")
            .iter()
            .filter_map(|key| key["name"].as_str())
            .collect();
        assert!(names.contains(&"fresh"), "{names:?}");
    }

    #[tokio::test]
    async fn the_actor_defaults_to_the_name() {
        let test = app().await;
        let keys = keys(&test).await;
        let body = json!({"user": crate::db::TEST_USER, "name": "plain", "access": "read", "catalogs": ["main"]});
        let (status, created) =
            send(&test, "POST", "/api/v1/keys", Some(&keys.admin), Some(body)).await;
        assert_eq!(status.as_u16(), 200, "{created}");
        assert_eq!(created["key"]["actor"], "plain");
        assert_eq!(created["key"]["default_catalog"], serde_json::Value::Null);
    }

    #[tokio::test]
    async fn create_stays_within_the_callers_reach() {
        let test = app().await;
        let keys = keys(&test).await;
        let root = stored(&test, "root", Access::Admin, Grants::All).await;
        let post = |body: serde_json::Value, key: String| {
            let test = &test;
            async move { send(test, "POST", "/api/v1/keys", Some(&key), Some(body)).await }
        };
        // A catalog the admin does not reach looks like an unknown one.
        let (status, _) = post(
            json!({"user": crate::db::TEST_USER, "name": "x1", "access": "read", "catalogs": ["other"]}),
            keys.admin.clone(),
        )
        .await;
        assert_eq!(status.as_u16(), 404);
        // Only a key that reaches every catalog may make one that does.
        let (status, _) = post(
            json!({"user": crate::db::TEST_USER, "name": "x2", "access": "read", "all": true}),
            keys.admin.clone(),
        )
        .await;
        assert_eq!(status.as_u16(), 403);
        let (status, _) = post(
            json!({"user": crate::db::TEST_USER, "name": "x3", "access": "read", "all": true}),
            root.key.clone(),
        )
        .await;
        assert_eq!(status.as_u16(), 200);
        // Bad input.
        let (status, body) = post(
            json!({"user": crate::db::TEST_USER, "name": "x4", "access": "owner", "catalogs": ["main"]}),
            keys.admin.clone(),
        )
        .await;
        assert_eq!(status.as_u16(), 400);
        assert_eq!(body["error"], "InvalidPath");
        let (status, _) = post(
            json!({"user": crate::db::TEST_USER, "name": "bad name", "access": "read", "catalogs": ["main"]}),
            keys.admin.clone(),
        )
        .await;
        assert_eq!(status.as_u16(), 409);
        let (status, _) = post(
            json!({"user": crate::db::TEST_USER, "name": "x5", "access": "read", "all": true, "catalogs": ["main"]}),
            root.key.clone(),
        )
        .await;
        assert_eq!(status.as_u16(), 409);
        let (status, _) = post(
            json!({"user": crate::db::TEST_USER, "name": "x6", "access": "read", "catalogs": ["main"], "extra": 1}),
            keys.admin.clone(),
        )
        .await;
        assert_eq!(
            status.as_u16(),
            422,
            "unknown fields are refused by the Json extractor"
        );
        let (status, body) = post(
            json!({"user": "nobody", "name": "x7", "access": "read", "catalogs": ["main"]}),
            keys.admin.clone(),
        )
        .await;
        assert_eq!(status.as_u16(), 404);
        assert_eq!(body["error"], "UnknownName");
        let (status, _) = post(
            json!({"name": "x8", "access": "read", "catalogs": ["main"]}),
            keys.admin.clone(),
        )
        .await;
        assert_eq!(status.as_u16(), 422, "a key names its user");

        // The scoped admin does not see the all-catalogs keys; root sees every key.
        let names = |listed: &serde_json::Value| -> Vec<String> {
            listed["keys"]
                .as_array()
                .expect("a list")
                .iter()
                .filter_map(|key| key["name"].as_str().map(str::to_string))
                .collect()
        };
        let (_, scoped) = send(&test, "GET", "/api/v1/keys", Some(&keys.admin), None).await;
        let scoped = names(&scoped);
        assert!(
            !scoped
                .iter()
                .any(|name| name == "root" || name == "x3" || name == "stranger"),
            "{scoped:?}"
        );
        let (_, everything) = send(&test, "GET", "/api/v1/keys", Some(&root.key), None).await;
        let everything = names(&everything);
        assert!(
            ["root", "x3", "stranger", "admin"]
                .iter()
                .all(|name| everything.iter().any(|n| n == name)),
            "{everything:?}"
        );
    }

    #[tokio::test]
    async fn grant_and_ungrant_stay_within_reach() {
        let test = app().await;
        let keys = keys(&test).await;
        let root = stored(&test, "root", Access::Admin, Grants::All).await;
        let main = main_only();
        let target = stored(&test, "target", Access::Read, Grants::Catalogs(&main))
            .await
            .id;

        // The scoped admin cannot grant a catalog it does not reach.
        let uri = format!("/api/v1/keys/{target}/catalogs/other");
        let (status, _) = send(&test, "PUT", &uri, Some(&keys.admin), None).await;
        assert_eq!(status.as_u16(), 404);
        // Root can; then the key reaches beyond the scoped admin, who loses it.
        let (status, granted) = send(&test, "PUT", &uri, Some(&root.key), None).await;
        assert_eq!(status.as_u16(), 200, "{granted}");
        assert_eq!(granted["catalogs"], json!(["main", "other"]));
        let main_uri = format!("/api/v1/keys/{target}/catalogs/main");
        let (status, _) = send(&test, "DELETE", &main_uri, Some(&keys.admin), None).await;
        assert_eq!(status.as_u16(), 404);
        // Root takes `other` away again.
        let (status, ungranted) = send(&test, "DELETE", &uri, Some(&root.key), None).await;
        assert_eq!(status.as_u16(), 200);
        assert_eq!(ungranted["catalogs"], json!(["main"]));
        // An unknown catalog, and an unknown key id.
        let (status, _) = send(
            &test,
            "PUT",
            &format!("/api/v1/keys/{target}/catalogs/nope"),
            Some(&root.key),
            None,
        )
        .await;
        assert_eq!(status.as_u16(), 404);
        let (status, _) = send(
            &test,
            "PUT",
            "/api/v1/keys/no-such-id/catalogs/main",
            Some(&root.key),
            None,
        )
        .await;
        assert_eq!(status.as_u16(), 404);
        // A name is not an id: `{id}` never falls back to name resolution.
        let (status, _) = send(
            &test,
            "POST",
            "/api/v1/keys/target/revoke",
            Some(&root.key),
            None,
        )
        .await;
        assert_eq!(status.as_u16(), 404);
    }

    #[tokio::test]
    async fn reporting_and_revoke_take_effect_at_once() {
        let test = app().await;
        let keys = keys(&test).await;
        let main = main_only();
        let target = stored(&test, "target", Access::Read, Grants::Catalogs(&main)).await;

        let (status, patched) = send(
            &test,
            "PATCH",
            &format!("/api/v1/keys/{}", target.id),
            Some(&keys.admin),
            Some(json!({"maintenance_report": true})),
        )
        .await;
        assert_eq!(status.as_u16(), 200);
        assert_eq!(patched["maintenance_report"], true);
        let (_, me) = send(&test, "GET", "/api/v1/whoami", Some(&target.key), None).await;
        assert_eq!(me["maintenance_report"], true);

        let revoke = format!("/api/v1/keys/{}/revoke", target.id);
        let (status, revoked) = send(&test, "POST", &revoke, Some(&keys.admin), None).await;
        assert_eq!(status.as_u16(), 200);
        assert!(revoked["revoked_at"].is_string());
        let (status, _) = send(&test, "GET", "/api/v1/whoami", Some(&target.key), None).await;
        assert_eq!(
            status.as_u16(),
            401,
            "the revoked key is refused on its next request"
        );
        let (status, _) = send(&test, "POST", &revoke, Some(&keys.admin), None).await;
        assert_eq!(status.as_u16(), 409, "already revoked");
    }

    #[tokio::test]
    async fn a_legacy_id_changes_only_its_own_key() {
        let test = app().await;
        let root = stored(&test, "root", Access::Admin, Grants::All).await;
        // A pre-0005 row, id == name, and a newer live key of the same name.
        key_rows::insert_legacy_test_key(
            test.app.pool(),
            "legacy",
            Access::Write,
            "terminal",
            Utc::now(),
        )
        .await;
        let new = NewKey {
            name: "legacy",
            access: Access::Read,
            maintenance_report: false,
            grants: Grants::Catalogs(&main_only()),
            default_catalog: None,
            actor: "web",
            user: crate::db::TEST_USER,
            self_issued: false,
            expires_at: None,
            levels: &[],
        };
        let web = key_rows::create_with_id(test.app.pool(), &new, Utc::now())
            .await
            .expect("create the newer key");
        test.app.poll().await.expect("poll the keys in");
        let state = |listed: &serde_json::Value, id: &str| -> serde_json::Value {
            listed["keys"]
                .as_array()
                .expect("a list")
                .iter()
                .find(|key| key["id"] == id)
                .map(|key| key["revoked_at"].clone())
                .expect("the key is listed")
        };

        // By name alone the store would call this ambiguous; by id it is one key.
        let (status, body) = send(
            &test,
            "POST",
            "/api/v1/keys/legacy/revoke",
            Some(&root.key),
            None,
        )
        .await;
        assert_eq!(status.as_u16(), 200, "{body}");
        let (_, listed) = send(&test, "GET", "/api/v1/keys", Some(&root.key), None).await;
        assert!(state(&listed, "legacy").is_string());
        assert!(
            state(&listed, &web.id).is_null(),
            "the newer key stays live"
        );

        // Revoked, the legacy id must not resolve to a live key of its name and actor.
        let replacement = NewKey {
            actor: "terminal",
            ..new
        };
        let replacement = key_rows::create_with_id(test.app.pool(), &replacement, Utc::now())
            .await
            .expect("create the replacement");
        test.app.poll().await.expect("poll the keys in");
        let (status, _) = send(
            &test,
            "POST",
            "/api/v1/keys/legacy/revoke",
            Some(&root.key),
            None,
        )
        .await;
        assert_eq!(status.as_u16(), 409);
        let (_, listed) = send(&test, "GET", "/api/v1/keys", Some(&root.key), None).await;
        assert!(
            state(&listed, &replacement.id).is_null(),
            "the replacement stays live"
        );
    }

    #[tokio::test]
    async fn a_grant_is_managed_by_id_but_never_given_maintenance_reporting() {
        use crate::store::oauth::testing::{insert_client, insert_grant};

        let test = app().await;
        let keys = keys(&test).await;
        let client = insert_client(
            test.app.pool(),
            "https://client.example.com/client.json",
            "Example Client",
            Utc::now(),
        )
        .await;
        let grant = insert_grant(
            test.app.pool(),
            &client,
            Access::Read,
            &["main"],
            Utc::now(),
        )
        .await;
        let uri = format!("/api/v1/keys/{grant}");

        let (enable, refused) = send(
            &test,
            "PATCH",
            &uri,
            Some(&keys.admin),
            Some(json!({"maintenance_report": true})),
        )
        .await;
        let (revoke, revoked) = send(
            &test,
            "POST",
            &format!("{uri}/revoke"),
            Some(&keys.admin),
            None,
        )
        .await;

        assert_eq!(enable.as_u16(), 409, "{refused}");
        assert_eq!(revoke.as_u16(), 200, "{revoked}");
        assert_eq!(revoked["kind"], "oauth");
        assert_eq!(revoked["client_name"], "Example Client");
        assert_eq!(revoked["maintenance_report"], false);
        assert!(revoked["revoked_at"].is_string());
    }

    #[tokio::test]
    async fn the_key_list_shows_each_catalogs_level_expiry_and_who_issued_it() {
        let test = app().await;
        let keys = keys(&test).await;

        let (status, listed) = send(&test, "GET", "/api/v1/keys", Some(&keys.admin), None).await;

        assert_eq!(status.as_u16(), 200, "{listed}");
        let reader = listed["keys"]
            .as_array()
            .expect("a list")
            .iter()
            .find(|key| key["name"] == "reader")
            .expect("the reader key is listed");
        assert_eq!(reader["catalog_access"], json!({"main": "read"}));
        assert_eq!(reader["self_issued"], false);
        assert!(reader["expires_at"].is_null());
        assert!(
            reader.get("user_id").is_none(),
            "the API names users, not their ids"
        );
    }

    #[tokio::test]
    async fn reporting_is_refused_for_a_self_issued_key() {
        let test = app().await;
        let keys = keys(&test).await;
        let main = main_only();
        let new = NewKey {
            name: "mine",
            access: Access::Write,
            maintenance_report: false,
            grants: Grants::Catalogs(&main),
            default_catalog: None,
            actor: "mine",
            user: crate::db::TEST_USER,
            self_issued: true,
            expires_at: None,
            levels: &[],
        };
        let mine = key_rows::create_with_id(test.app.pool(), &new, Utc::now())
            .await
            .expect("create a self-issued key");
        test.app.poll().await.expect("poll the key in");

        let (status, refused) = send(
            &test,
            "PATCH",
            &format!("/api/v1/keys/{}", mine.id),
            Some(&keys.admin),
            Some(json!({"maintenance_report": true})),
        )
        .await;

        assert_eq!(status.as_u16(), 422, "{refused}");
        assert_eq!(refused["limit"], "maintenance_report", "{refused}");
        let (_, me) = send(&test, "GET", "/api/v1/whoami", Some(&mine.key), None).await;
        assert_eq!(me["maintenance_report"], false);
    }
}

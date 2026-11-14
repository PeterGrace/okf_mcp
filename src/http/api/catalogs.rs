//! `/api/v1/catalogs` (web console design, "API": Catalogs, Import /
//! export): the catalogs a key reaches, their administration as
//! `okf-mcp catalog create|update|delete` does it, and the archive routes
//! mounted again under `/api/v1` with the same handlers, body cap and
//! deadline as `/catalogs/{name}/export|import`.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, put};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};

use super::{require_every_catalog, settle};
use crate::app::App;
use crate::auth::Principal;
use crate::catalog::CatalogState;
use crate::clock::format_timestamp;
use crate::error::OkfError;
use crate::http::{archives, AppError};
use crate::mcp::catalogs::SuspensionInfo;
use crate::store::catalogs as catalog_rows;
use crate::store::ceiling::{self, MemberAccess};
use crate::store::keys::Access;

/// This area's routes.
///
/// # Arguments
/// * `app` - the application (the import route's body cap comes from its config).
///
/// # Returns
/// The router. Every path parameter is `{catalog}`, as in the other areas:
/// axum refuses two routes that name the same position differently.
pub(crate) fn routes(app: &App) -> Router<Arc<App>> {
    Router::new()
        // `get(a).post(b)` is one method router: one path, a handler per method.
        .route("/catalogs", get(list_catalogs).post(create_catalog))
        .route(
            "/catalogs/{catalog}",
            patch(update_catalog).delete(delete_catalog),
        )
        .route("/catalogs/{catalog}/owner", put(set_owner))
        .route(
            "/catalogs/{catalog}/member-access",
            get(member_access).put(set_member_access),
        )
        .route("/catalogs/{catalog}/export", archives::export_route())
        .route("/catalogs/{catalog}/import", archives::import_route(app))
}

/// One catalog as the console lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogSummary {
    /// Its name.
    pub name: String,
    /// Its description.
    pub description: String,
    /// The name of the team that owns it; null for a catalog from before
    /// users and teams not yet assigned.
    pub owner_team: Option<String>,
    /// How many files it holds (every stored file).
    pub files: usize,
    /// How many of them are concepts.
    pub concepts: usize,
    /// The generation of the loaded snapshot: what an import's
    /// `expect_generation` compares against.
    pub generation: i64,
    /// Its newest change (`2026-11-02T21:00:00Z`), or null when empty.
    pub last_change: Option<String>,
    /// Set when its owner team is suspended (M13): `{"team", "notice"}`;
    /// null for an active catalog.
    pub suspended: Option<SuspensionInfo>,
}

impl CatalogSummary {
    /// The summary of one loaded catalog, as `principal` sees it.
    ///
    /// # Arguments
    /// * `name` - its name.
    /// * `state` - its state.
    /// * `principal` - the caller, which must reach the catalog (its
    ///   suspension is shown only to those who do).
    fn of(name: &str, state: &CatalogState, principal: &Principal) -> Self {
        // `current()` is an `Arc` of the loaded snapshot: one consistent
        // generation, file count and concept count.
        let loaded = state.current();
        Self {
            name: name.to_string(),
            description: state.description().to_string(),
            // `as_deref` borrows the `Arc<str>` inside as a `&str`.
            owner_team: state.owner_team().as_deref().map(str::to_string),
            files: loaded.files(),
            concepts: loaded.snapshot().concepts().count(),
            generation: loaded.generation(),
            last_change: loaded.last_change().map(format_timestamp),
            suspended: principal.suspension(name).map(SuspensionInfo::from),
        }
    }
}

/// `GET /catalogs`' answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogListing {
    /// The catalogs the key reaches, by name.
    pub catalogs: Vec<CatalogSummary>,
}

/// `POST /catalogs`' body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct NewCatalog {
    /// `[a-z0-9-]+`.
    name: String,
    /// Free text; empty when left out, as with the CLI.
    #[serde(default)]
    description: String,
    /// The team that owns it. Required: a body without it is a 422.
    owner: String,
}

/// `PATCH /catalogs/{catalog}`'s body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DescriptionChange {
    /// The new description (empty is allowed).
    description: String,
}

/// `PATCH /catalogs/{catalog}`'s answer, as `okf-mcp catalog update` prints it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DescriptionChanged {
    /// The catalog.
    pub name: String,
    /// The description it had.
    pub previous: String,
    /// The description it has now.
    pub description: String,
}

/// `PUT /catalogs/{catalog}/owner`'s body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnerChange {
    /// The team that owns it from now on.
    team: String,
}

/// `PUT /catalogs/{catalog}/owner`'s answer, as `okf-mcp catalog set-owner`
/// prints it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct OwnerChanged {
    /// The catalog.
    pub name: String,
    /// The team that owned it; null when it was unassigned.
    pub previous: Option<String>,
    /// The team that owns it now.
    pub owner_team: String,
}

/// `DELETE /catalogs/{catalog}`'s query.
#[derive(Debug, Default, Deserialize)]
struct DeleteQuery {
    /// Delete even with unpublished changes in an open changeset.
    #[serde(default)]
    force: bool,
}

/// `DELETE /catalogs/{catalog}`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct CatalogDeleted {
    /// The catalog that was deleted.
    pub name: String,
}

/// `GET /catalogs`: every loaded catalog the key reaches, any access level.
///
/// # Returns
/// The listing, by name (`Catalogs` is a `BTreeMap`, so it iterates sorted).
async fn list_catalogs(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
) -> Result<Json<CatalogListing>, AppError> {
    // Held in a local so the map outlives the iteration that borrows it.
    let loaded = app.catalogs();
    let catalogs = loaded
        .iter()
        .filter(|(name, _)| principal.reaches(name))
        .map(|(name, state)| CatalogSummary::of(name, state, &principal))
        .collect();
    Ok(Json(CatalogListing { catalogs }))
}

/// `POST /catalogs`: creates an empty catalog, then polls so it is served at
/// once. It needs an admin key that reaches every catalog: a key granted
/// named catalogs would not reach the new one, and grants are made with the
/// keys routes.
///
/// # Returns
/// 201 with the new catalog's summary.
///
/// # Errors
/// 403 for any other key; 404 ([`OkfError::UnknownName`]) for an unknown
/// owner; 409 ([`OkfError::Refused`]) for an invalid or taken name; 422 for a
/// body without `owner`; 500 for a database failure.
async fn create_catalog(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    // `Json` reads the body, so it must be the last extractor.
    Json(new): Json<NewCatalog>,
) -> Result<(StatusCode, Json<CatalogSummary>), AppError> {
    require_every_catalog(&principal)?;
    let row = catalog_rows::create(
        app.pool(),
        &new.name,
        &new.description,
        &new.owner,
        app.clock().now(),
    )
    .await?;
    // Catalog rows record no actor (nor does the CLI); the log line does.
    tracing::info!(catalog = %row.name, key = %principal.key_id, actor = %principal.actor,
                   "catalog created");
    settle(&app).await;
    // The owner may be a suspended team. Creating bumps the admin generation,
    // so the poll `settle` ran reloaded the cache's suspension map; if that
    // poll failed, this reads null until the next one, as for every team write.
    let suspended = app
        .keys()
        .suspensions()
        .get(&row.name)
        .map(SuspensionInfo::from);
    // Built from the row, not the loaded state: a new catalog is empty, and
    // the answer must not depend on the poll having loaded it.
    let summary = CatalogSummary {
        name: row.name,
        description: row.description,
        owner_team: row.owner_team,
        files: 0,
        concepts: 0,
        generation: row.generation,
        last_change: None,
        suspended,
    };
    Ok((StatusCode::CREATED, Json(summary)))
}

/// `PATCH /catalogs/{catalog}`: replaces the description (`admin` on the
/// catalog), then polls so the listing shows it at once.
///
/// # Errors
/// 404 for a catalog the key does not reach or that does not exist; 403
/// below `admin`; 500 for a database failure.
async fn update_catalog(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(catalog): Path<String>,
    Json(change): Json<DescriptionChange>,
) -> Result<Json<DescriptionChanged>, AppError> {
    principal.require(Access::Admin, &catalog)?;
    let previous =
        catalog_rows::update_description(app.pool(), &catalog, &change.description).await?;
    tracing::info!(%catalog, key = %principal.key_id, actor = %principal.actor,
                   "catalog description changed");
    settle(&app).await;
    Ok(Json(DescriptionChanged {
        name: catalog,
        previous,
        description: change.description,
    }))
}

/// `PUT /catalogs/{catalog}/owner`: makes `team` the catalog's owner, then
/// polls so the listing shows it at once. Ownership grants no access, so it
/// needs an admin key that reaches every catalog, like the users and teams
/// routes (plan Decision 3), not `admin` on the catalog.
///
/// # Errors
/// 403 for any other key; 404 for an unknown catalog or team; 422 for a body
/// without `team`; 500 for a database failure.
async fn set_owner(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(catalog): Path<String>,
    Json(change): Json<OwnerChange>,
) -> Result<Json<OwnerChanged>, AppError> {
    require_every_catalog(&principal)?;
    let previous = catalog_rows::set_owner(app.pool(), &catalog, &change.team).await?;
    tracing::info!(%catalog, key = %principal.key_id, actor = %principal.actor,
                   "catalog owner changed");
    settle(&app).await;
    Ok(Json(OwnerChanged {
        name: catalog,
        previous,
        owner_team: change.team,
    }))
}

/// `PUT /catalogs/{catalog}/member-access`'s body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct MemberAccessBody {
    /// `none`, `read` or `write`.
    access: String,
}

/// `GET /catalogs/{catalog}/member-access`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct MemberAccessView {
    /// The catalog.
    pub name: String,
    /// `none`, `read` or `write`.
    pub member_access: &'static str,
}

/// `PUT /catalogs/{catalog}/member-access`'s answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct MemberAccessChanged {
    /// The catalog.
    pub name: String,
    /// The level before.
    pub previous: &'static str,
    /// The level now.
    pub member_access: &'static str,
}

/// `GET /catalogs/{catalog}/member-access`: what the owning team's members
/// may reach through their self-issued credentials.
///
/// # Errors
/// 404 for a catalog the caller does not reach or that does not exist; 403
/// below `admin` on it.
async fn member_access(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(catalog): Path<String>,
) -> Result<Json<MemberAccessView>, AppError> {
    principal.require(Access::Admin, &catalog)?;
    let access = ceiling::member_access(app.pool(), &catalog).await?;
    Ok(Json(MemberAccessView {
        name: catalog,
        member_access: access.as_str(),
    }))
}

/// `PUT /catalogs/{catalog}/member-access`: sets it, then polls so every
/// member's self-issued credentials are re-capped at once. The permission
/// is `ceiling::may_set_member_access`, the one hook team owners will
/// later share (spec "Admin surface").
///
/// # Errors
/// 403 for a caller `may_set_member_access` refuses; 400 for an unknown
/// level; 404 for an unknown catalog; 422 without `access`.
async fn set_member_access(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(catalog): Path<String>,
    Json(body): Json<MemberAccessBody>,
) -> Result<Json<MemberAccessChanged>, AppError> {
    if !ceiling::may_set_member_access(&principal, &catalog) {
        return Err(OkfError::Forbidden {
            reason: "this needs an admin key that reaches every catalog".to_string(),
        }
        .into());
    }
    let access = MemberAccess::parse(&body.access).ok_or_else(|| OkfError::InvalidPath {
        path: "access".to_string(),
        reason: format!("must be none, read or write, not {:?}", body.access),
    })?;
    let previous = ceiling::set_member_access(app.pool(), &catalog, access).await?;
    tracing::info!(%catalog, previous = previous.as_str(), access = access.as_str(),
                   key = %principal.key_id, actor = %principal.actor, "member access set");
    settle(&app).await;
    Ok(Json(MemberAccessChanged {
        name: catalog,
        previous: previous.as_str(),
        member_access: access.as_str(),
    }))
}

/// `DELETE /catalogs/{catalog}[?force=true]`: deletes the catalog with its
/// documents, history, changesets and grants (`admin` on the catalog), then
/// polls so it is gone at once.
///
/// # Errors
/// 404 for a catalog the key does not reach or that does not exist; 403
/// below `admin`; 409 ([`OkfError::Refused`]) with unpublished changes and
/// no `force`; 500 for a database failure.
async fn delete_catalog(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(catalog): Path<String>,
    Query(query): Query<DeleteQuery>,
) -> Result<Json<CatalogDeleted>, AppError> {
    principal.require(Access::Admin, &catalog)?;
    catalog_rows::delete(app.pool(), &catalog, query.force)
        .await
        // The store's hint names the CLI flag; say what HTTP needs.
        .map_err(|error| match error {
            OkfError::Refused { reason } => OkfError::Refused {
                reason: reason.replace("pass --force", "add ?force=true"),
            },
            other => other,
        })?;
    tracing::info!(%catalog, key = %principal.key_id, actor = %principal.actor,
                   force = query.force, "catalog deleted");
    settle(&app).await;
    Ok(Json(CatalogDeleted { name: catalog }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{HeaderMap, Request};
    use chrono::Utc;
    use serde_json::json;
    use tokio_util::sync::CancellationToken;
    use tower::ServiceExt;

    use crate::db::TEST_TEAM;
    use crate::http::api::testing::{
        app, assert_matrix, assert_matrix_with, every_catalog_admin, keys, send, Browser, Payload,
        Row,
    };
    use crate::http::archives::tarball_of;
    use crate::mcp::testing::{open, pending_entry, writer, PendingEdit, TestApp};
    use crate::store::audit::AuditActor;
    use crate::store::users::AdminChange;

    /// One request with a raw body through the whole router; the status,
    /// the headers and the body bytes.
    async fn raw(
        test: &TestApp,
        method: &str,
        uri: &str,
        key: &str,
        body: Vec<u8>,
    ) -> (StatusCode, HeaderMap, Vec<u8>) {
        let request = Request::builder()
            .method(method)
            .uri(uri)
            .header("authorization", format!("Bearer {key}"))
            .body(Body::from(body))
            .expect("a valid request");
        let response = crate::http::router(Arc::clone(&test.app), &CancellationToken::new())
            .oneshot(request)
            .await
            .expect("the router answers");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read the body");
        (status, headers, bytes.to_vec())
    }

    /// The names a `GET /catalogs` lists for `key`.
    async fn names(test: &TestApp, key: &str) -> Vec<String> {
        let (status, body) = send(test, "GET", "/api/v1/catalogs", Some(key), None).await;
        assert_eq!(status, StatusCode::OK, "{body}");
        body["catalogs"]
            .as_array()
            .expect("a list")
            .iter()
            .map(|catalog| catalog["name"].as_str().expect("a name").to_string())
            .collect()
    }

    #[tokio::test]
    async fn member_access_reads_need_admin_on_the_catalog_and_writes_an_every_catalog_admin() {
        let test = app().await;
        let keys = keys(&test).await;
        let root = every_catalog_admin(&test).await;
        assert_matrix_with(
            &test,
            &keys,
            &[
                Row {
                    method: "GET",
                    uri: "/api/v1/catalogs/main/member-access".to_string(),
                    body: None,
                    read: 403,
                    write: 403,
                    admin: 200,
                    stranger: 404,
                },
                Row {
                    method: "PUT",
                    uri: "/api/v1/catalogs/main/member-access".to_string(),
                    body: Some(json!({"access": "read"})),
                    read: 403,
                    write: 403,
                    admin: 403,
                    stranger: 403,
                },
            ],
        )
        .await;
        let put = |catalog: &str, access: &str| {
            let uri = format!("/api/v1/catalogs/{catalog}/member-access");
            let body = json!({ "access": access });
            let (test, root) = (&test, root.as_str());
            async move { send(test, "PUT", &uri, Some(root), Some(body)).await }
        };

        let (_, before) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/member-access",
            Some(&root),
            None,
        )
        .await;
        let (set, body) = put("main", "read").await;
        let (_, now) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/member-access",
            Some(&root),
            None,
        )
        .await;
        let (bad, _) = put("main", "admin").await;
        let (unknown, _) = put("nowhere", "read").await;
        let (unknown_read, _) = send(
            &test,
            "GET",
            "/api/v1/catalogs/nowhere/member-access",
            Some(&root),
            None,
        )
        .await;

        assert_eq!(before, json!({"name": "main", "member_access": "write"}));
        assert_eq!(set.as_u16(), 200, "{body}");
        assert_eq!(
            body,
            json!({"name": "main", "previous": "write", "member_access": "read"})
        );
        assert_eq!(now, json!({"name": "main", "member_access": "read"}));
        assert_eq!(bad.as_u16(), 400);
        assert_eq!(unknown.as_u16(), 404);
        assert_eq!(unknown_read.as_u16(), 404);
    }

    #[tokio::test]
    async fn member_access_follows_the_session_rules_too() {
        // An admin user's session reaches every catalog and may set member
        // access; a plain user's session is below admin everywhere.
        let test = app().await;
        let root = every_catalog_admin(&test).await;
        let (status, body) = send(
            &test,
            "POST",
            "/api/v1/users",
            Some(&root),
            Some(json!({"name": "alice"})),
        )
        .await;
        assert_eq!(status.as_u16(), 201, "{body}");
        let mut plain = Browser::session_for(&test, "alice").await;
        let (read_plain, _) = plain
            .send(
                &test,
                "GET",
                "/api/v1/catalogs/main/member-access",
                Payload::Empty,
            )
            .await
            .parts();
        let (write_plain, _) = plain
            .send(
                &test,
                "PUT",
                "/api/v1/catalogs/main/member-access",
                Payload::Json(json!({"access": "none"})),
            )
            .await
            .parts();
        crate::store::users::set_admin(
            test.app.pool(),
            "alice",
            AdminChange {
                on: true,
                allow_last: false,
            },
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .expect("make alice an admin");
        let mut admin = Browser::session_for(&test, "alice").await;
        let (write_admin, answer) = admin
            .send(
                &test,
                "PUT",
                "/api/v1/catalogs/main/member-access",
                Payload::Json(json!({"access": "none"})),
            )
            .await
            .parts();

        assert_eq!(
            read_plain.as_u16(),
            404,
            "a plain session that does not reach the catalog"
        );
        assert_eq!(write_plain.as_u16(), 403);
        assert_eq!(write_admin.as_u16(), 200, "{answer}");
        assert_eq!(answer["member_access"], "none");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_catalog_routes_follow_the_access_matrix() {
        let test = app().await;
        // The delete runs last: it removes `main`. Every key in the matrix
        // reaches one catalog only, so none may create one or change an
        // owner (all 403).
        assert_matrix(
            &test,
            &[
                Row {
                    method: "GET",
                    uri: "/api/v1/catalogs".into(),
                    body: None,
                    read: 200,
                    write: 200,
                    admin: 200,
                    stranger: 200,
                },
                Row {
                    method: "GET",
                    uri: "/api/v1/catalogs/main/export".into(),
                    body: None,
                    read: 200,
                    write: 200,
                    admin: 200,
                    stranger: 404,
                },
                // `{}` is not gzip: an admin gets past the key check to 400.
                Row {
                    method: "POST",
                    uri: "/api/v1/catalogs/main/import".into(),
                    body: Some(json!({})),
                    read: 403,
                    write: 403,
                    admin: 400,
                    stranger: 404,
                },
                Row {
                    method: "POST",
                    uri: "/api/v1/catalogs".into(),
                    body: Some(json!({ "name": "made-in-matrix", "owner": TEST_TEAM })),
                    read: 403,
                    write: 403,
                    admin: 403,
                    stranger: 403,
                },
                Row {
                    method: "PUT",
                    uri: "/api/v1/catalogs/main/owner".into(),
                    body: Some(json!({ "team": TEST_TEAM })),
                    read: 403,
                    write: 403,
                    admin: 403,
                    stranger: 403,
                },
                Row {
                    method: "PATCH",
                    uri: "/api/v1/catalogs/main".into(),
                    body: Some(json!({ "description": "patched" })),
                    read: 403,
                    write: 403,
                    admin: 200,
                    stranger: 404,
                },
                Row {
                    method: "DELETE",
                    uri: "/api/v1/catalogs/main?force=true".into(),
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

    #[tokio::test(flavor = "multi_thread")]
    async fn the_list_shows_reachable_catalogs_with_counts_and_generation() {
        let test = app().await;
        let keys = keys(&test).await;
        let generation = catalog_rows::find(test.app.pool(), "main")
            .await
            .expect("main")
            .generation;

        let (status, body) = send(&test, "GET", "/api/v1/catalogs", Some(&keys.read), None).await;

        assert_eq!(status, StatusCode::OK);
        let main = &body["catalogs"][0];
        assert_eq!(body["catalogs"].as_array().map(Vec::len), Some(1));
        assert_eq!(main["name"], "main");
        assert_eq!(main["description"], "");
        assert_eq!(main["owner_team"], TEST_TEAM);
        assert_eq!(main["files"], 6);
        // systems/alpha.md and systems/beta.md; index, log and AGENTS.md are not concepts.
        assert_eq!(main["concepts"], 2);
        assert_eq!(main["generation"], generation);
        assert!(main["last_change"].is_string());
        assert_eq!(names(&test, &keys.stranger).await, ["other"]);
    }

    /// M13 Task 4: the listing marks a suspended catalog rather than
    /// hiding it, to every key that reaches it, and a catalog created for a
    /// suspended team is marked in the answer.
    #[tokio::test(flavor = "multi_thread")]
    async fn the_list_marks_a_suspended_catalog_and_never_hides_it() {
        use crate::mcp::testing::{suspend_catalogs, SUSPENDED_TEAM, SUSPENSION_NOTICE};
        let test = app().await;
        let keys = keys(&test).await;
        let root = every_catalog_admin(&test).await;
        suspend_catalogs(&test, &["main"]).await;
        let listed = |body: &serde_json::Value, name: &str| -> serde_json::Value {
            body["catalogs"]
                .as_array()
                .expect("a list")
                .iter()
                .find(|catalog| catalog["name"] == name)
                .cloned()
                .unwrap_or_else(|| panic!("{name} in {body}"))
        };
        let marked = json!({ "team": SUSPENDED_TEAM, "notice": SUSPENSION_NOTICE });

        let (_, readers) = send(&test, "GET", "/api/v1/catalogs", Some(&keys.read), None).await;
        let (_, roots) = send(&test, "GET", "/api/v1/catalogs", Some(&root), None).await;
        let (_, strangers) =
            send(&test, "GET", "/api/v1/catalogs", Some(&keys.stranger), None).await;
        let (created, in_suspended) = send(
            &test,
            "POST",
            "/api/v1/catalogs",
            Some(&root),
            Some(json!({ "name": "paused", "owner": SUSPENDED_TEAM })),
        )
        .await;
        let (_, in_active) = send(
            &test,
            "POST",
            "/api/v1/catalogs",
            Some(&root),
            Some(json!({ "name": "active", "owner": TEST_TEAM })),
        )
        .await;

        assert_eq!(listed(&readers, "main")["suspended"], marked);
        assert_eq!(
            listed(&roots, "main")["suspended"],
            marked,
            "an admin sees it too"
        );
        // `get`, not `[]`: a missing key would read as null too.
        assert_eq!(
            listed(&roots, "other").get("suspended"),
            Some(&serde_json::Value::Null)
        );
        assert_eq!(names(&test, &keys.stranger).await, ["other"]);
        assert_eq!(
            listed(&strangers, "other").get("suspended"),
            Some(&serde_json::Value::Null),
            "a key that does not reach main never learns it is suspended"
        );
        assert_eq!(created, StatusCode::CREATED, "{in_suspended}");
        assert_eq!(in_suspended["suspended"]["team"], SUSPENDED_TEAM);
        assert_eq!(in_active.get("suspended"), Some(&serde_json::Value::Null));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_every_catalog_admin_creates_a_catalog_that_is_served_at_once() {
        let test = app().await;
        let keys = keys(&test).await;
        let root = every_catalog_admin(&test).await;
        let new = json!({ "name": "scratch", "description": "Scratch space", "owner": TEST_TEAM });

        let (status, created) = send(
            &test,
            "POST",
            "/api/v1/catalogs",
            Some(&root),
            Some(new.clone()),
        )
        .await;
        let (again, taken) = send(&test, "POST", "/api/v1/catalogs", Some(&root), Some(new)).await;
        let (invalid, _) = send(
            &test,
            "POST",
            "/api/v1/catalogs",
            Some(&root),
            Some(json!({ "name": "Not Valid", "owner": TEST_TEAM })),
        )
        .await;
        let (unowned, _) = send(
            &test,
            "POST",
            "/api/v1/catalogs",
            Some(&root),
            Some(json!({ "name": "unowned" })),
        )
        .await;
        let (unknown_owner, nobody) = send(
            &test,
            "POST",
            "/api/v1/catalogs",
            Some(&root),
            Some(json!({ "name": "orphan", "owner": "nobody" })),
        )
        .await;
        let (scoped, refused) = send(
            &test,
            "POST",
            "/api/v1/catalogs",
            Some(&keys.admin),
            Some(json!({ "name": "mine", "owner": TEST_TEAM })),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["name"], "scratch");
        assert_eq!(created["description"], "Scratch space");
        assert_eq!(created["owner_team"], TEST_TEAM);
        assert_eq!(created["files"], 0);
        // Polled at once: loaded, and listed for a key that reaches it.
        assert!(test.app.catalog("scratch").is_some());
        assert_eq!(names(&test, &root).await, ["main", "other", "scratch"]);
        assert_eq!(names(&test, &keys.read).await, ["main"]);
        assert_eq!(again, StatusCode::CONFLICT);
        assert!(taken["message"]
            .as_str()
            .unwrap()
            .contains("already exists"));
        assert_eq!(invalid, StatusCode::CONFLICT);
        // Every new catalog has an owner (Decision 2: a missing field is 422).
        assert_eq!(unowned, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(unknown_owner, StatusCode::NOT_FOUND, "{nobody}");
        assert_eq!(names(&test, &root).await, ["main", "other", "scratch"]);
        assert_eq!(scoped, StatusCode::FORBIDDEN);
        assert!(
            refused["message"]
                .as_str()
                .unwrap()
                .contains("every catalog"),
            "{refused}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_admin_changes_a_description_and_the_list_shows_it() {
        let test = app().await;
        let keys = keys(&test).await;

        let (status, changed) = send(
            &test,
            "PATCH",
            "/api/v1/catalogs/main",
            Some(&keys.admin),
            Some(json!({ "description": "The main catalog" })),
        )
        .await;
        let (_, listed) = send(&test, "GET", "/api/v1/catalogs", Some(&keys.read), None).await;

        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            changed,
            json!({ "name": "main", "previous": "",
                                    "description": "The main catalog" })
        );
        assert_eq!(listed["catalogs"][0]["description"], "The main catalog");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_refuses_unpublished_work_unless_forced() {
        let test = app().await;
        let keys = keys(&test).await;
        let ctx = writer(&test, "drafter", "main").await;
        let handle = open(&ctx).await;
        let edit = PendingEdit {
            path: "systems/gamma.md",
            text: Some("# Gamma\n"),
            at: Utc::now(),
        };
        pending_entry(&test, &handle, &edit).await;

        let (refused, body) = send(
            &test,
            "DELETE",
            "/api/v1/catalogs/main",
            Some(&keys.admin),
            None,
        )
        .await;
        let still_there = test.app.catalog("main").is_some();
        let (forced, deleted) = send(
            &test,
            "DELETE",
            "/api/v1/catalogs/main?force=true",
            Some(&keys.admin),
            None,
        )
        .await;

        assert_eq!(refused, StatusCode::CONFLICT);
        assert!(
            body["message"]
                .as_str()
                .unwrap()
                .contains("add ?force=true"),
            "{body}"
        );
        assert!(still_there);
        assert_eq!(forced, StatusCode::OK);
        assert_eq!(deleted, json!({ "name": "main" }));
        assert!(test.app.catalog("main").is_none());
        assert!(names(&test, &keys.read).await.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn export_and_import_under_api_v1_share_the_archive_routes() {
        let test = app().await;
        let keys = keys(&test).await;

        let (status, headers, bytes) = raw(
            &test,
            "GET",
            "/api/v1/catalogs/main/export",
            &keys.read,
            Vec::new(),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["content-type"], "application/gzip");
        assert_eq!(headers["cache-control"], "no-store");
        assert_eq!(bytes[..2], [0x1f, 0x8b], "gzip magic");

        // Machinery only: AGENTS.md changes, the concepts stay.
        let machinery = tarball_of(&[("AGENTS.md", "# Agents, new\n")]);
        let (status, _, body) = raw(
            &test,
            "POST",
            "/api/v1/catalogs/main/import?replace=true&only_machinery=true",
            &keys.admin,
            machinery,
        )
        .await;
        let report: serde_json::Value = serde_json::from_slice(&body).expect("a report");
        assert_eq!(status, StatusCode::OK, "{report}");
        assert_eq!(report["updated"], json!(["AGENTS.md"]));
        assert_eq!(report["deleted"], json!([]));

        let generation = catalog_rows::find(test.app.pool(), "main")
            .await
            .expect("main")
            .generation;
        let archive = || tarball_of(&[("index.md", "# Main\n\nImported.\n")]);
        let stale = format!(
            "/api/v1/catalogs/main/import?replace=true&expect_generation={}",
            generation + 1
        );
        let (status, _, body) = raw(&test, "POST", &stale, &keys.admin, archive()).await;
        let refused: serde_json::Value = serde_json::from_slice(&body).expect("an error");
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(
            refused["message"]
                .as_str()
                .unwrap()
                .contains("export it again"),
            "{refused}"
        );

        let current =
            format!("/api/v1/catalogs/main/import?replace=true&expect_generation={generation}");
        let (status, _, body) = raw(&test, "POST", &current, &keys.admin, archive()).await;
        let report: serde_json::Value = serde_json::from_slice(&body).expect("a report");
        assert_eq!(status, StatusCode::OK, "{report}");
        assert_eq!(report["updated"], json!(["index.md"]));
        // The snapshot was swapped in before the answer.
        let (_, listed) = send(&test, "GET", "/api/v1/catalogs", Some(&keys.read), None).await;
        assert_eq!(listed["catalogs"][0]["generation"], generation + 1);
        assert_eq!(listed["catalogs"][0]["files"], 1);
    }

    /// M13 Task 3: the per-catalog admin routes (update, member access,
    /// delete) check `require(Admin, ..)` themselves. Below admin they still answer `Forbidden` (the level
    /// check comes before suspension), and an admin, who is exempt, still
    /// manages a suspended catalog.
    #[tokio::test]
    async fn catalog_admin_routes_stay_open_to_an_admin_while_suspended() {
        use crate::http::api::testing::{assert_answers, Expect};
        use crate::mcp::testing::suspend_catalogs;
        let test = app().await;
        let keys = keys(&test).await;
        suspend_catalogs(&test, &["main"]).await;
        let described = || Some(json!({"description": "Paused."}));
        let catalog = "/api/v1/catalogs/main";
        let member_access = "/api/v1/catalogs/main/member-access";
        let delete = "/api/v1/catalogs/main?force=true";

        assert_answers(
            &test,
            &[
                Expect {
                    method: "PATCH",
                    uri: catalog,
                    body: described(),
                    key: &keys.write,
                    who: "write",
                    status: 403,
                    error: Some("Forbidden"),
                },
                Expect {
                    method: "PATCH",
                    uri: catalog,
                    body: described(),
                    key: &keys.admin,
                    who: "admin",
                    status: 200,
                    error: None,
                },
                Expect {
                    method: "GET",
                    uri: member_access,
                    body: None,
                    key: &keys.read,
                    who: "read",
                    status: 403,
                    error: Some("Forbidden"),
                },
                Expect {
                    method: "GET",
                    uri: member_access,
                    body: None,
                    key: &keys.admin,
                    who: "admin",
                    status: 200,
                    error: None,
                },
                // Last: the admin's delete removes `main`.
                Expect {
                    method: "DELETE",
                    uri: delete,
                    body: None,
                    key: &keys.write,
                    who: "write",
                    status: 403,
                    error: Some("Forbidden"),
                },
                Expect {
                    method: "DELETE",
                    uri: delete,
                    body: None,
                    key: &keys.admin,
                    who: "admin",
                    status: 200,
                    error: None,
                },
            ],
        )
        .await;
    }
}

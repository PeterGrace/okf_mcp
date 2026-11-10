//! `/api/v1/catalogs` (web console design, "API": Catalogs, Import /
//! export): the catalogs a key reaches, their administration as
//! `okf-mcp catalog create|update|delete` does it, and the archive routes
//! mounted again under `/api/v1` with the same handlers, body cap and
//! deadline as `/catalogs/{name}/export|import`.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, patch};
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};

use super::require_admin;
use crate::app::App;
use crate::auth::{Principal, Reach};
use crate::catalog::CatalogState;
use crate::clock::format_timestamp;
use crate::error::OkfError;
use crate::http::{archives, AppError};
use crate::store::catalogs as catalog_rows;
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
    /// How many files it holds (every stored file).
    pub files: usize,
    /// How many of them are concepts.
    pub concepts: usize,
    /// The generation of the loaded snapshot: what an import's
    /// `expect_generation` compares against.
    pub generation: i64,
    /// Its newest change (`2026-11-02T21:00:00Z`), or null when empty.
    pub last_change: Option<String>,
}

impl CatalogSummary {
    /// The summary of one loaded catalog.
    ///
    /// # Arguments
    /// * `name` - its name.
    /// * `state` - its state.
    fn of(name: &str, state: &CatalogState) -> Self {
        // `current()` is an `Arc` of the loaded snapshot: one consistent
        // generation, file count and concept count.
        let loaded = state.current();
        Self {
            name: name.to_string(),
            description: state.description().to_string(),
            files: loaded.files(),
            concepts: loaded.snapshot().concepts().count(),
            generation: loaded.generation(),
            last_change: loaded.last_change().map(format_timestamp),
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
        .map(|(name, state)| CatalogSummary::of(name, state))
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
/// 403 for any other key; 409 ([`OkfError::Refused`]) for an invalid or
/// taken name; 500 for a database failure.
async fn create_catalog(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    // `Json` reads the body, so it must be the last extractor.
    Json(new): Json<NewCatalog>,
) -> Result<(StatusCode, Json<CatalogSummary>), AppError> {
    require_every_catalog(&principal)?;
    let row =
        catalog_rows::create(app.pool(), &new.name, &new.description, app.clock().now()).await?;
    // Catalog rows record no actor (nor does the CLI); the log line does.
    tracing::info!(catalog = %row.name, key = %principal.key_id, actor = %principal.actor,
                   "catalog created");
    settle(&app).await;
    // Built from the row, not the loaded state: a new catalog is empty, and
    // the answer must not depend on the poll having loaded it.
    let summary = CatalogSummary {
        name: row.name,
        description: row.description,
        files: 0,
        concepts: 0,
        generation: row.generation,
        last_change: None,
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

/// Refuses all but an admin key that reaches every catalog.
///
/// # Errors
/// [`OkfError::Forbidden`] below `admin`, or for a key granted named catalogs.
fn require_every_catalog(principal: &Principal) -> Result<(), OkfError> {
    require_admin(principal)?;
    match &principal.reach {
        Reach::All => Ok(()),
        Reach::Catalogs(_) => Err(OkfError::Forbidden {
            reason: "creating a catalog needs an admin key that reaches every catalog \
                     (okf-mcp key create --all)"
                .to_string(),
        }),
    }
}

/// Polls after an administrative change, so the key cache and the catalog
/// list reflect it for the next request. The change has committed, so a
/// failed poll is logged, not returned: the poll loop retries it.
///
/// # Arguments
/// * `app` - the application. An `&Arc<App>` argument coerces to `&App`
///   automatically (deref coercion).
async fn settle(app: &App) {
    if let Err(error) = app.poll().await {
        tracing::error!(%error, "poll after a catalog change failed; the poll loop retries");
    }
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

    use crate::http::api::testing::{app, assert_matrix, keys, send, Row};
    use crate::http::archives::tarball_of;
    use crate::mcp::testing::{open, pending_entry, writer, PendingEdit, TestApp};
    use crate::store::keys::{self as key_rows, Grants, NewKey};

    /// An admin key that reaches every catalog, the only kind that may
    /// create one. It is polled in, like `testing::keys`.
    async fn every_catalog_admin(test: &TestApp) -> String {
        let new = NewKey {
            name: "root",
            access: Access::Admin,
            maintenance_report: false,
            grants: Grants::All,
            default_catalog: Some("main"),
            actor: "root",
        };
        let key = key_rows::create_with_id(test.app.pool(), &new, Utc::now())
            .await
            .expect("create the key")
            .key;
        test.app.poll().await.expect("poll the key in");
        key
    }

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

    #[tokio::test(flavor = "multi_thread")]
    async fn the_catalog_routes_follow_the_access_matrix() {
        let test = app().await;
        // The delete runs last: it removes `main`. Every key in the matrix
        // reaches one catalog only, so none may create one (all 403).
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
                    body: Some(json!({ "name": "made-in-matrix" })),
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
        assert_eq!(main["files"], 6);
        // systems/alpha.md and systems/beta.md; index, log and AGENTS.md are not concepts.
        assert_eq!(main["concepts"], 2);
        assert_eq!(main["generation"], generation);
        assert!(main["last_change"].is_string());
        assert_eq!(names(&test, &keys.stranger).await, ["other"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_every_catalog_admin_creates_a_catalog_that_is_served_at_once() {
        let test = app().await;
        let keys = keys(&test).await;
        let root = every_catalog_admin(&test).await;
        let new = json!({ "name": "scratch", "description": "Scratch space" });

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
            Some(json!({ "name": "Not Valid" })),
        )
        .await;
        let (scoped, refused) = send(
            &test,
            "POST",
            "/api/v1/catalogs",
            Some(&keys.admin),
            Some(json!({ "name": "mine" })),
        )
        .await;

        assert_eq!(status, StatusCode::CREATED, "{created}");
        assert_eq!(created["name"], "scratch");
        assert_eq!(created["description"], "Scratch space");
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
}

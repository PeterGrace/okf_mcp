//! `/api/v1/changesets` (web console design, "API": Changesets): open
//! changesets, their pending diffs, and discarding one. Thin adapters over
//! the `list_changesets`, `pending_changes` and `discard` tools, so the
//! access rules are the tools' own (M8 Decisions 3 and 8):
//! - list: the key's own changesets, an admin's every key's, in the
//!   catalogs it reaches (a read key owns none);
//! - diff: the owning key or an admin;
//! - discard: the owning write key or an admin.
//!
//! Another key's handle is 404 (`UnknownChangeset`), never 403, so a key
//! cannot tell a real handle from a made-up one.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::{delete, get};
use axum::{Extension, Json, Router};
use serde::Deserialize;

use crate::app::App;
use crate::auth::Principal;
use crate::http::api::context;
use crate::http::AppError;
use crate::mcp::changesets::{
    discard, list_changesets, pending_changes, DiscardParams, Discarded, ListParams, Listed,
    PendingChanges, PendingParams,
};

/// `GET /changesets`' query.
#[derive(Debug, Deserialize)]
struct ListQuery {
    /// Only this catalog (absent or empty: every catalog the key reaches).
    catalog: Option<String>,
}

/// This area's routes.
///
/// # Returns
/// `GET /changesets`, `GET /changesets/{handle}/diff` and
/// `DELETE /changesets/{handle}`.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/changesets", get(list))
        .route("/changesets/{handle}", delete(remove))
        .route("/changesets/{handle}/diff", get(diff))
}

/// `GET /changesets?catalog=`: as the `list_changesets` tool.
///
/// # Errors
/// 404 for a catalog the key does not reach; 500 if the store fails.
async fn list(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Query(query): Query<ListQuery>,
) -> Result<Json<Listed>, AppError> {
    let ctx = context(&app, principal);
    // `filter` turns `?catalog=` (an empty select) into "every catalog".
    let catalog = query.catalog.filter(|name| !name.is_empty());
    Ok(Json(list_changesets(&ctx, ListParams { catalog }).await?))
}

/// `GET /changesets/{handle}/diff`: as the `pending_changes` tool.
///
/// # Errors
/// 404 for a handle the key may not use; 409 for a published or discarded one.
async fn diff(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(handle): Path<String>,
) -> Result<Json<PendingChanges>, AppError> {
    let ctx = context(&app, principal);
    let params = PendingParams { changeset: handle };
    Ok(Json(pending_changes(&ctx, params).await?))
}

/// `DELETE /changesets/{handle}`: drops every entry and closes the
/// changeset for good, as `discard` without `paths`.
///
/// # Errors
/// 404 for a handle the key may not use; 403 for an owner below write
/// (none exists: opening needs write); 409 if it was published or
/// discarded meanwhile.
async fn remove(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(handle): Path<String>,
) -> Result<Json<Discarded>, AppError> {
    let ctx = context(&app, principal);
    let params = DiscardParams {
        paths: None,
        changeset: handle,
    };
    Ok(Json(discard(&ctx, params).await?))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::json;

    use crate::http::api::context;
    use crate::http::api::testing::{app, assert_matrix_with, keys, send, Row};
    use crate::mcp::changesets::{open_changeset, OpenParams};
    use crate::mcp::testing::{pending_entry, PendingEdit, TestApp};

    /// Opens a changeset in `main` as the key `secret` and stores one new
    /// file in it; returns the handle.
    async fn open_as(test: &TestApp, secret: &str, path: &str) -> String {
        let principal = test.app.keys().find(secret).expect("a live key");
        let ctx = context(&test.app, principal);
        let handle = open_changeset(
            &ctx,
            OpenParams {
                catalog: Some("main".to_string()),
            },
        )
        .await
        .expect("open a changeset")
        .changeset;
        let edit = PendingEdit {
            path,
            text: Some("# Gamma\n\nDraft.\n"),
            at: Utc::now(),
        };
        pending_entry(test, &handle, &edit).await;
        handle
    }

    /// The handles of a `GET /changesets` body, in its order.
    fn handles(listed: &serde_json::Value) -> Vec<String> {
        listed["changesets"]
            .as_array()
            .expect("a list")
            .iter()
            .filter_map(|row| row["changeset"].as_str().map(str::to_string))
            .collect()
    }

    /// A matrix row; the body is always empty here.
    fn row(method: &'static str, uri: String, statuses: [u16; 4]) -> Row {
        let [read, write, admin, stranger] = statuses;
        Row {
            method,
            uri,
            body: None,
            read,
            write,
            admin,
            stranger,
        }
    }

    #[tokio::test]
    async fn changeset_routes_follow_the_tools_access_rules() {
        let test = app().await;
        let keys = keys(&test).await;
        let writers = open_as(&test, &keys.write, "systems/gamma.md").await;
        let admins = open_as(&test, &keys.admin, "systems/delta.md").await;
        assert_matrix_with(
            &test,
            &keys,
            &[
                row("GET", "/api/v1/changesets".into(), [200, 200, 200, 200]),
                row(
                    "GET",
                    "/api/v1/changesets?catalog=main".into(),
                    [200, 200, 200, 404],
                ),
                row(
                    "GET",
                    format!("/api/v1/changesets/{writers}/diff"),
                    [404, 200, 200, 404],
                ),
                row(
                    "DELETE",
                    format!("/api/v1/changesets/{admins}"),
                    [404, 404, 200, 404],
                ),
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn a_key_lists_its_own_changesets_and_an_admin_every_keys() {
        let test = app().await;
        let keys = keys(&test).await;
        let writers = open_as(&test, &keys.write, "systems/gamma.md").await;
        let admins = open_as(&test, &keys.admin, "systems/delta.md").await;
        let uri = "/api/v1/changesets?catalog=main";

        let (_, as_writer) = send(&test, "GET", uri, Some(&keys.write), None).await;
        assert_eq!(handles(&as_writer), vec![writers.clone()]);
        assert_eq!(as_writer["changesets"][0]["key_name"], "writer");
        assert_eq!(
            as_writer["changesets"][0]["paths"],
            json!(["/systems/gamma.md"])
        );
        assert_eq!(as_writer["changesets"][0]["idle"], false);

        let (_, as_admin) = send(&test, "GET", uri, Some(&keys.admin), None).await;
        let mut all = handles(&as_admin);
        all.sort();
        let mut want = vec![writers, admins];
        want.sort();
        assert_eq!(all, want);

        let (_, as_reader) = send(&test, "GET", uri, Some(&keys.read), None).await;
        assert!(handles(&as_reader).is_empty());
    }

    #[tokio::test]
    async fn the_diff_shows_each_pending_file() {
        let test = app().await;
        let keys = keys(&test).await;
        let handle = open_as(&test, &keys.write, "systems/gamma.md").await;
        let uri = format!("/api/v1/changesets/{handle}/diff");
        let (status, body) = send(&test, "GET", &uri, Some(&keys.write), None).await;
        assert_eq!(status.as_u16(), 200, "{body}");
        assert_eq!(body["catalog"], "main");
        assert_eq!(body["files"][0]["path"], "/systems/gamma.md");
        assert_eq!(body["files"][0]["change"], "create");
        let text = body["files"][0]["diff"].as_str().expect("a diff");
        assert!(text.contains("+# Gamma"), "{text}");
        let unknown = "/api/v1/changesets/no-such-handle/diff";
        let (status, _) = send(&test, "GET", unknown, Some(&keys.admin), None).await;
        assert_eq!(status.as_u16(), 404);
    }

    #[tokio::test]
    async fn the_owner_discards_its_changeset_for_good() {
        let test = app().await;
        let keys = keys(&test).await;
        let handle = open_as(&test, &keys.write, "systems/gamma.md").await;
        let uri = format!("/api/v1/changesets/{handle}");
        let (status, body) = send(&test, "DELETE", &uri, Some(&keys.write), None).await;
        assert_eq!(status.as_u16(), 200, "{body}");
        assert_eq!(body["removed"], 1);
        assert_eq!(body["closed"], true);
        let (_, listed) = send(&test, "GET", "/api/v1/changesets", Some(&keys.write), None).await;
        assert!(handles(&listed).is_empty());
        // The handle is closed: its diff and a second discard are refused.
        let diff = format!("{uri}/diff");
        let (status, _) = send(&test, "GET", &diff, Some(&keys.write), None).await;
        assert_eq!(status.as_u16(), 409);
        let (status, _) = send(&test, "DELETE", &uri, Some(&keys.write), None).await;
        assert_eq!(status.as_u16(), 409);
    }
}

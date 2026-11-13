//! `GET /api/v1/search` (web console design, "API": Search): the `search`
//! tool over the catalogs a request names, or every catalog the key reaches.

use std::sync::Arc;

use axum::extract::{Query, State};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::Deserialize;

use crate::app::App;
use crate::auth::Principal;
use crate::http::api::context;
use crate::http::AppError;
use crate::mcp::search::{run, SearchParams, SearchResult};

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new().route("/search", get(search))
}

/// `?q=&catalogs=&limit=`.
#[derive(Debug, Clone, Default, Deserialize)]
struct SearchArgs {
    /// Free text, as the tool's `query`; absent means every concept.
    #[serde(default)]
    q: String,
    /// Comma-separated catalog names; absent or empty means every catalog
    /// the key reaches.
    catalogs: Option<String>,
    /// At most this many hits (as the tool: default 20, at most 200).
    limit: Option<usize>,
}

/// The names in a `catalogs=` value.
///
/// # Arguments
/// * `listed` - the raw value, such as `main, other`.
///
/// # Returns
/// The names, trimmed, empty ones dropped, in order.
fn catalog_names(listed: &str) -> Vec<String> {
    listed
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .map(str::to_string)
        .collect()
}

/// `GET /search`.
///
/// Unlike the MCP tool, whose default is the connection's catalogs (else
/// the key's default catalog), the console has no connection: without
/// `catalogs` it searches every loaded catalog the key reaches, leaving out
/// those whose owner team is suspended (unless the key is admin).
///
/// # Errors
/// As `search`: 404 for a named catalog the key does not reach or that does
/// not exist; 403 `Suspended` for a named catalog whose team is suspended.
async fn search(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Query(args): Query<SearchArgs>,
) -> Result<Json<SearchResult>, AppError> {
    let named = args
        .catalogs
        .as_deref()
        .map(catalog_names)
        .unwrap_or_default();
    let catalogs = if named.is_empty() {
        // `Catalogs` is a BTreeMap, so the names come out sorted. An empty
        // list (a key that reaches nothing active) falls back to the key's
        // default inside the selection, which then refuses as the tool would.
        // A suspended team's catalogs are not searched (M13), and reach is
        // checked first; naming one explicitly is refused by the selection.
        app.catalogs()
            .keys()
            .filter(|name| principal.reaches(name) && principal.check_not_suspended(name).is_ok())
            .cloned()
            .collect()
    } else {
        named
    };
    let ctx = context(&app, principal);
    let result = run(
        &ctx,
        SearchParams {
            query: args.q,
            catalogs: Some(catalogs),
            concept_type: None,
            tags: Vec::new(),
            status: None,
            limit: args.limit,
            changeset: None,
        },
    )
    .await?;
    Ok(Json(result))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::testing::{app, assert_matrix, keys, send, Row};

    #[test]
    fn catalog_names_are_trimmed_and_empty_ones_dropped() {
        assert_eq!(catalog_names(" main, other,,"), ["main", "other"]);
        assert!(catalog_names("").is_empty());
    }

    #[tokio::test]
    async fn search_needs_read_on_every_named_catalog() {
        let test = app().await;
        let row = |uri: &str, stranger: u16| Row {
            method: "GET",
            uri: uri.to_string(),
            body: None,
            read: 200,
            write: 200,
            admin: 200,
            stranger,
        };

        assert_matrix(
            &test,
            &[
                row("/api/v1/search?q=alpha&catalogs=main", 404),
                // Without `catalogs`, each key searches what it reaches.
                row("/api/v1/search?q=alpha", 200),
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn search_finds_a_concept_with_its_catalog_and_snippet() {
        let test = app().await;
        let keys = keys(&test).await;

        let (status, found) = send(
            &test,
            "GET",
            "/api/v1/search?q=runs%20beta",
            Some(&keys.read),
            None,
        )
        .await;
        let (_, limited) = send(
            &test,
            "GET",
            "/api/v1/search?q=&limit=1",
            Some(&keys.read),
            None,
        )
        .await;
        let (_, elsewhere) = send(
            &test,
            "GET",
            "/api/v1/search?q=alpha",
            Some(&keys.stranger),
            None,
        )
        .await;

        assert_eq!(status.as_u16(), 200, "{found}");
        assert_eq!(found["hits"][0]["catalog"], "main");
        assert_eq!(found["hits"][0]["path"], "/systems/alpha.md");
        assert_eq!(found["hits"][0]["snippet"], "Alpha runs [Beta](beta.md).");
        assert_eq!(limited["hits"].as_array().map(Vec::len), Some(1));
        assert_eq!(
            elsewhere["hits"],
            serde_json::json!([]),
            "other has no concepts"
        );
    }

    /// M13 Task 4: searching every reached catalog leaves a suspended
    /// team's catalogs out (an admin's search keeps them); naming one is a
    /// 403 `Suspended`, and a key that does not reach it gets 404 still.
    #[tokio::test]
    async fn search_leaves_out_or_refuses_a_suspended_catalog() {
        use crate::config::Config;
        use crate::http::api::testing::{assert_answers, Expect};
        use crate::mcp::testing::{app_with, stored_secret, suspend_catalogs};
        use crate::store::keys::Access;
        const ALPHA: &str = "---\ntype: system\ntitle: Alpha\n---\n# Alpha\n";
        let test = app_with(
            &[
                ("main", &[("alpha.md", ALPHA)]),
                ("other", &[("alpha.md", ALPHA)]),
            ],
            Config::default(),
        )
        .await;
        let member = stored_secret(&test, "member", Access::Read, &["main", "other"], false).await;
        let only_main = stored_secret(&test, "only-main", Access::Read, &["main"], false).await;
        let stranger = stored_secret(&test, "stranger", Access::Read, &["other"], false).await;
        let root = stored_secret(&test, "root", Access::Admin, &[], false).await;
        suspend_catalogs(&test, &["main"]).await;
        let searched = |listed: &serde_json::Value| -> Vec<String> {
            let mut catalogs: Vec<String> = listed["hits"]
                .as_array()
                .expect("hits")
                .iter()
                .filter_map(|hit| hit["catalog"].as_str().map(str::to_string))
                .collect();
            catalogs.dedup();
            catalogs
        };

        let (status, members) =
            send(&test, "GET", "/api/v1/search?q=alpha", Some(&member), None).await;
        let (_, admins) = send(&test, "GET", "/api/v1/search?q=alpha", Some(&root), None).await;
        let named = "/api/v1/search?q=alpha&catalogs=main";
        let mixed = "/api/v1/search?q=alpha&catalogs=other,main";
        let every = "/api/v1/search?q=alpha";
        let expect = |uri, key, who, status, error| Expect {
            method: "GET",
            uri,
            body: None,
            key,
            who,
            status,
            error,
        };

        assert_eq!(status.as_u16(), 200, "{members}");
        assert_eq!(
            searched(&members),
            ["other"],
            "the suspended catalog is left out"
        );
        assert_eq!(
            searched(&admins),
            ["main", "other"],
            "an admin searches both"
        );
        assert_answers(
            &test,
            &[
                expect(named, &member, "member", 403, Some("Suspended")),
                expect(mixed, &member, "member", 403, Some("Suspended")),
                expect(
                    named,
                    &stranger,
                    "stranger",
                    404,
                    Some("CatalogNotSelected"),
                ),
                expect(named, &root, "root", 200, None),
                // Every catalog it reaches is suspended: its default (`main`)
                // is then the selection, and is refused as named.
                expect(every, &only_main, "only-main", 403, Some("Suspended")),
            ],
        )
        .await;
    }
}

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
/// `catalogs` it searches every loaded catalog the key reaches.
///
/// # Errors
/// As `search`: 404 for a named catalog the key does not reach or that does
/// not exist.
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
        // list (a key that reaches nothing) falls back to the key's default
        // inside the selection, which then refuses as the tool would.
        app.catalogs()
            .keys()
            .filter(|name| principal.reaches(name))
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
}

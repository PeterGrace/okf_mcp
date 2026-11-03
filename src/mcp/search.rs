//! `search`: concepts across the selected catalogs.

use std::sync::Arc;

use axum::http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, Json};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::catalog::{CatalogState, Loaded};
use crate::error::OkfError;
use crate::mcp::context::ToolContext;
use crate::mcp::{blocking, reply, OkfServer};
use crate::okf::search::{search_catalogs, CatalogHit, SearchQuery};
use crate::okf::snapshot::Snapshot;

/// The design's default `limit`.
const DEFAULT_LIMIT: usize = 20;

/// `search`'s arguments. Doc comments are the parameters' descriptions in
/// the tool's input schema (`JsonSchema` reads them).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct SearchParams {
    /// Free text. Every whitespace-separated term must occur
    /// (case-insensitive substring) in a concept's title or aliases, tags,
    /// description or body; free text matches tags by substring. Empty
    /// matches every concept that passes the filters.
    pub query: String,
    /// The catalogs to search (default: this connection's catalogs).
    pub catalogs: Option<Vec<String>>,
    /// Only concepts of this `type` (case-insensitive).
    // `type` is a Rust keyword, so the field has another name and serde
    // renames it in the JSON.
    #[serde(rename = "type")]
    pub concept_type: Option<String>,
    /// Only concepts carrying every one of these tags.
    /// These match whole tags only (case-insensitive): `infra` does not
    /// match `infrastructure`, while the same text in `query` would, by
    /// substring.
    // `default` lets a caller leave the argument out (an empty list).
    #[serde(default)]
    pub tags: Vec<String>,
    /// Only concepts with this `status` (case-insensitive; a missing status
    /// counts as `stable`).
    pub status: Option<String>,
    /// At most this many hits (default 20).
    pub limit: Option<usize>,
}

/// `search`'s result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SearchResult {
    /// Best first: concepts holding the whole query in a better field
    /// first, deprecated concepts last, then catalog order, then path.
    pub hits: Vec<CatalogHit>,
}

/// Runs a search.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the arguments.
///
/// # Returns
/// The merged hits.
///
/// # Errors
/// A selection error ([`ToolContext::states`]).
pub(crate) async fn run(ctx: &ToolContext, params: SearchParams) -> Result<SearchResult, OkfError> {
    let states = ctx.states(params.catalogs.as_deref())?;
    let query = SearchQuery {
        text: params.query,
        concept_type: params.concept_type,
        tags: params.tags,
        status: params.status,
        limit: params.limit.unwrap_or(DEFAULT_LIMIT),
    };
    // Each catalog's current snapshot, taken once here. The pairs are `Arc`
    // handles (reference-counted pointers, cheap to copy), so the blocking
    // thread can own them; nothing is cloned but the counts.
    let loaded: Vec<(Arc<CatalogState>, Arc<Loaded>)> = states
        .into_iter()
        .map(|state| {
            let current = state.current();
            (state, current)
        })
        .collect();
    // `move` hands `loaded` and `query` to the closure, which runs on
    // another thread; the borrows below live only inside it.
    let hits = blocking(move || {
        let catalogs: Vec<(&str, &Snapshot)> = loaded
            .iter()
            .map(|(state, current)| (state.name(), current.snapshot()))
            .collect();
        search_catalogs(&catalogs, &query)
    })
    .await;
    Ok(SearchResult { hits })
}

// As in `catalogs.rs`: the macro turns this block's `#[tool]` methods into a
// router named `search_router`, joined in `OkfServer::tools`.
#[tool_router(router = search_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `search` tool.
    // `Parameters(params)` destructures the JSON arguments, which rmcp has
    // already deserialized into `SearchParams`. The return type is spelled
    // out (not `ToolResult<...>`) so the macro sees `Json<...>` and builds
    // the output schema.
    #[tool(
        description = "Search concepts across this connection's catalogs (or `catalogs`). \
                       Every term of `query` must occur, as a case-insensitive substring, in \
                       a concept's title or aliases, tags, description or body; free text \
                       matches tags by substring. The `tags` filter matches whole tags only. \
                       Returns catalog, path, title, description and a snippet, best first."
    )]
    async fn search(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<SearchParams>,
    ) -> Result<Json<SearchResult>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(run(&ctx, params).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::mcp::testing::{app_with, context, principal};

    const TAGGED: &str =
        "---\ntype: Service\ntitle: Router\ntags: [infrastructure]\n---\nRoutes.\n";

    fn params(query: &str, tags: &[&str]) -> SearchParams {
        SearchParams {
            query: query.to_string(),
            catalogs: None,
            concept_type: None,
            tags: tags.iter().map(|tag| tag.to_string()).collect(),
            status: None,
            limit: None,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tags_match_whole_tags_while_free_text_matches_substrings() {
        let test = app_with(&[("pg", &[("router.md", TAGGED)])], Config::default()).await;
        let ctx = context(&test, principal(&[], Some("pg")), None);

        let by_tag = run(&ctx, params("", &["infra"])).await.unwrap();
        let by_whole_tag = run(&ctx, params("", &["infrastructure"])).await.unwrap();
        let by_text = run(&ctx, params("infra", &[])).await.unwrap();

        assert!(by_tag.hits.is_empty());
        assert_eq!(by_whole_tag.hits.len(), 1);
        assert_eq!(by_text.hits.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn search_spans_the_selected_catalogs_and_names_each_hit_s_catalog() {
        let test = app_with(
            &[
                ("pg", &[("router.md", TAGGED)]),
                ("work", &[("switch.md", TAGGED)]),
            ],
            Config::default(),
        )
        .await;
        let ctx = context(&test, principal(&[], None), Some(&["work", "pg"]));

        let found = run(&ctx, params("routes", &[])).await.unwrap();

        let catalogs: Vec<&str> = found.hits.iter().map(|hit| hit.catalog.as_str()).collect();
        assert_eq!(catalogs, ["work", "pg"]);
    }

    #[test]
    fn the_schema_says_tags_match_whole_tags_only() {
        let tools = OkfServer::search_router().list_all();
        let tool = tools.iter().find(|tool| tool.name == "search").unwrap();

        let description = tool.description.as_deref().unwrap_or_default();
        let properties = &tool.input_schema["properties"];
        let tags = properties["tags"]["description"].as_str().unwrap();
        let query = properties["query"]["description"].as_str().unwrap();

        assert!(description.contains("whole tags only"), "{description}");
        assert!(description.contains("tags by substring"), "{description}");
        // A doc comment's line breaks stay in the description, so the
        // phrase must sit on one line of the comment to match here.
        assert!(tags.contains("whole tags only"), "{tags}");
        assert!(query.contains("tags by substring"), "{query}");
        let output = tool.output_schema.as_ref().expect("an output schema");
        assert_eq!(output["type"], "object");
    }
}

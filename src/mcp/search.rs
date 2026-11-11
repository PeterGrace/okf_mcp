//! `search`: concepts across the selected catalogs.

use std::sync::Arc;

use axum::http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, Json};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::catalog::CatalogState;
use crate::error::OkfError;
use crate::mcp::context::{is_broken_overlay, require_selected, ToolContext};
use crate::mcp::{blocking, reply, OkfServer};
use crate::okf::search::{search_catalogs, CatalogHit, SearchQuery};
use crate::okf::snapshot::Snapshot;
use crate::overlay::View;

/// The design's default `limit`.
const DEFAULT_LIMIT: usize = 20;

/// The most hits one call returns, whatever `limit` asks for, so a response
/// stays a reasonable size for an MCP client.
const MAX_LIMIT: usize = 200;

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
    /// At most this many hits (default 20, at most 200; 0 returns none).
    pub limit: Option<usize>,
    /// Read through this changeset's drafts (its handle); its catalog must be one this call reads.
    pub changeset: Option<String>,
}

/// `search`'s result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SearchResult {
    /// Best first: concepts holding the whole query in a better field
    /// first, deprecated concepts last, then catalog order, then path.
    pub hits: Vec<CatalogHit>,
    /// Catalogs left out because the `changeset` read through does not
    /// apply over their published files; every other catalog still
    /// answered. Empty when every catalog was searched.
    pub skipped: Vec<SkippedCatalog>,
}

/// A catalog `search` left out, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SkippedCatalog {
    /// The catalog.
    pub catalog: String,
    /// The error, as a failed call would report it (`error`, `message`
    /// and what it concerns).
    pub error: serde_json::Value,
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
/// A selection error ([`ToolContext::states`]); as [`ToolContext::pin`] for
/// `changeset`, and [`OkfError::Refused`] if its catalog is not searched;
/// any error from [`ToolContext::view`] (a database error, `Busy`) but a
/// broken overlay. A
/// catalog whose overlay
/// does not apply ([`is_broken_overlay`]) is not an error: it is left out
/// and listed in [`SearchResult::skipped`], so one broken changeset does not
/// fail a search across every catalog.
pub(crate) async fn run(ctx: &ToolContext, params: SearchParams) -> Result<SearchResult, OkfError> {
    let states = ctx.states(params.catalogs.as_deref())?;
    let pinned = ctx.pin(params.changeset.as_deref()).await?;
    require_selected(pinned.as_ref(), &states)?;
    let query = SearchQuery {
        text: params.query,
        concept_type: params.concept_type,
        tags: params.tags,
        status: params.status,
        limit: hit_limit(params.limit),
    };
    // Each catalog's view (published, or the pinned changeset's overlay), taken
    // once here. Both hold `Arc`s (reference-counted pointers, cheap to
    // copy), so the blocking thread can own them.
    let mut views: Vec<(Arc<CatalogState>, View)> = Vec::with_capacity(states.len());
    let mut skipped = Vec::new();
    for state in states {
        match ctx.view(&state, pinned.as_ref()).await {
            Ok(view) => views.push((state, view)),
            // `if` on a match arm is a guard: the arm applies only when it
            // holds. A broken overlay is never internal, so the kind is
            // enough for the log; the caller gets the full error.
            Err(error) if is_broken_overlay(&error) => {
                tracing::warn!(catalog = %state.name(), kind = error.kind(), "search skipped a catalog");
                skipped.push(SkippedCatalog {
                    catalog: state.name().to_string(),
                    error: error.to_json(),
                });
            }
            // Anything else (a database failure, `Busy`): the call fails (and
            // `tool_error` logs an internal error with its details), so the
            // agent retries.
            Err(error) => return Err(error),
        }
    }
    // `move` hands `views` and `query` to the closure, which runs on
    // another thread; the borrows below live only inside it.
    let hits = blocking(move || {
        let catalogs: Vec<(&str, &Snapshot)> = views
            .iter()
            .map(|(state, view)| (state.name(), view.snapshot()))
            .collect();
        search_catalogs(&catalogs, &query)
    })
    .await;
    Ok(SearchResult { hits, skipped })
}

/// The number of hits a call returns at most.
///
/// # Arguments
/// * `requested` - the `limit` argument, if given.
///
/// # Returns
/// [`DEFAULT_LIMIT`] when none was given, else `requested` capped at
/// [`MAX_LIMIT`].
fn hit_limit(requested: Option<usize>) -> usize {
    // `map_or(default, f)` is Python's `default if x is None else f(x)`.
    requested.map_or(DEFAULT_LIMIT, |limit| limit.min(MAX_LIMIT))
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
        title = "Search Concepts",
        description = "Search concepts across this connection's catalogs (or `catalogs`). \
                       Every term of `query` must occur, as a case-insensitive substring, in \
                       a concept's title or aliases, tags, description or body; free text \
                       matches tags by substring. The `tags` filter matches whole tags only. \
                       Returns catalog, path, title, description, a snippet and the \
                       concept's size (`lines` and `chars` of the whole file, as a read \
                       returns it), best first: check the size before a `get_concept`, \
                       since a large concept stays in context once read. With \
                       `changeset` (a handle from open_changeset), its catalog is searched \
                       through that changeset's drafts, and is listed in `skipped` if they \
                       cannot be applied.",
        annotations(
            title = "Search Concepts",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
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
    use chrono::Utc;

    use crate::config::Config;
    use crate::mcp::testing::{
        app_with, context, open, pending_entry, principal, principal_as, store_key, PendingEdit,
    };
    use crate::store::keys::Access;

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
            changeset: None,
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

        let mut only_pg = params("routes", &[]);
        only_pg.catalogs = Some(vec!["pg".to_string()]);

        let found = run(&ctx, params("routes", &[])).await.unwrap();
        let found_in_pg = run(&ctx, only_pg).await.unwrap();

        let names = |result: &SearchResult| -> Vec<String> {
            result.hits.iter().map(|hit| hit.catalog.clone()).collect()
        };
        assert_eq!(names(&found), ["work", "pg"]);
        // The `catalogs` argument narrows the connection's catalogs.
        assert_eq!(names(&found_in_pg), ["pg"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_catalog_whose_overlay_fails_is_skipped_and_the_others_answer() {
        let test = app_with(
            &[
                ("pg", &[("router.md", TAGGED)]),
                ("work", &[("switch.md", TAGGED)]),
            ],
            Config::default(),
        )
        .await;
        store_key(&test, "writer").await;
        let ctx = context(
            &test,
            principal_as("writer", Access::Write, &[], None),
            Some(&["pg", "work"]),
        );
        // The write target is `pg`, the first selected catalog.
        let handle = open(&ctx).await;
        // `router.md` is a published file, so a pending `router.md/x.md`
        // cannot be applied over it.
        let clash = PendingEdit {
            path: "router.md/x.md",
            text: Some("# X\n"),
            at: Utc::now(),
        };
        pending_entry(&test, &handle, &clash).await;
        let mut through = params("routes", &[]);
        through.changeset = Some(handle.clone());

        let found = run(&ctx, through).await.unwrap();

        let catalogs: Vec<&str> = found.hits.iter().map(|hit| hit.catalog.as_str()).collect();
        assert_eq!(catalogs, ["work"]);
        assert_eq!(found.skipped.len(), 1);
        assert_eq!(found.skipped[0].catalog, "pg");
        assert_eq!(found.skipped[0].error["error"], "InvalidPath");
        let message = found.skipped[0].error["message"].as_str().unwrap();
        assert!(
            message.contains(&format!("pending changeset {handle}")),
            "{message}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_database_failure_fails_the_search_instead_of_skipping() {
        let test = app_with(&[("pg", &[("router.md", TAGGED)])], Config::default()).await;
        let ctx = context(
            &test,
            principal_as("writer", Access::Write, &[], Some("pg")),
            None,
        );
        // A closed pool fails the changeset lookup, as an outage would.
        test.app.pool().close().await;
        let mut through = params("routes", &[]);
        through.changeset = Some("any-handle".to_string());

        let result = run(&ctx, through).await;

        assert_eq!(result.unwrap_err().kind(), "Database");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_changeset_outside_the_selected_catalogs_is_refused() {
        let test = app_with(
            &[
                ("pg", &[("router.md", TAGGED)]),
                ("work", &[("switch.md", TAGGED)]),
            ],
            Config::default(),
        )
        .await;
        store_key(&test, "writer").await;
        let ctx = context(
            &test,
            principal_as("writer", Access::Write, &[], Some("pg")),
            None,
        );
        let handle = open(&ctx).await;
        let mut in_work = params("routes", &[]);
        in_work.catalogs = Some(vec!["work".to_string()]);
        in_work.changeset = Some(handle.clone());

        let refused = run(&ctx, in_work).await.unwrap_err();

        assert_eq!(refused.kind(), "Refused");
        let message = refused.to_string();
        assert!(
            message.contains(&handle) && message.contains("\"pg\""),
            "{message}"
        );
    }

    #[test]
    fn the_limit_defaults_to_20_and_is_capped_at_200() {
        let limits = [None, Some(0), Some(5), Some(200), Some(10_000)].map(hit_limit);

        assert_eq!(limits, [20, 0, 5, 200, 200]);
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
        assert!(description.contains("`lines` and `chars`"), "{description}");
    }

    #[test]
    fn the_output_schema_documents_each_hit_s_size() {
        let tools = OkfServer::search_router().list_all();
        let tool = tools.iter().find(|tool| tool.name == "search").unwrap();
        let output = tool.output_schema.as_ref().expect("an output schema");
        // schemars puts `CatalogHit` (with `SearchHit` flattened in) under
        // `$defs`, and lists non-`Option` fields as required.
        let hit = &output["$defs"]["CatalogHit"];

        for field in ["lines", "chars"] {
            let property = &hit["properties"][field];
            assert_eq!(property["type"], "integer", "{field}: {property}");
            let description = property["description"].as_str().unwrap_or_default();
            assert!(description.contains("whole file"), "{field}: {description}");
            assert!(
                hit["required"].as_array().unwrap().contains(&field.into()),
                "{field}: {hit}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_hit_read_through_a_changeset_has_the_draft_s_size() {
        let test = app_with(&[("pg", &[("router.md", TAGGED)])], Config::default()).await;
        store_key(&test, "writer").await;
        let ctx = context(
            &test,
            principal_as("writer", Access::Write, &[], Some("pg")),
            None,
        );
        let handle = open(&ctx).await;
        let draft = "---\ntype: Service\ntitle: Router\n---\nRoutes.\nAnd \u{e9}dits.\nThird.\n";
        let edit = PendingEdit {
            path: "router.md",
            text: Some(draft),
            at: Utc::now(),
        };
        pending_entry(&test, &handle, &edit).await;
        let mut through = params("routes", &[]);
        through.changeset = Some(handle);

        let published = run(&ctx, params("routes", &[])).await.unwrap();
        let drafted = run(&ctx, through).await.unwrap();

        let size = |result: &SearchResult| (result.hits[0].hit.lines, result.hits[0].hit.chars);
        assert_eq!(
            size(&published),
            (TAGGED.lines().count(), TAGGED.chars().count())
        );
        assert_eq!(size(&published), (6, 67));
        // The draft has one line more than the published file, and its `\u{e9}`
        // is two bytes but one character.
        assert_eq!(size(&drafted), (7, draft.chars().count()));
        assert_eq!((draft.chars().count(), draft.len()), (62, 63));
    }
}

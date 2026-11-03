//! `history`: a document's revisions, from the first selected catalog that
//! has any (Decision 1).

use axum::http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, Json};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::clock::format_timestamp;
use crate::error::OkfError;
use crate::mcp::context::ToolContext;
use crate::mcp::{reply, OkfServer};
use crate::okf::paths::{escapes_root, normpath};
use crate::store::history::revisions;

/// The design's default `limit`.
const DEFAULT_LIMIT: u32 = 20;

/// The most revisions one call returns, whatever `limit` asks for, so a
/// response stays a reasonable size for an MCP client (as `search`'s cap).
const MAX_LIMIT: u32 = 200;

/// `history`'s arguments. Doc comments are the parameters' descriptions in
/// the tool's input schema (`JsonSchema` reads them).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct HistoryParams {
    /// The document: `/infra/time-server.md`, `infra/time-server`, `/log.md`.
    pub path: String,
    /// The catalog (default: the first selected catalog with any revision of it).
    pub catalog: Option<String>,
    /// At most this many revisions (default 20, at most 200; 0 counts as 1).
    pub limit: Option<u32>,
}

/// One revision.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Revision {
    /// The revision number, 1 for the first.
    pub revision: i32,
    /// `create`, `update`, `delete` or `import`.
    pub op: String,
    /// Who made it.
    pub actor: String,
    /// The publish message (`import` for an import).
    pub message: String,
    /// When (`2026-11-02T21:00:00Z`).
    pub at: String,
}

/// `history`'s result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct History {
    /// The catalog it came from.
    pub catalog: String,
    /// `/`-prefixed path.
    pub path: String,
    /// Newest first.
    pub revisions: Vec<Revision>,
}

/// The stored paths a reference may mean, as `get_concept` spells them: as
/// written, then with `.md`; the leading `/` is optional. Unlike
/// `get_concept`, any file counts, not only concepts.
///
/// # Arguments
/// * `reference` - the caller's path.
///
/// # Returns
/// The normalized candidates, in order, leaving out any that leave the
/// catalog.
fn candidates(reference: &str) -> Vec<String> {
    let bare = reference.trim_start_matches('/');
    [normpath(bare), normpath(&format!("{bare}.md"))]
        .into_iter()
        .filter(|path| !escapes_root(path))
        .collect()
}

/// The number of revisions a call returns at most.
///
/// # Arguments
/// * `requested` - the `limit` argument, if given.
///
/// # Returns
/// [`DEFAULT_LIMIT`] when none was given, else `requested` clamped to
/// 1..=[`MAX_LIMIT`]. Never 0: with 0 every path would look as if it had no
/// history.
fn revision_limit(requested: Option<u32>) -> i64 {
    // `map_or(default, f)` is Python's `default if x is None else f(x)`;
    // `clamp(lo, hi)` is `min(max(x, lo), hi)`.
    i64::from(requested.map_or(DEFAULT_LIMIT, |limit| limit.clamp(1, MAX_LIMIT)))
}

/// A document's revisions.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the arguments.
///
/// # Returns
/// The revisions from the first catalog of the read set that has any, newest
/// first.
///
/// # Errors
/// A selection error ([`ToolContext::read_set`]); [`OkfError::NotFound`] if
/// no catalog in the read set has any revision of the path;
/// [`OkfError::Database`].
pub(crate) async fn history(ctx: &ToolContext, params: HistoryParams) -> Result<History, OkfError> {
    let limit = revision_limit(params.limit);
    let paths = candidates(&params.path);
    for state in ctx.read_set(params.catalog.as_deref())? {
        for path in &paths {
            // `.await` suspends this call until Postgres answers, freeing the
            // thread for other requests meanwhile.
            let rows = revisions(ctx.app().pool(), state.id(), path, limit).await?;
            if rows.is_empty() {
                continue;
            }
            // `into_iter` consumes `rows`, so each row's strings move into
            // the result instead of being copied.
            let revisions = rows
                .into_iter()
                .map(|row| Revision {
                    revision: row.revision,
                    op: row.op,
                    actor: row.actor,
                    message: row.message,
                    at: format_timestamp(row.created_at),
                })
                .collect();
            return Ok(History {
                catalog: state.name().to_string(),
                path: format!("/{path}"),
                revisions,
            });
        }
    }
    Err(OkfError::NotFound { path: params.path })
}

// As in `catalogs.rs`: the macro turns this block's `#[tool]` methods into a
// router named `history_router`, joined in `OkfServer::tools`.
#[tool_router(router = history_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `history` tool.
    // The return type is spelled out (not `ToolResult<...>`) so the macro
    // sees `Json<...>` and builds the output schema.
    #[tool(
        description = "A document's revisions, newest first: revision, op (create, update, \
                       delete, import), actor, message and time. Deleted documents keep their \
                       history; log.md, indexes and other files have one too. Without \
                       `catalog`, the first selected catalog with any revision of the path \
                       answers. At most `limit` revisions (default 20, at most 200)."
    )]
    async fn history(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<HistoryParams>,
    ) -> Result<Json<History>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(history(&ctx, params).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::mcp::testing::{app_with, context, principal};

    fn params(path: &str) -> HistoryParams {
        HistoryParams {
            path: path.to_string(),
            catalog: None,
            limit: None,
        }
    }

    #[test]
    fn the_limit_defaults_to_20_counts_0_as_1_and_is_capped_at_200() {
        let limits = [None, Some(0), Some(5), Some(200), Some(u32::MAX)].map(revision_limit);

        assert_eq!(limits, [20, 1, 5, 200, 200]);
    }

    #[test]
    fn candidates_add_md_and_drop_paths_that_leave_the_catalog() {
        let plain = candidates("/infra/b");
        let escaping = candidates("../x");

        assert_eq!(plain, ["infra/b", "infra/b.md"]);
        assert!(escaping.is_empty());
    }

    #[test]
    fn the_server_lists_history_with_an_object_output_schema() {
        let tools = OkfServer::tools().list_all();

        let listed = tools.iter().find(|tool| tool.name == "history");

        let schema = listed
            .and_then(|tool| tool.output_schema.as_ref())
            .expect("history is registered with an output schema");
        assert_eq!(schema["type"], "object");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn history_finds_the_path_in_the_read_set_and_spells_paths_like_get_concept() {
        let test = app_with(
            &[
                ("pg", &[("a.md", "# A\n")]),
                ("work", &[("infra/b.md", "# B\n"), ("log.md", "# Log\n")]),
            ],
            Config::default(),
        )
        .await;
        let ctx = context(&test, principal(&[], None), Some(&["pg", "work"]));

        let found = history(&ctx, params("infra/b")).await.unwrap();
        let log = history(&ctx, params("/log.md")).await.unwrap();
        let missing = history(&ctx, params("nowhere")).await.unwrap_err();
        let elsewhere = HistoryParams {
            catalog: Some("pg".to_string()),
            ..params("infra/b")
        };
        let not_in_pg = history(&ctx, elsewhere).await.unwrap_err();
        let zero = HistoryParams {
            limit: Some(0),
            ..params("infra/b")
        };
        let zero = history(&ctx, zero).await.unwrap();

        assert_eq!(
            (found.catalog.as_str(), found.path.as_str()),
            ("work", "/infra/b.md")
        );
        assert_eq!(found.revisions.len(), 1);
        assert_eq!(found.revisions[0].op, "import");
        assert_eq!(found.revisions[0].actor, "tester");
        assert!(found.revisions[0].at.ends_with('Z'));
        assert_eq!(log.path, "/log.md");
        assert_eq!(zero.revisions.len(), 1);
        assert!(matches!(missing, OkfError::NotFound { .. }));
        assert!(matches!(not_in_pg, OkfError::NotFound { .. }));
    }
}

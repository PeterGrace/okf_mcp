//! `list_catalogs`: the catalogs a key can reach and this connection's
//! selection.

use axum::http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, Json};
use schemars::JsonSchema;
use serde::Serialize;

use crate::clock::format_timestamp;
use crate::mcp::context::ToolContext;
use crate::mcp::OkfServer;

/// One reachable catalog.
// `JsonSchema` gives the tool its output schema; doc comments become the
// fields' descriptions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct CatalogInfo {
    /// The catalog's name.
    pub name: String,
    /// Its description.
    pub description: String,
    /// How many files it holds.
    pub files: usize,
    /// Its newest change (`2026-11-02T21:00:00Z`), or null when empty.
    pub last_change: Option<String>,
}

/// `list_catalogs`' result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct CatalogList {
    /// Every catalog this key can reach, by name.
    pub catalogs: Vec<CatalogInfo>,
    /// The catalogs a call without a `catalog` argument reads, in order.
    pub selection: Vec<String>,
    /// Where writes go by default: the first of `selection`.
    pub write_target: Option<String>,
    /// Why there is no selection, when there is none.
    pub selection_error: Option<String>,
}

/// Lists the catalogs `ctx`'s key reaches and the connection's selection.
///
/// # Arguments
/// * `ctx` - the call's context.
///
/// # Returns
/// The list; a selection that fails is reported in `selection_error`
/// rather than failing the call, since this is the tool to diagnose it with.
pub(crate) fn list(ctx: &ToolContext) -> CatalogList {
    let catalogs = ctx
        .catalogs()
        .iter()
        .filter(|(name, _)| ctx.principal().reaches(name))
        .map(|(name, state)| {
            let loaded = state.current();
            CatalogInfo {
                name: name.clone(),
                description: state.description().to_string(),
                files: loaded.files(),
                last_change: loaded.last_change().map(format_timestamp),
            }
        })
        .collect();
    let (selection, write_target, selection_error) = match ctx.select(None) {
        Ok(selection) => {
            let write_target = Some(selection.write_target().to_string());
            (selection.catalogs().to_vec(), write_target, None)
        }
        Err(error) => (Vec::new(), None, Some(error.to_string())),
    };
    CatalogList {
        catalogs,
        selection,
        write_target,
        selection_error,
    }
}

// `#[tool_router]` is a macro that runs at compile time: it turns the
// `#[tool]` methods of this block into a router named `catalogs_router`,
// which `OkfServer::tools` joins with the other modules' routers.
#[tool_router(router = catalogs_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `list_catalogs` tool.
    // `Extension(parts): Extension<Parts>` destructures the argument in the
    // signature: rmcp fills it with the HTTP request, and `parts` is the
    // value inside. The return type is spelled out, not `ToolResult<...>`:
    // the macro finds the output schema by reading `Json<...>` in the
    // written type, and an alias hides it.
    #[tool(
        description = "List the catalogs this API key can reach (name, description, file \
                       count, last change) and this connection's selection: the catalogs \
                       a call without a `catalog` argument reads, and the default write target."
    )]
    async fn list_catalogs(
        &self,
        Extension(parts): Extension<Parts>,
    ) -> Result<Json<CatalogList>, CallToolResult> {
        // `?` returns the error result at once, like a Python early `return`.
        let ctx = self.context(&parts)?;
        Ok(Json(list(&ctx)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::mcp::testing::{app_with, context, principal};

    #[tokio::test(flavor = "multi_thread")]
    async fn lists_reachable_catalogs_and_the_connection_selection() {
        let test = app_with(
            &[
                ("pg", &[("a.md", "# A\n"), ("b.md", "# B\n")]),
                ("work", &[]),
                ("home", &[]),
            ],
            Config::default(),
        )
        .await;
        let key = principal(&["pg", "work"], None);

        let listed = list(&context(&test, key, Some(&["work", "pg"])));

        let names: Vec<&str> = listed.catalogs.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["pg", "work"]);
        assert_eq!(listed.catalogs[0].files, 2);
        assert!(listed.catalogs[0].last_change.is_some());
        assert_eq!(listed.catalogs[1].files, 0);
        assert_eq!(listed.catalogs[1].last_change, None);
        assert_eq!(listed.selection, ["work", "pg"]);
        assert_eq!(listed.write_target.as_deref(), Some("work"));
        assert_eq!(listed.selection_error, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn without_any_source_the_selection_says_why() {
        let test = app_with(&[("pg", &[])], Config::default()).await;

        let listed = list(&context(&test, principal(&[], None), None));

        assert_eq!(listed.catalogs.len(), 1);
        assert!(listed.selection.is_empty());
        assert_eq!(listed.write_target, None);
        assert!(listed
            .selection_error
            .unwrap()
            .contains("no catalog selected"));
    }

    #[test]
    fn the_server_lists_the_tool() {
        let tools = OkfServer::tools().list_all();

        let listed = tools
            .iter()
            .find(|tool| tool.name == "list_catalogs")
            .expect("list_catalogs is registered");
        let schema = listed.output_schema.as_ref().expect("an output schema");
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["selection_error"].is_object());
    }
}

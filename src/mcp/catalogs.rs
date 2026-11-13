//! `list_catalogs`: the catalogs this connection offers and its selection.

use axum::http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, Json};
use schemars::JsonSchema;
use serde::Serialize;

use crate::clock::format_timestamp;
use crate::mcp::context::ToolContext;
use crate::mcp::OkfServer;
use crate::store::teams::SuspendedCatalog;

/// Why a listed catalog is unavailable: its owner team is suspended (M13).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct SuspensionInfo {
    /// The owner team.
    pub team: String,
    /// The operator's notice, if any.
    pub notice: Option<String>,
}

// `From` is Rust's conversion trait (like a Python `from_x` classmethod);
// implementing it also gives `.into()` and lets `.map(SuspensionInfo::from)`
// convert a borrowed cache entry into an owned listing field.
impl From<&SuspendedCatalog> for SuspensionInfo {
    fn from(suspended: &SuspendedCatalog) -> Self {
        Self {
            team: suspended.team.clone(),
            notice: suspended.notice.clone(),
        }
    }
}

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
    /// Set when its owner team is suspended: then only export works (for a
    /// credential below admin). Null for an active catalog.
    pub suspended: Option<SuspensionInfo>,
}

/// `list_catalogs`' result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct CatalogList {
    /// Every catalog this connection offers, by name: those the key reaches,
    /// narrowed to the connection's list when it has one.
    pub catalogs: Vec<CatalogInfo>,
    /// The catalogs a call without a `catalog` argument reads, in order.
    pub selection: Vec<String>,
    /// Where writes go by default: the first of `selection`.
    pub write_target: Option<String>,
    /// Why there is no selection, when there is none.
    pub selection_error: Option<String>,
}

/// Lists the catalogs `ctx` offers ([`ToolContext::offers`]) and the
/// connection's selection.
///
/// # Arguments
/// * `ctx` - the call's context.
///
/// # Returns
/// The list; a selection that fails is reported in `selection_error`
/// rather than failing the call, since this is the tool to diagnose it with.
/// A suspended catalog is listed with `suspended` set, and the selection
/// leaves it out of a connection's list ([`crate::selection::resolve`]).
pub(crate) fn list(ctx: &ToolContext) -> CatalogList {
    let catalogs = ctx
        .catalogs()
        .iter()
        .filter(|(name, _)| ctx.offers(name))
        .map(|(name, state)| {
            let loaded = state.current();
            CatalogInfo {
                name: name.clone(),
                description: state.description().to_string(),
                files: loaded.files(),
                last_change: loaded.last_change().map(format_timestamp),
                // Listed only after `offers` (reach), so a credential that does
                // not reach a catalog never learns it is suspended.
                suspended: ctx.principal().suspension(name).map(SuspensionInfo::from),
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
        title = "List Catalogs",
        description = "List the catalogs this connection offers (name, description, file \
                       count, last change): those the API key can reach, limited to the \
                       connection's X-OKF-Catalogs list when it has one. A catalog whose \
                       team is suspended is listed with `suspended` set, and only export \
                       works on it. Also this connection's selection: the catalogs a call \
                       without a `catalog` argument reads, and the default write target.",
        annotations(
            title = "List Catalogs",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
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
    use std::sync::Arc;

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
    async fn a_connection_list_hides_the_key_default_and_other_grants() {
        let test = app_with(
            &[("pg", &[]), ("work", &[]), ("home", &[])],
            Config::default(),
        )
        .await;
        let key = principal(&["pg", "work"], Some("pg"));

        let listed = list(&context(&test, Arc::clone(&key), Some(&["work"])));
        let mistyped = list(&context(&test, key, Some(&["work", "home"])));

        let names: Vec<&str> = listed.catalogs.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["work"]);
        assert_eq!(listed.selection, ["work"]);
        assert_eq!(listed.write_target.as_deref(), Some("work"));
        // An ungranted name is reported, never listed, and nothing falls back.
        let names: Vec<&str> = mistyped.catalogs.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(names, ["work"]);
        assert!(mistyped.selection.is_empty());
        assert!(mistyped.selection_error.unwrap().contains("\"home\""));
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

    /// M13 Task 4: listing marks a suspended catalog instead of hiding it,
    /// and the connection's selection keeps the other team's catalog.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_suspended_catalog_is_listed_marked_and_the_others_still_select() {
        use crate::mcp::testing::{
            cached_principal, stored_secret, suspend_catalogs, SUSPENDED_TEAM, SUSPENSION_NOTICE,
        };
        use crate::store::keys::Access;
        let test = app_with(
            &[
                ("main", &[("index.md", "# Main\n")]),
                ("other", &[("index.md", "# Other\n")]),
            ],
            Config::default(),
        )
        .await;
        let reader = stored_secret(&test, "reader", Access::Read, &[], false).await;
        let stranger = stored_secret(&test, "stranger", Access::Read, &["other"], false).await;
        let root = stored_secret(&test, "root", Access::Admin, &[], false).await;
        suspend_catalogs(&test, &["main"]).await;
        let on = |secret: &str, connection: &[&str]| {
            context(&test, cached_principal(&test, secret), Some(connection))
        };

        let listed = list(&on(&reader, &["main", "other"]));
        let only_suspended = list(&on(&reader, &["main"]));
        let strangers = list(&on(&stranger, &["other"]));
        let admins = list(&on(&root, &["main", "other"]));

        let main = listed.catalogs.iter().find(|c| c.name == "main").unwrap();
        assert_eq!(
            main.suspended,
            Some(SuspensionInfo {
                team: SUSPENDED_TEAM.to_string(),
                notice: Some(SUSPENSION_NOTICE.to_string()),
            })
        );
        let other = listed.catalogs.iter().find(|c| c.name == "other").unwrap();
        assert_eq!(other.suspended, None, "the other team's catalog is active");
        assert_eq!(
            listed.selection,
            ["other"],
            "the connection keeps the active catalog"
        );
        assert_eq!(listed.write_target.as_deref(), Some("other"));
        assert!(only_suspended.selection.is_empty());
        assert!(
            only_suspended
                .selection_error
                .as_deref()
                .is_some_and(|error| error.contains("suspended")),
            "{:?}",
            only_suspended.selection_error
        );
        let names: Vec<&str> = strangers.catalogs.iter().map(|c| c.name.as_str()).collect();
        assert_eq!(
            names,
            ["other"],
            "a key that does not reach main never sees it"
        );
        assert!(
            admins.catalogs[0].suspended.is_some(),
            "an admin sees the mark"
        );
        assert_eq!(admins.selection, ["main", "other"], "and selects both");
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

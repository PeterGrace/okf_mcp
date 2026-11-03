//! The restructuring tools (design "Write tools", M6): `move_concept` and
//! `split_concept`, ports of `okf.py`'s `mv` and `split`, and
//! `delete_concept`. Each goes through [`crate::write::apply`], so it acts
//! on this connection's changeset in one catalog and the write rules apply.
//! A block of its own (`restructure_router`), joined in `OkfServer::tools`.

use axum::http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, Json};
use schemars::JsonSchema;
use serde::Deserialize;

use crate::error::OkfError;
use crate::mcp::context::ToolContext;
use crate::mcp::{reply, OkfServer};
use crate::okf::ops::{self, Split, DEFAULT_SPLIT_TYPE};
use crate::write::{apply, WriteReport};

/// `move_concept`'s arguments. Doc comments are the parameters'
/// descriptions in the tool's input schema.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct MoveConceptParams {
    /// The concept to move, `.md` optional, e.g. `/infra/ntp`.
    pub old: String,
    /// Its new path, `.md` optional, e.g. `/playbooks/ntp.md`; it must not exist.
    pub new: String,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// `split_concept`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct SplitConceptParams {
    /// The concept to split, `.md` optional, e.g. `/infra/ntp`.
    pub path: String,
    /// The section: its heading text (case-insensitive) or its anchor slug, e.g. `Ports` or `ports-1`.
    pub section: String,
    /// The new concept's path, `.md` optional; it must not exist.
    pub to: String,
    /// The new concept's `type` (default: Note); a blank one is refused, as `create_concept` and `set_frontmatter` refuse it.
    // `rename`: the JSON name is `type`, a Rust keyword, so the field is `kind`.
    #[serde(rename = "type")]
    pub kind: Option<String>,
    /// The new concept's title (default: the heading, backticks dropped).
    pub title: Option<String>,
    /// A one-sentence description; also the index listing's summary and the default stub. Without it, a placeholder is written and a note says so.
    pub description: Option<String>,
    /// The paragraph left under the heading in the parent; a link to the new concept is appended unless it has one.
    pub stub: Option<String>,
    /// Ids of the parent's sources to copy (default: those the section's footnotes cite, else all of them).
    pub sources: Option<Vec<String>>,
    /// Who wrote it, for the `generated` stamp (default: the key's actor).
    pub by: Option<String>,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// `delete_concept`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct DeleteConceptParams {
    /// The concept, `.md` optional, e.g. `/infra/ntp`.
    pub path: String,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// Moves a concept, its listing and every link to it.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the old and new paths.
///
/// # Returns
/// The write's report: the old path (a deletion), the new one, and every
/// file whose links or listing changed.
///
/// # Errors
/// As [`apply`]; the op's [`OkfError::NotFound`] or [`OkfError::InvalidPath`]
/// (taken, reserved, machinery).
pub(crate) async fn move_concept(
    ctx: &ToolContext,
    params: MoveConceptParams,
) -> Result<WriteReport, OkfError> {
    let MoveConceptParams { old, new, catalog } = params;
    apply(ctx, catalog.as_deref(), None, move |ws, _| {
        ops::move_concept(ws, &old, &new)
    })
    .await
}

/// Splits a section out of a concept into a new one.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept, the section and the new concept.
///
/// # Returns
/// The write's report, with `okf.py split`'s notes.
///
/// # Errors
/// As [`apply`]; the op's refusals ([`OkfError::Refused`] for a heading it
/// cannot find or that is ambiguous, an empty section or an unknown source).
pub(crate) async fn split_concept(
    ctx: &ToolContext,
    params: SplitConceptParams,
) -> Result<WriteReport, OkfError> {
    // Destructuring moves each field into its own variable, so the closure
    // can take the ones it needs while `catalog` and `by` stay here.
    let SplitConceptParams {
        path,
        section,
        to,
        kind,
        title,
        description,
        stub,
        sources,
        by,
        catalog,
    } = params;
    apply(ctx, catalog.as_deref(), by.as_deref(), move |ws, op_ctx| {
        // `unwrap_or_default` on an `Option<Vec<_>>`: an empty list when absent.
        let sources = sources.unwrap_or_default();
        let split = Split {
            concept: &path,
            section: &section,
            to: &to,
            kind: kind.as_deref().unwrap_or(DEFAULT_SPLIT_TYPE),
            title: title.as_deref(),
            description: description.as_deref(),
            stub: stub.as_deref(),
            sources: &sources,
        };
        ops::split_concept(ws, op_ctx, &split)
    })
    .await
}

/// Deletes a concept and its listing, refused while it is linked.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept.
///
/// # Returns
/// The write's report: the concept (a deletion) and its folder index.
///
/// # Errors
/// As [`apply`]; [`OkfError::StillLinked`] while other documents link to
/// it; [`OkfError::InvalidPath`] for a non-concept or machinery.
pub(crate) async fn delete_concept(
    ctx: &ToolContext,
    params: DeleteConceptParams,
) -> Result<WriteReport, OkfError> {
    let DeleteConceptParams { path, catalog } = params;
    apply(ctx, catalog.as_deref(), None, move |ws, _| {
        ops::delete_concept(ws, &path)
    })
    .await
}

// The restructuring tools; `OkfServer::tools` joins `restructure_router()` with the rest.
#[tool_router(router = restructure_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `move_concept` tool.
    #[tool(
        description = "Move or rename a concept in this connection's changeset (port of `okf.py \
                       mv`): its folder listing moves with it (creating folder indexes as \
                       needed) and every link to it is rewritten, keeping each link's style and \
                       anchor. `old` and `new` are concept paths, .md optional. Publish replays \
                       the index.md and log.md edits onto newer text; any other file it changed \
                       conflicts if someone changed it meanwhile. Write access."
    )]
    async fn move_concept(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<MoveConceptParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(move_concept(&ctx, params).await)
    }

    /// The `split_concept` tool.
    #[tool(
        description = "Carve a section (and its subsections) out of a concept into a new concept, \
                       in this connection's changeset (port of `okf.py split`). The new concept \
                       gets type, title, description, the parent's status, generated, and the \
                       sources the section's footnotes cite (or `sources`); the parent keeps the \
                       heading with a stub linking to it; links to the section follow it; it is \
                       listed in its folder's index.md. Returns okf.py's notes. Write access."
    )]
    async fn split_concept(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<SplitConceptParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(split_concept(&ctx, params).await)
    }

    /// The `delete_concept` tool.
    #[tool(
        description = "Delete a concept and its listing in its folder's index.md, in this \
                       connection's changeset. Refused with StillLinked, listing each link \
                       (source, line), while any other document links to it, log.md and other \
                       index.md files included; move_concept or edit_document fixes them first. \
                       index.md files are never deleted. Write access."
    )]
    async fn delete_concept(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<DeleteConceptParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(delete_concept(&ctx, params).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::mcp::changesets::{discard, pending_changes, revert, DiscardParams, RevertParams};
    use crate::mcp::reports::CatalogParam;
    use crate::mcp::testing::{app_with, writer, TestApp};
    use crate::mcp::writes::{edit_document, EditDocumentParams};
    use crate::store::changesets::{self, EntryRow};

    const TIME: &str = "---\ntype: System\ntitle: Time\ndescription: The clock.\n---\n\
                        # Time\n\n## Ports\n\nUDP 123.\n";
    const HUB: &str = "---\ntype: Note\ntitle: Hub\ndescription: Links.\n---\n\
                       See [time](/infra/time.md#ports).\n";

    async fn pg() -> TestApp {
        app_with(
            &[(
                "pg",
                &[
                    (
                        "index.md",
                        "# PG\n\n# Sections\n\n* [Infra](/infra/index.md) - systems\n",
                    ),
                    (
                        "infra/index.md",
                        "# Infra\n\n* [Time](time.md) - the clock\n",
                    ),
                    ("infra/time.md", TIME),
                    ("hub.md", HUB),
                    ("log.md", "# Log\n"),
                ],
            )],
            Config::default(),
        )
        .await
    }

    fn move_time(new: &str) -> MoveConceptParams {
        MoveConceptParams {
            old: "/infra/time".to_string(),
            new: new.to_string(),
            catalog: None,
        }
    }

    /// The changeset's entry for `path`.
    async fn entry(test: &TestApp, changeset: i64, path: &str) -> EntryRow {
        changesets::entries(test.app.pool(), changeset)
            .await
            .unwrap()
            .into_iter()
            .find(|entry| entry.path == path)
            .unwrap_or_else(|| panic!("no entry for {path}"))
    }

    /// The text of `path` as this connection sees it (its overlay).
    async fn seen(ctx: &ToolContext, path: &str) -> Option<String> {
        let state = ctx.app().catalog("pg").unwrap();
        let view = ctx.view(&state).await.unwrap();
        view.snapshot().get(path).map(|doc| doc.text().to_string())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_move_stores_a_deletion_a_creation_and_a_replayable_listing() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;

        let report = move_concept(&ctx, move_time("/infra/clock.md"))
            .await
            .unwrap();

        assert_eq!(
            report.touched,
            [
                "/infra/time.md",
                "/infra/clock.md",
                "/hub.md",
                "/infra/index.md"
            ]
        );
        assert_eq!(report.message, "moved /infra/time.md -> /infra/clock.md");
        let old = entry(&test, report.changeset, "infra/time.md").await;
        assert_eq!((old.content, old.base_revision), (None, Some(1)));
        let new = entry(&test, report.changeset, "infra/clock.md").await;
        assert_eq!((new.base_revision, new.replay), (None, None));
        assert_eq!(entry(&test, report.changeset, "hub.md").await.replay, None);
        let listing = entry(&test, report.changeset, "infra/index.md").await;
        assert_eq!(listing.replay.expect("a relink replays")[0]["op"], "relink");
        assert_eq!(seen(&ctx, "infra/time.md").await, None);
        assert!(seen(&ctx, "hub.md")
            .await
            .unwrap()
            .contains("[time](/infra/clock.md#ports)"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_split_lists_the_new_concept_and_returns_okf_py_s_notes() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;

        let report = split_concept(
            &ctx,
            SplitConceptParams {
                path: "/infra/time".to_string(),
                section: "Ports".to_string(),
                to: "/infra/ports.md".to_string(),
                kind: None,
                title: None,
                description: Some("UDP ports.".to_string()),
                stub: None,
                sources: None,
                by: None,
                catalog: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(
            report.touched,
            [
                "/infra/time.md",
                "/infra/ports.md",
                "/hub.md",
                "/infra/index.md"
            ]
        );
        assert_eq!(report.notes.len(), 1, "{:?}", report.notes);
        assert!(report.notes[0].starts_with("note: 'Ports' is the last section"));
        let ports = seen(&ctx, "infra/ports.md").await.unwrap();
        // `type` defaulted to Note, and `by` to the key's actor.
        assert!(
            ports.starts_with("---\ntype: Note\ntitle: Ports\n"),
            "{ports}"
        );
        assert!(ports.contains("by: writer-a"), "{ports}");
        assert!(seen(&ctx, "hub.md")
            .await
            .unwrap()
            .contains("[time](/infra/ports.md)"));
        let listing = entry(&test, report.changeset, "infra/index.md").await;
        assert_eq!(
            listing.replay.expect("a listing replays")[0]["op"],
            "list_bullet"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_blank_type_is_refused_and_nothing_is_stored() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;

        let refused = split_concept(
            &ctx,
            SplitConceptParams {
                path: "/infra/time".to_string(),
                section: "Ports".to_string(),
                to: "/infra/ports.md".to_string(),
                kind: Some("  ".to_string()),
                title: None,
                description: None,
                stub: None,
                sources: None,
                by: None,
                catalog: None,
            },
        )
        .await
        .unwrap_err();

        let body = refused.to_json();
        assert_eq!(body["error"], "Refused");
        assert_eq!(
            refused.to_string(),
            crate::okf::ops::blank_type().to_string()
        );
        let state = ctx.app().catalog("pg").unwrap();
        assert!(!ctx.view(&state).await.unwrap().pending());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_delete_is_refused_while_linked_then_goes_through() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let delete = || DeleteConceptParams {
            path: "/infra/time".to_string(),
            catalog: None,
        };

        let refused = delete_concept(&ctx, delete()).await.unwrap_err();
        let state = ctx.app().catalog("pg").unwrap();
        let nothing_stored = !ctx.view(&state).await.unwrap().pending();
        edit_document(
            &ctx,
            EditDocumentParams {
                path: "/hub.md".to_string(),
                old_str: "See [time](/infra/time.md#ports).".to_string(),
                new_str: "See the time server.".to_string(),
                replace_all: false,
                catalog: None,
            },
        )
        .await
        .unwrap();
        let deleted = delete_concept(&ctx, delete()).await.unwrap();

        let body = refused.to_json();
        assert_eq!(body["error"], "StillLinked");
        assert_eq!(
            body["links"],
            serde_json::json!([{"source": "/hub.md", "line": 6}])
        );
        assert!(nothing_stored, "a refused delete stores nothing");
        assert_eq!(deleted.touched, ["/infra/time.md", "/infra/index.md"]);
        let listing = entry(&test, deleted.changeset, "infra/index.md").await;
        assert_eq!(
            listing.replay.expect("an unlist replays")[0]["op"],
            "unlist"
        );
        assert_eq!(
            seen(&ctx, "infra/index.md").await.as_deref(),
            Some("# Infra\n\n")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_move_is_listed_reverted_and_discarded_like_any_other_edit() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let pending = || CatalogParam { catalog: None };
        move_concept(&ctx, move_time("/infra/clock.md"))
            .await
            .unwrap();

        let listed = pending_changes(&ctx, pending()).await.unwrap();
        revert(
            &ctx,
            RevertParams {
                path: "/infra/time.md".to_string(),
                revision: 1,
                catalog: None,
            },
        )
        .await
        .unwrap();
        let reverted = pending_changes(&ctx, pending()).await.unwrap();
        let discarded = discard(
            &ctx,
            DiscardParams {
                paths: None,
                catalog: None,
            },
        )
        .await
        .unwrap();

        let changes: Vec<(&str, &str, bool)> = listed
            .files
            .iter()
            .map(|file| (file.path.as_str(), file.change, file.replayable))
            .collect();
        assert_eq!(
            changes,
            [
                ("/hub.md", "update", false),
                ("/infra/clock.md", "create", false),
                ("/infra/index.md", "update", true),
                ("/infra/time.md", "delete", false),
            ]
        );
        // The old path is back, byte for byte the published text.
        let time = reverted
            .files
            .iter()
            .find(|file| file.path == "/infra/time.md")
            .unwrap();
        assert_eq!(
            (time.change, time.diff.as_str()),
            ("update", "No change from the published text\n")
        );
        assert_eq!((discarded.removed, discarded.closed), (4, true));
        let state = ctx.app().catalog("pg").unwrap();
        assert!(!ctx.view(&state).await.unwrap().pending());
    }

    #[test]
    fn the_three_tools_are_listed_with_their_schemas() {
        let tools = OkfServer::restructure_router().list_all();
        // `&*` borrows the name (a `Cow<str>`) as a plain `&str`.
        let names: Vec<&str> = tools.iter().map(|tool| &*tool.name).collect();
        let split = tools
            .iter()
            .find(|tool| tool.name == "split_concept")
            .unwrap();
        let properties = split.input_schema["properties"].as_object().unwrap();
        let required = split.input_schema["required"].as_array().unwrap();

        assert_eq!(names.len(), 3, "{names:?}");
        // And the server's router has them (`OkfServer::tools` joins this block).
        let all = OkfServer::tools().list_all();
        assert!(all.iter().any(|tool| tool.name == "delete_concept"));
        for name in ["move_concept", "split_concept", "delete_concept"] {
            assert!(names.contains(&name), "{name} is registered");
        }
        assert!(properties.contains_key("type") && !properties.contains_key("kind"));
        assert!(properties.contains_key("sources"));
        assert_eq!(
            required
                .iter()
                .filter_map(|name| name.as_str())
                .collect::<Vec<_>>(),
            ["path", "section", "to"]
        );
        for tool in &tools {
            let output = tool.output_schema.as_ref().expect("an output schema");
            assert_eq!(output["type"], "object", "{}", tool.name);
            let inputs = tool.input_schema["properties"].as_object().unwrap();
            assert!(
                !inputs.keys().any(|key| key == "at" || key == "now"),
                "{}",
                tool.name
            );
        }
    }
}

//! The write tools (design "Write tools"): the content tools here (Task 8),
//! the bookkeeping tools below them (Task 9). Each goes through
//! [`crate::write::apply`], so it acts on this connection's changeset in one
//! catalog, the write rules apply, and nothing is visible to other
//! connections until `publish`.

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
use crate::okf::ops::{self, NewConcept};
use crate::write::{apply, WriteReport};

/// `create_concept`'s arguments. Doc comments are the parameters'
/// descriptions in the tool's input schema.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct CreateConceptParams {
    /// The new concept's path, e.g. `/infra/ntp.md` (the `.md` is optional); it must not exist.
    pub path: String,
    /// Frontmatter keys as a JSON object; `type` is required, and `generated`, `verified` and `reviewed` are refused (the server writes `generated`).
    pub frontmatter: serde_json::Map<String, serde_json::Value>,
    /// The markdown body after the frontmatter.
    pub body: String,
    /// List the concept in its folder's index.md with this summary, creating folder indexes as needed.
    pub index_summary: Option<String>,
    /// The index.md section to list it under (default: where `okf.py index` puts it); needs `index_summary`.
    pub index_section: Option<String>,
    /// Who wrote it, for the `generated` stamp (default: the key's actor).
    pub by: Option<String>,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// `edit_document`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct EditDocumentParams {
    /// A full path with `.md`: a concept or an index.md, e.g. `/infra/ntp.md` (`/infra/ntp` is refused).
    pub path: String,
    /// The exact text to replace; without `replace_all` it must occur exactly once. The edit may not add or change frontmatter.
    pub old_str: String,
    /// The replacement text.
    pub new_str: String,
    /// Replace every occurrence instead of exactly one.
    #[serde(default)]
    pub replace_all: bool,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// `replace_body`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct ReplaceBodyParams {
    /// The concept, with or without `.md`, e.g. `/infra/ntp` or `/infra/ntp.md`.
    pub path: String,
    /// The new body; the frontmatter is kept byte for byte, and the body may not add any.
    pub body: String,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// `set_frontmatter`'s arguments.
#[derive(Debug, Clone, PartialEq, Deserialize, JsonSchema)]
pub struct SetFrontmatterParams {
    /// The concept, with or without `.md`, e.g. `/infra/ntp` or `/infra/ntp.md`.
    pub path: String,
    /// A descriptive key such as `title`, `status` or `tags`; the stamps, `open_questions` and `sources` have their own tools.
    pub key: String,
    /// The new value as JSON; required, and an explicit null deletes the key (`type` cannot be deleted, and must be a non-blank string).
    // No `#[serde(default)]`: a missing `value` is a parameter error, so
    // forgetting the argument can never delete the key. `serde_json::Value`
    // is not an `Option`, so serde treats the field as required.
    pub value: serde_json::Value,
    /// The catalog (default: this connection's write target).
    pub catalog: Option<String>,
}

/// Creates a concept (and, with `index_summary`, its listing).
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept.
///
/// # Returns
/// The write's report.
///
/// # Errors
/// As [`apply`]; the op's [`OkfError::InvalidPath`] (taken, reserved,
/// machinery) or [`OkfError::Refused`] (a missing `type`, a stamp key, a
/// listing it cannot make).
pub(crate) async fn create_concept(
    ctx: &ToolContext,
    params: CreateConceptParams,
) -> Result<WriteReport, OkfError> {
    // Destructuring moves each field into its own variable, so the closure
    // can take the ones it needs while `catalog` and `by` stay here.
    let CreateConceptParams {
        path,
        frontmatter,
        body,
        index_summary,
        index_section,
        by,
        catalog,
    } = params;
    apply(ctx, catalog.as_deref(), by.as_deref(), move |ws, op_ctx| {
        let new = NewConcept {
            path: &path,
            frontmatter: &frontmatter,
            body: &body,
            index_summary: index_summary.as_deref(),
            index_section: index_section.as_deref(),
        };
        ops::create_concept(ws, op_ctx, &new)
    })
    .await
}

/// Replaces exact text in a concept or an `index.md`.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the document (a full `.md` path) and the replacement.
///
/// # Returns
/// The write's report.
///
/// # Errors
/// As [`apply`]; [`OkfError::EditMismatch`] when `old_str` is found 0 or
/// (without `replace_all`) more than 1 times; [`OkfError::InvalidPath`] for
/// `log.md`, machinery or a path without `.md`; [`OkfError::Refused`] for an
/// edit that adds or changes frontmatter.
pub(crate) async fn edit_document(
    ctx: &ToolContext,
    params: EditDocumentParams,
) -> Result<WriteReport, OkfError> {
    let EditDocumentParams {
        path,
        old_str,
        new_str,
        replace_all,
        catalog,
    } = params;
    apply(ctx, catalog.as_deref(), None, move |ws, _| {
        ops::edit_document(ws, &path, &old_str, &new_str, replace_all)
    })
    .await
}

/// Replaces a concept's body, keeping its frontmatter byte for byte.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept reference and its new body.
///
/// # Returns
/// The write's report.
///
/// # Errors
/// As [`apply`]; the op's [`OkfError::NotFound`], [`OkfError::InvalidPath`]
/// or [`OkfError::Refused`] (a body that adds frontmatter).
pub(crate) async fn replace_body(
    ctx: &ToolContext,
    params: ReplaceBodyParams,
) -> Result<WriteReport, OkfError> {
    let ReplaceBodyParams {
        path,
        body,
        catalog,
    } = params;
    apply(ctx, catalog.as_deref(), None, move |ws, _| {
        ops::replace_body(ws, &path, &body)
    })
    .await
}

/// Sets or deletes one descriptive frontmatter key, as a textual edit.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the concept reference, the key and the value (null deletes).
///
/// # Returns
/// The write's report.
///
/// # Errors
/// As [`apply`]; the op's refusal of a dedicated key, of deleting `type`
/// or of a `type` that is not a non-blank string.
pub(crate) async fn set_frontmatter(
    ctx: &ToolContext,
    params: SetFrontmatterParams,
) -> Result<WriteReport, OkfError> {
    let SetFrontmatterParams {
        path,
        key,
        value,
        catalog,
    } = params;
    // The op reads a JSON null as a delete, so the value passes as it is.
    apply(ctx, catalog.as_deref(), None, move |ws, _| {
        ops::set_frontmatter(ws, &path, &key, Some(&value))
    })
    .await
}

// The content tools; `OkfServer::tools` joins `writes_router()` with the rest.
#[tool_router(router = writes_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `create_concept` tool.
    #[tool(
        description = "Create a concept in this connection's changeset (nothing is visible to \
                       others until `publish`). `path` is the new concept's path, `.md` optional \
                       (e.g. /infra/ntp). Needs `type` in `frontmatter`; the server writes \
                       `generated: { by, at }` itself. With `index_summary`, also lists it in its \
                       folder's index.md (creating folder indexes as needed). Write access."
    )]
    async fn create_concept(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<CreateConceptParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(create_concept(&ctx, params).await)
    }

    /// The `edit_document` tool.
    #[tool(
        description = "Replace exact text in a concept or an index.md, in this connection's \
                       changeset. `path` is a full path with .md (e.g. /infra/ntp.md or \
                       /infra/index.md). `old_str` must occur exactly once unless `replace_all`; \
                       the edit may not add or change frontmatter. log.md and machinery are \
                       refused (use `log`). Write access."
    )]
    async fn edit_document(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<EditDocumentParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(edit_document(&ctx, params).await)
    }

    /// The `replace_body` tool.
    #[tool(
        description = "Replace a concept's body, keeping its frontmatter byte for byte, in this \
                       connection's changeset. `path` is a concept reference, .md optional \
                       (/infra/ntp or /infra/ntp.md). Write access."
    )]
    async fn replace_body(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<ReplaceBodyParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(replace_body(&ctx, params).await)
    }

    /// The `set_frontmatter` tool.
    #[tool(
        description = "Set (or, with a null value, delete) one descriptive frontmatter key of a \
                       concept, in this connection's changeset. `path` is a concept reference, \
                       .md optional (/infra/ntp or /infra/ntp.md). The stamps, open_questions \
                       and sources have their own tools; `type` cannot be deleted. Write access."
    )]
    async fn set_frontmatter(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<SetFrontmatterParams>,
    ) -> Result<Json<WriteReport>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(set_frontmatter(&ctx, params).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::mcp::testing::{app_with, writer, TestApp};
    use crate::store::changesets;

    const NOTE: &str = "---\ntype: Note\ntitle: A\ndescription: A note.\n---\n# A\n\nOld text.\n";

    async fn pg() -> TestApp {
        app_with(
            &[(
                "pg",
                &[
                    (
                        "index.md",
                        "# Catalog\n\n# Sections\n\n* [Infra](infra/index.md)\n",
                    ),
                    ("a.md", NOTE),
                    ("infra/index.md", "# Infra\n"),
                    ("log.md", "# Log\n"),
                ],
            )],
            Config::default(),
        )
        .await
    }

    /// The text of `path` as this connection sees it (its overlay).
    async fn seen(ctx: &ToolContext, path: &str) -> String {
        let state = ctx.app().catalog("pg").unwrap();
        let view = ctx.view(&state).await.unwrap();
        view.snapshot()
            .get(path)
            .map(|doc| doc.text().to_string())
            .unwrap_or_default()
    }

    fn edit(path: &str, old_str: &str, new_str: &str) -> EditDocumentParams {
        EditDocumentParams {
            path: path.to_string(),
            old_str: old_str.to_string(),
            new_str: new_str.to_string(),
            replace_all: false,
            catalog: None,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_content_tools_edit_through_the_overlay() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;
        let frontmatter =
            serde_json::json!({"type": "System", "title": "NTP", "description": "Hosts sync."});

        let created = create_concept(
            &ctx,
            CreateConceptParams {
                path: "/infra/ntp.md".to_string(),
                frontmatter: frontmatter.as_object().unwrap().clone(),
                body: "# NTP\n\nSee [A](/a.md).\n".to_string(),
                index_summary: None,
                index_section: None,
                by: None,
                catalog: None,
            },
        )
        .await
        .unwrap();
        let edited = edit_document(&ctx, edit("/a.md", "Old text.", "New text."))
            .await
            .unwrap();
        replace_body(
            &ctx,
            ReplaceBodyParams {
                path: "/infra/ntp".to_string(),
                body: "# NTP\n\nRewritten.\n".to_string(),
                catalog: None,
            },
        )
        .await
        .unwrap();
        set_frontmatter(
            &ctx,
            SetFrontmatterParams {
                path: "/a.md".to_string(),
                key: "status".to_string(),
                value: serde_json::json!("draft"),
                catalog: None,
            },
        )
        .await
        .unwrap();
        let mismatch = edit_document(&ctx, edit("/a.md", "not there", "x"))
            .await
            .unwrap_err();

        assert_eq!(created.touched, ["/infra/ntp.md"]);
        assert_eq!(edited.changeset, created.changeset);
        let ntp = seen(&ctx, "infra/ntp.md").await;
        // `by` defaulted to the key's actor in the `generated` stamp.
        assert!(
            ntp.contains("generated:") && ntp.contains("writer-a"),
            "{ntp}"
        );
        assert!(ntp.ends_with("# NTP\n\nRewritten.\n"), "{ntp}");
        let a = seen(&ctx, "a.md").await;
        assert!(
            a.contains("status: draft") && a.contains("New text."),
            "{a}"
        );
        assert!(matches!(
            mismatch,
            OkfError::EditMismatch { matches: 0, .. }
        ));
        let entries = changesets::entries(test.app.pool(), created.changeset)
            .await
            .unwrap();
        let paths: Vec<&str> = entries.iter().map(|entry| entry.path.as_str()).collect();
        assert_eq!(paths, ["a.md", "infra/ntp.md"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn edit_document_refuses_log_md() {
        let test = pg().await;
        let ctx = writer(&test, "writer-a", "pg").await;

        let log = edit_document(&ctx, edit("/log.md", "# Log", "# Mine"))
            .await
            .unwrap_err();
        // A concept reference without `.md` is not a document path here.
        let bare = edit_document(&ctx, edit("/a", "Old text.", "New."))
            .await
            .unwrap_err();

        assert_eq!(log.kind(), "InvalidPath");
        assert_eq!(bare.kind(), "InvalidPath");
    }

    #[test]
    fn set_frontmatter_requires_value_and_only_an_explicit_null_deletes() {
        let missing = serde_json::json!({"path": "/a", "key": "status"});
        let null = serde_json::json!({"path": "/a", "key": "status", "value": null});

        let refused = serde_json::from_value::<SetFrontmatterParams>(missing);
        let delete = serde_json::from_value::<SetFrontmatterParams>(null).unwrap();
        let tools = OkfServer::writes_router().list_all();
        let tool = tools
            .iter()
            .find(|tool| tool.name == "set_frontmatter")
            .unwrap();

        let error = refused.unwrap_err().to_string();
        assert!(error.contains("missing field `value`"), "{error}");
        assert_eq!(delete.value, serde_json::Value::Null);
        let required = tool.input_schema["required"].as_array().unwrap();
        assert!(required.iter().any(|name| name == "value"), "{required:?}");
    }

    #[test]
    fn the_write_tools_have_object_output_schemas_and_say_who_by_defaults_to() {
        let tools = OkfServer::writes_router().list_all();

        for name in [
            "create_concept",
            "edit_document",
            "replace_body",
            "set_frontmatter",
        ] {
            let tool = tools
                .iter()
                .find(|tool| tool.name == name)
                .unwrap_or_else(|| panic!("{name}"));
            let output = tool.output_schema.as_ref().expect("an output schema");
            assert_eq!(output["type"], "object", "{name}");
        }
        let create = tools
            .iter()
            .find(|tool| tool.name == "create_concept")
            .unwrap();
        let by = create.input_schema["properties"]["by"]["description"]
            .as_str()
            .unwrap();
        // The phrase sits on one doc-comment line (schemars keeps line breaks).
        assert!(by.contains("default: the key's actor"), "{by}");
    }
}

//! `get_concept` and `read_document`: one document, from the first selected
//! catalog that has it (Decision 1).

use axum::http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, Json};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::OkfError;
use crate::mcp::context::ToolContext;
use crate::mcp::{reply, OkfServer};
use crate::okf::paths::{basename, escapes_root, normpath};
use crate::okf::snapshot::{Frontmatter, Snapshot, SnapshotDoc};
use crate::okf::yaml::Value;

/// `get_concept`'s arguments.
// `Deserialize` reads them from the call's JSON; `JsonSchema` gives the tool
// its input schema, with the doc comments as the fields' descriptions.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct GetConceptParams {
    /// The concept: `/infra/time-server.md`, `infra/time-server` or
    /// `/infra/time-server`.
    pub path: String,
    /// The catalog (default: the first selected catalog that has it).
    pub catalog: Option<String>,
    /// Also return its resolved outbound links and its backlinks.
    // `#[serde(default)]` makes the field optional: absent means `false`.
    #[serde(default)]
    pub links: bool,
    /// Also return the file verbatim.
    #[serde(default)]
    pub raw: bool,
}

/// A link out of the concept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct OutboundLink {
    /// 1-based line of the link.
    pub line: usize,
    /// `/`-prefixed target path.
    pub target: String,
    /// The text after `#`, if any.
    pub fragment: Option<String>,
    /// The target's title, if it is a document.
    pub title: Option<String>,
    /// The target's description, if it has one.
    pub description: Option<String>,
    /// The target does not exist.
    pub broken: bool,
}

/// A link into the concept from another document.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct InboundLink {
    /// `/`-prefixed path of the linking document.
    pub path: String,
    /// 1-based line of the link in it.
    pub line: usize,
    /// The linking document's title.
    pub title: Option<String>,
}

/// `get_concept`'s result.
// No `Eq`: `serde_json::Value` is only `PartialEq` (it can hold floats).
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Concept {
    /// The catalog it came from.
    pub catalog: String,
    /// `/`-prefixed path.
    pub path: String,
    /// Its title.
    pub title: Option<String>,
    /// The frontmatter as JSON, keys in file order; null when absent or invalid.
    pub frontmatter: serde_json::Value,
    /// Why the frontmatter does not parse, when it does not.
    pub frontmatter_error: Option<String>,
    /// The text after the frontmatter.
    pub body: String,
    /// The file verbatim (with `raw`).
    // Left out of the JSON entirely (not `null`) when not asked for.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<String>,
    /// Outbound links, in-page links left out (with `links`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub links: Option<Vec<OutboundLink>>,
    /// Backlinks (with `links`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backlinks: Option<Vec<InboundLink>>,
}

/// `read_document`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct ReadDocumentParams {
    /// An `index.md` (any folder's), `log.md` or `AGENTS.md`; default
    /// `/index.md`. Use `get_concept` for concepts.
    pub path: Option<String>,
    /// The catalog (default: the first selected catalog that has it).
    pub catalog: Option<String>,
}

/// `read_document`'s result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Document {
    /// The catalog it came from.
    pub catalog: String,
    /// `/`-prefixed path.
    pub path: String,
    /// The file verbatim.
    pub text: String,
}

/// A frontmatter string value.
///
/// # Arguments
/// * `doc` - the document.
/// * `key` - the top-level key.
///
/// # Returns
/// The value, when the frontmatter parses and `key` holds a string.
fn text_field(doc: &SnapshotDoc, key: &str) -> Option<String> {
    // `?` on an `Option` returns `None` at once, like a Python early return.
    let data = doc.frontmatter().data()?;
    data.get(key).and_then(Value::as_str).map(str::to_string)
}

/// The frontmatter as JSON, and the parse error if any.
///
/// # Arguments
/// * `doc` - the document.
///
/// # Returns
/// The object (keys in file order) and `None`; or null and the error's
/// message when it does not parse; or null and `None` without frontmatter.
fn frontmatter_json(doc: &SnapshotDoc) -> (serde_json::Value, Option<String>) {
    match doc.frontmatter() {
        Frontmatter::Parsed(data) => {
            let object = data
                .iter()
                .map(|(key, value)| (key.clone(), value.to_json()))
                .collect();
            (serde_json::Value::Object(object), None)
        }
        Frontmatter::Absent => (serde_json::Value::Null, None),
        Frontmatter::Invalid(error) => (serde_json::Value::Null, Some(error.to_string())),
    }
}

/// Builds the result for one resolved concept.
///
/// # Arguments
/// * `catalog` - the catalog's name.
/// * `snapshot` - its snapshot, for the links' targets and the backlinks.
/// * `doc` - the concept.
/// * `params` - the call's arguments (`links`, `raw`).
///
/// # Returns
/// The concept, with links and the raw text only when asked for.
fn concept(
    catalog: &str,
    snapshot: &Snapshot,
    doc: &SnapshotDoc,
    params: &GetConceptParams,
) -> Concept {
    let (frontmatter, frontmatter_error) = frontmatter_json(doc);
    // `bool::then(closure)` is `Some(closure())` when true, else `None`, so
    // the links are only computed when asked for.
    let links = params.links.then(|| {
        doc.links()
            .iter()
            .filter(|link| !link.in_page)
            .map(|link| {
                let target = snapshot.get(&link.target);
                OutboundLink {
                    line: link.line,
                    target: format!("/{}", link.target),
                    fragment: link.fragment.clone(),
                    title: target.and_then(|t| t.title().map(str::to_string)),
                    description: target.and_then(|t| text_field(t, "description")),
                    broken: !link.target_exists,
                }
            })
            .collect()
    });
    let backlinks = params.links.then(|| {
        snapshot
            .backlinks(doc.path())
            .iter()
            .map(|backlink| InboundLink {
                path: format!("/{}", backlink.source),
                line: backlink.line,
                title: snapshot
                    .get(&backlink.source)
                    .and_then(|source| source.title().map(str::to_string)),
            })
            .collect()
    });
    Concept {
        catalog: catalog.to_string(),
        path: format!("/{}", doc.path()),
        title: doc.title().map(str::to_string),
        frontmatter,
        frontmatter_error,
        body: doc.doc().body(),
        raw: params.raw.then(|| doc.raw()),
        links,
        backlinks,
    }
}

/// Finds a concept in the read set.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the call's arguments.
///
/// # Returns
/// The concept, from the first catalog of the read set that has it.
///
/// # Errors
/// A selection error; [`OkfError::NotFound`] if no selected catalog has it;
/// [`OkfError::InvalidPath`] if no selected catalog has it as a concept and
/// one holds a document at that path that is not a concept;
/// [`OkfError::Database`] looking up the connection's changeset
/// ([`ToolContext::view`]).
pub(crate) async fn get_concept(
    ctx: &ToolContext,
    params: GetConceptParams,
) -> Result<Concept, OkfError> {
    let mut refused = None;
    for state in ctx.read_set(params.catalog.as_deref())? {
        // This connection's view: its overlay if it has pending edits here.
        let view = ctx.view(&state).await?;
        let snapshot = view.snapshot();
        match snapshot.resolve_concept(&params.path) {
            Ok(path) => {
                let doc = snapshot
                    .get(&path)
                    .expect("resolve_concept returns only paths the snapshot holds");
                return Ok(concept(state.name(), snapshot, doc, &params));
            }
            Err(OkfError::NotFound { .. }) => {}
            // Keep looking, but report this if nothing better turns up.
            // `get_or_insert` keeps the first such error.
            Err(error) => {
                refused.get_or_insert(error);
            }
        }
    }
    Err(refused.unwrap_or(OkfError::NotFound { path: params.path }))
}

/// The documents `read_document` serves.
///
/// # Arguments
/// * `path` - a normalized, catalog-relative path.
///
/// # Returns
/// Whether it is an `index.md` or `log.md` (any folder's) or the root
/// `AGENTS.md`.
fn readable(path: &str) -> bool {
    matches!(basename(path), "index.md" | "log.md") || path == "AGENTS.md"
}

/// Reads an index, the log or `AGENTS.md` from the read set.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the call's arguments.
///
/// # Returns
/// The file verbatim, from the first catalog of the read set that has it.
///
/// # Errors
/// A selection error; [`OkfError::InvalidPath`] for any other path or one
/// outside the catalog; [`OkfError::NotFound`] if no selected catalog has it;
/// [`OkfError::Database`] looking up the connection's changeset
/// ([`ToolContext::view`]).
pub(crate) async fn read_document(
    ctx: &ToolContext,
    params: ReadDocumentParams,
) -> Result<Document, OkfError> {
    let asked = params.path.unwrap_or_else(|| "/index.md".to_string());
    let path = normpath(asked.trim_start_matches('/'));
    if escapes_root(&path) || !readable(&path) {
        return Err(OkfError::InvalidPath {
            path: asked,
            reason: "read_document reads index.md, log.md and AGENTS.md; \
                     use get_concept for concepts"
                .to_string(),
        });
    }
    for state in ctx.read_set(params.catalog.as_deref())? {
        if let Some(doc) = ctx.view(&state).await?.snapshot().get(&path) {
            return Ok(Document {
                catalog: state.name().to_string(),
                path: format!("/{path}"),
                text: doc.raw(),
            });
        }
    }
    Err(OkfError::NotFound { path: asked })
}

// The macro turns this block's `#[tool]` methods into `documents_router`,
// which `OkfServer::tools` joins with the other modules' routers.
#[tool_router(router = documents_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `get_concept` tool.
    // Inside a method, a bare `get_concept(..)` is the module's free
    // function: Rust reaches methods only through `self.` or `Self::`, so
    // the shared name does not clash. The return type is spelled out, not
    // `ToolResult<...>`, so the macro sees `Json<...>` and emits an output
    // schema.
    #[tool(
        description = "Get a concept: its frontmatter as JSON and its body. With `links`, its \
                       resolved outbound links (target, title, description, broken) and its \
                       backlinks; with `raw`, the file verbatim. Without `catalog`, the first \
                       selected catalog that has it answers."
    )]
    async fn get_concept(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<GetConceptParams>,
    ) -> Result<Json<Concept>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(get_concept(&ctx, params).await)
    }

    /// The `read_document` tool.
    #[tool(
        description = "Read the raw text of an index.md (default /index.md), log.md or \
                       AGENTS.md. Without `catalog`, the first selected catalog that has it \
                       answers."
    )]
    async fn read_document(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<ReadDocumentParams>,
    ) -> Result<Json<Document>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(read_document(&ctx, params).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    use crate::config::Config;
    use crate::mcp::testing::{
        app_with, context, pending_entry, principal, principal_as, store_key, PendingEdit,
    };
    use crate::store::keys::Access;

    const HUB: &str = "---\ntype: Note\ntitle: Hub\ndescription: The hub.\n---\n\
                       See [leaf](leaf.md) and [gone](gone.md). [Top](#hub).\n";
    const LEAF: &str =
        "---\ntype: Note\ntitle: Leaf\ndescription: A leaf.\n---\nBack to [hub](hub.md).\n";

    fn concept_params(path: &str) -> GetConceptParams {
        GetConceptParams {
            path: path.to_string(),
            catalog: None,
            links: false,
            raw: false,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_concept_returns_frontmatter_body_links_and_raw() {
        let test = app_with(
            &[(
                "pg",
                &[
                    ("hub.md", HUB),
                    ("leaf.md", LEAF),
                    ("index.md", "# Index\n"),
                ],
            )],
            Config::default(),
        )
        .await;
        let ctx = context(&test, principal(&[], Some("pg")), None);
        let mut params = concept_params("hub");
        params.links = true;
        params.raw = true;

        let concept = get_concept(&ctx, params).await.unwrap();

        assert_eq!(concept.catalog, "pg");
        assert_eq!(concept.path, "/hub.md");
        assert_eq!(concept.title.as_deref(), Some("Hub"));
        assert_eq!(concept.frontmatter["title"], "Hub");
        assert_eq!(concept.frontmatter_error, None);
        assert_eq!(
            concept.body,
            "See [leaf](leaf.md) and [gone](gone.md). [Top](#hub).\n"
        );
        assert_eq!(concept.raw.as_deref(), Some(HUB));
        let links = concept.links.unwrap();
        // `[Top](#hub)` is an in-page link, so it is left out.
        assert_eq!(links.len(), 2);
        assert_eq!(
            (links[0].target.as_str(), links[0].broken),
            ("/leaf.md", false)
        );
        assert_eq!(links[0].title.as_deref(), Some("Leaf"));
        assert_eq!(links[0].description.as_deref(), Some("A leaf."));
        assert_eq!(
            (links[1].target.as_str(), links[1].broken),
            ("/gone.md", true)
        );
        assert_eq!(links[1].description, None);
        let backlinks = concept.backlinks.unwrap();
        assert_eq!(backlinks.len(), 1);
        assert_eq!(backlinks[0].path, "/leaf.md");
        assert_eq!(backlinks[0].title.as_deref(), Some("Leaf"));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_concept_looks_through_the_read_set_in_order() {
        let test = app_with(
            &[
                (
                    "pg",
                    &[
                        ("only-pg.md", LEAF),
                        ("index.md", "# Index\n"),
                        ("twice.md", "---\ntitle: A\ntitle: B\n---\nBody\n"),
                    ],
                ),
                (
                    "work",
                    &[("hub.md", HUB), ("only-pg.md", HUB), ("index.md.md", LEAF)],
                ),
            ],
            Config::default(),
        )
        .await;
        let ctx = context(&test, principal(&[], None), Some(&["pg", "work"]));

        let found = get_concept(&ctx, concept_params("/hub.md")).await.unwrap();
        let first = get_concept(&ctx, concept_params("only-pg")).await.unwrap();
        let missing = get_concept(&ctx, concept_params("nowhere"))
            .await
            .unwrap_err();
        let not_a_concept = get_concept(&ctx, concept_params("index"))
            .await
            .unwrap_err();
        // `index.md` is refused in `pg`, but `resolve_concept` also tries
        // `index.md.md`, which `work` holds as a concept: the search goes on.
        let later = get_concept(&ctx, concept_params("index.md")).await.unwrap();
        let invalid = get_concept(&ctx, concept_params("twice")).await.unwrap();
        let mut pinned = concept_params("hub");
        pinned.catalog = Some("pg".to_string());
        let not_in_pg = get_concept(&ctx, pinned).await.unwrap_err();

        assert_eq!(found.catalog, "work");
        assert!(found.links.is_none() && found.backlinks.is_none() && found.raw.is_none());
        // Fields not asked for are left out of the JSON, not sent as null.
        let json = serde_json::to_value(&found).unwrap();
        assert!(["raw", "links", "backlinks"]
            .iter()
            .all(|key| json.get(key).is_none()));
        assert_eq!(first.catalog, "pg");
        assert_eq!(first.title.as_deref(), Some("Leaf"));
        assert!(matches!(missing, OkfError::NotFound { .. }));
        assert!(matches!(not_a_concept, OkfError::InvalidPath { .. }));
        assert_eq!(
            (later.catalog.as_str(), later.path.as_str()),
            ("work", "/index.md.md")
        );
        assert_eq!(invalid.frontmatter, serde_json::Value::Null);
        assert!(invalid.frontmatter_error.is_some());
        assert_eq!(invalid.body, "Body\n");
        assert!(matches!(not_in_pg, OkfError::NotFound { .. }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn read_document_reads_indexes_logs_and_agents_only() {
        let test = app_with(
            &[(
                "pg",
                &[
                    ("index.md", "# Index\n"),
                    ("infra/index.md", "# Infra\n"),
                    ("log.md", "# Log\n"),
                    ("AGENTS.md", "# Agents\n"),
                    ("hub.md", HUB),
                ],
            )],
            Config::default(),
        )
        .await;
        let ctx = context(&test, principal(&[], Some("pg")), None);
        // A closure that captures `ctx` by reference, so each call below is
        // one line.
        let read = |path: Option<&str>| {
            read_document(
                &ctx,
                ReadDocumentParams {
                    path: path.map(str::to_string),
                    catalog: None,
                },
            )
        };

        let root = read(None).await.unwrap();

        assert_eq!(
            (root.catalog.as_str(), root.path.as_str()),
            ("pg", "/index.md")
        );
        assert_eq!(root.text, "# Index\n");
        assert_eq!(
            read(Some("/infra/index.md")).await.unwrap().text,
            "# Infra\n"
        );
        assert_eq!(read(Some("log.md")).await.unwrap().text, "# Log\n");
        assert_eq!(read(Some("/AGENTS.md")).await.unwrap().text, "# Agents\n");
        assert!(matches!(
            read(Some("/hub.md")).await,
            Err(OkfError::InvalidPath { .. })
        ));
        assert!(matches!(
            read(Some("/../index.md")).await,
            Err(OkfError::InvalidPath { .. })
        ));
        assert!(matches!(
            read(Some("/nope/index.md")).await,
            Err(OkfError::NotFound { .. })
        ));
    }

    #[test]
    fn the_server_lists_both_tools_with_object_output_schemas() {
        let tools = OkfServer::tools().list_all();

        for name in ["get_concept", "read_document"] {
            let listed = tools
                .iter()
                .find(|tool| tool.name == name)
                .unwrap_or_else(|| panic!("{name} is registered"));
            let schema = listed.output_schema.as_ref().expect("an output schema");
            assert_eq!(schema["type"], "object", "{name}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_concept_sees_a_pending_edit_only_on_the_key_and_label_that_made_it() {
        let test = app_with(&[("pg", &[("hub.md", HUB)])], Config::default()).await;
        store_key(&test, "alice").await;
        store_key(&test, "bob").await;
        let alice = || {
            context(
                &test,
                principal_as("alice", Access::Write, &[], Some("pg")),
                None,
            )
        };
        let bob = context(
            &test,
            principal_as("bob", Access::Write, &[], Some("pg")),
            None,
        );
        let edited = HUB.replace("The hub.", "The edited hub.");
        let edit = PendingEdit {
            catalog: "pg",
            path: "hub.md",
            text: Some(&edited),
            at: Utc::now(),
        };
        pending_entry(&test, &alice(), &edit).await;

        let mine = get_concept(&alice(), concept_params("hub")).await.unwrap();
        let other_label = get_concept(&alice().with_label("other"), concept_params("hub"))
            .await
            .unwrap();
        let other_key = get_concept(&bob, concept_params("hub")).await.unwrap();

        assert_eq!(mine.frontmatter["description"], "The edited hub.");
        assert_eq!(other_label.frontmatter["description"], "The hub.");
        assert_eq!(other_key.frontmatter["description"], "The hub.");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_broken_overlay_is_reported_not_hidden_behind_the_published_state() {
        let test = app_with(&[("pg", &[("hub.md", HUB)])], Config::default()).await;
        store_key(&test, "alice").await;
        let alice = || {
            context(
                &test,
                principal_as("alice", Access::Write, &[], Some("pg")),
                None,
            )
        };
        // `hub.md` is a published file, so a pending `hub.md/x.md` cannot be
        // applied over it.
        let clash = PendingEdit {
            catalog: "pg",
            path: "hub.md/x.md",
            text: Some("# X\n"),
            at: Utc::now(),
        };
        let changeset = pending_entry(&test, &alice(), &clash).await;

        let error = get_concept(&alice(), concept_params("hub"))
            .await
            .unwrap_err();

        // Falling back to the published `hub.md` would hide alice's own
        // pending edits; the error says which changeset to fix, and how.
        let message = error.to_string();
        assert_eq!(error.kind(), "InvalidPath");
        assert!(
            message.contains(&format!("in pending changeset {}", changeset.id)),
            "{message}"
        );
        assert!(message.contains("discard the clashing path"), "{message}");
    }
}

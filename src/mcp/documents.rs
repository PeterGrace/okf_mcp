//! `get_concept` and `read_document`: one document, from the first selected
//! catalog that has it (Decision 1).

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
use crate::mcp::context::{is_broken_overlay, require_selected, Pinned, ToolContext};
use crate::mcp::{reply, OkfServer};
use crate::okf::outline::{Outline, OutlineHeading};
use crate::okf::paths::{basename, escapes_root, normpath};
use crate::okf::snapshot::{Frontmatter, Snapshot, SnapshotDoc};
use crate::okf::yaml::Value;
use crate::overlay::View;

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
    /// Also return the file verbatim, when the body is within the server's
    /// limit or with `full`.
    #[serde(default)]
    pub raw: bool,
    /// Return only this section, its subsections included: a heading's text
    /// (case-insensitive) or its anchor slug, as in a link's `#fragment`.
    /// Works at any size.
    pub section: Option<String>,
    /// Return the whole body (and `raw`) even when it is over the server's
    /// limit, for a deliberate full read.
    #[serde(default)]
    pub full: bool,
    /// Read through this changeset's drafts (its handle); its catalog must be one this call reads.
    pub changeset: Option<String>,
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
    /// Lines in the body (the text after the frontmatter).
    pub lines: usize,
    /// Characters (Unicode code points) in the body; over the server's
    /// limit, the body is outlined unless `full` is passed.
    pub chars: usize,
    /// The body is over the server's limit and `full` was not passed: it is
    /// not returned whole (`outline` instead, or only the `section`), and
    /// `raw` is left out.
    pub truncated: bool,
    /// How to read more, when `truncated`.
    // Left out of the JSON entirely (not `null`) when there is none.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
    /// The body; with `section`, only that section. Absent when outlined.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    /// Every heading with its slug, level, line and section size, in place
    /// of a body that is over the limit.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub outline: Option<Vec<OutlineHeading>>,
    /// The section returned (with `section`).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub section: Option<OutlineHeading>,
    /// The file verbatim (with `raw`, within the limit or with `full`).
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
    /// Read through this changeset's drafts (its handle); its catalog must be one this call reads.
    pub changeset: Option<String>,
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
/// * `params` - the call's arguments (`links`, `raw`, `section`, `full`).
/// * `limit` - the server's outline threshold, in characters of the body.
///
/// # Returns
/// The concept, with links and the raw text only when asked for. A body
/// over `limit` is replaced by its outline (and `raw` left out) unless
/// `full`; with `section`, the body is that section at any size.
///
/// # Errors
/// [`OkfError::Refused`] if `section` names no heading (the message lists
/// them) or several (it lists their slugs).
fn concept(
    catalog: &str,
    snapshot: &Snapshot,
    doc: &SnapshotDoc,
    params: &GetConceptParams,
    limit: usize,
) -> Result<Concept, OkfError> {
    let whole = doc.doc().body();
    let chars = whole.chars().count();
    // `str::lines`, as `search` counts: a final unterminated line counts.
    let lines = whole.lines().count();
    let within = params.full || chars <= limit;
    let (body, section, outline) = match params.section.as_deref() {
        Some(want) => {
            let (entry, text) = Outline::of(doc.text()).section(want)?;
            (Some(text), Some(entry), None)
        }
        None if within => (Some(whole), None, None),
        None => (None, None, Some(Outline::of(doc.text()).entries())),
    };
    let note = (!within).then(|| {
        let returned = if section.is_some() {
            "only the section is returned"
        } else {
            "only its outline is returned"
        };
        format!(
            "The body is {chars} characters, over this server's limit of {limit}, so \
             {returned}, without `raw`. Pass `section` (a heading's text or slug from the \
             outline) to read one section, or `full: true` for the whole body. To add to it, \
             edit_document with an `old_str` anchored on a heading. A concept this large may \
             be worth splitting (split_concept)."
        )
    });
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
    Ok(Concept {
        catalog: catalog.to_string(),
        path: format!("/{}", doc.path()),
        title: doc.title().map(str::to_string),
        frontmatter,
        frontmatter_error,
        lines,
        chars,
        truncated: !within,
        note,
        body,
        outline,
        section,
        raw: (params.raw && within).then(|| doc.raw()),
        links,
        backlinks,
    })
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
/// A selection error; as [`ToolContext::pin`] for `changeset`, and
/// [`OkfError::Refused`] if its catalog is not in the read set;
/// [`OkfError::NotFound`] if no selected catalog has it;
/// [`OkfError::InvalidPath`] if no selected catalog has it as a concept and
/// one holds a document at that path that is not a concept; the first
/// catalog's overlay error ([`is_broken_overlay`]) if no catalog resolves
/// it and one's pending changeset does not apply; any other error from
/// [`ToolContext::view`] (a database error, `Busy`), at once; as
/// [`concept`] for `section`.
pub(crate) async fn get_concept(
    ctx: &ToolContext,
    params: GetConceptParams,
) -> Result<Concept, OkfError> {
    let mut refused = None;
    let mut broken = None;
    let (states, pinned) =
        read_set(ctx, params.catalog.as_deref(), params.changeset.as_deref()).await?;
    for state in states {
        // The pinned changeset's overlay in its own catalog, else published.
        let Some(view) = view_or_note(ctx, &state, pinned.as_ref(), &mut broken).await? else {
            continue;
        };
        let snapshot = view.snapshot();
        match snapshot.resolve_concept(&params.path) {
            Ok(path) => {
                let doc = snapshot
                    .get(&path)
                    .expect("resolve_concept returns only paths the snapshot holds");
                let limit = ctx.app().config().concept_outline_chars;
                return concept(state.name(), snapshot, doc, &params, limit);
            }
            Err(OkfError::NotFound { .. }) => {}
            // Keep looking, but report this if nothing better turns up.
            // `get_or_insert` keeps the first such error.
            Err(error) => {
                refused.get_or_insert(error);
            }
        }
    }
    // A broken overlay first: the concept may be among its pending edits.
    Err(broken
        .or(refused)
        .unwrap_or(OkfError::NotFound { path: params.path }))
}

/// A read tool's read set and its pinned changeset, refusing a changeset
/// whose catalog is not in the read set.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `catalog` - the call's `catalog` argument.
/// * `changeset` - the call's `changeset` argument.
///
/// # Returns
/// The catalogs to read, in order, and the pinned changeset, if any.
///
/// # Errors
/// A selection error; as [`ToolContext::pin`]; as [`require_selected`].
async fn read_set(
    ctx: &ToolContext,
    catalog: Option<&str>,
    changeset: Option<&str>,
) -> Result<(Vec<Arc<CatalogState>>, Option<Pinned>), OkfError> {
    let states = ctx.read_set(catalog)?;
    let pinned = ctx.pin(changeset).await?;
    require_selected(pinned.as_ref(), &states)?;
    Ok((states, pinned))
}

/// The call's view of one catalog of a read set, or `None` if its overlay
/// does not apply; the first such error is kept in `broken`, so a later
/// catalog can still answer and the error is reported if none does.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `state` - the catalog.
/// * `pinned` - the call's changeset, if any.
/// * `broken` - the first broken overlay's error so far.
///
/// # Returns
/// The view, or `None` to skip the catalog.
///
/// # Errors
/// Any error from [`ToolContext::view`] but a broken overlay (a database
/// error, `Busy`): the call fails at once.
async fn view_or_note(
    ctx: &ToolContext,
    state: &CatalogState,
    pinned: Option<&Pinned>,
    broken: &mut Option<OkfError>,
) -> Result<Option<View>, OkfError> {
    match ctx.view(state, pinned).await {
        Ok(view) => Ok(Some(view)),
        Err(error) if is_broken_overlay(&error) => {
            // `get_or_insert` keeps the first error.
            broken.get_or_insert(error);
            Ok(None)
        }
        Err(error) => Err(error),
    }
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
/// A selection error; as [`get_concept`] for `changeset`;
/// [`OkfError::InvalidPath`] for any other path or one outside the catalog; [`OkfError::NotFound`] if no selected catalog has it,
/// or the first catalog's overlay error if one's pending changeset does not
/// apply ([`is_broken_overlay`]); any other error from
/// [`ToolContext::view`] (a database error, `Busy`), at once.
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
    let mut broken = None;
    let (states, pinned) =
        read_set(ctx, params.catalog.as_deref(), params.changeset.as_deref()).await?;
    for state in states {
        let Some(view) = view_or_note(ctx, &state, pinned.as_ref(), &mut broken).await? else {
            continue;
        };
        if let Some(doc) = view.snapshot().get(&path) {
            return Ok(Document {
                catalog: state.name().to_string(),
                path: format!("/{path}"),
                text: doc.raw(),
            });
        }
    }
    Err(broken.unwrap_or(OkfError::NotFound { path: asked }))
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
        description = "Get a concept: its frontmatter as JSON, its body and the body's size \
                       (`lines`, `chars`). A body over the server's limit (20,000 characters \
                       by default) comes back as an `outline` instead, with `truncated: true` \
                       and a `note`: each heading's text, slug, level, line and section size. \
                       `section` (a heading's text or slug) returns just that section with its \
                       subsections, at any size; `full: true` returns the whole body anyway. \
                       With `links`, its resolved outbound links (target, title, description, \
                       broken) and its backlinks; with `raw`, the file verbatim, but only \
                       within the limit or with `full`. Without `catalog`, the first selected \
                       catalog that has it answers. With `changeset` (a handle from \
                       open_changeset), its catalog reads through that changeset's drafts."
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
                       answers. With `changeset` (a handle from open_changeset), its catalog \
                       reads through that changeset's drafts."
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
        app_with, context, open, pending_entry, principal, principal_as, store_key, PendingEdit,
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
            section: None,
            full: false,
            changeset: None,
        }
    }

    /// `concept_params`, reading through the changeset `handle` names.
    fn through(path: &str, handle: &str) -> GetConceptParams {
        GetConceptParams {
            changeset: Some(handle.to_string()),
            ..concept_params(path)
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
            concept.body.as_deref(),
            Some("See [leaf](leaf.md) and [gone](gone.md). [Top](#hub).\n")
        );
        assert_eq!((concept.lines, concept.chars), (1, 54));
        assert!(!concept.truncated && concept.note.is_none() && concept.outline.is_none());
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
        assert!(["raw", "links", "backlinks", "note", "outline", "section"]
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
        assert_eq!(invalid.body.as_deref(), Some("Body\n"));
        assert!(matches!(not_in_pg, OkfError::NotFound { .. }));
    }

    /// A body with a repeated heading, and the frontmatter before it.
    const BIG_HEAD: &str = "---\ntype: Note\ntitle: Big\ndescription: A big one.\n---\n";
    const BIG_BODY: &str = "# Big\n\nIntro.\n\n## Notes\nFirst.\n\n### Detail\nDeep.\n\n\
                            ## Notes\nSecond.\n\n## Last\nEnd.\n";

    /// A server whose outline limit is `limit`, with `BIG_HEAD` and
    /// `BIG_BODY` published as `pg`'s `big.md`.
    async fn big_server(limit: usize) -> crate::mcp::testing::TestApp {
        let config = Config {
            concept_outline_chars: limit,
            ..Config::default()
        };
        let text = format!("{BIG_HEAD}{BIG_BODY}");
        app_with(&[("pg", &[("big.md", text.as_str())])], config).await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_body_at_the_limit_is_whole_and_one_character_over_is_outlined() {
        let at = big_server(BIG_BODY.len()).await;
        let over = big_server(BIG_BODY.len() - 1).await;
        let mut params = concept_params("big");
        params.raw = true;

        let whole = get_concept(
            &context(&at, principal(&[], Some("pg")), None),
            params.clone(),
        )
        .await
        .unwrap();
        let outlined = get_concept(&context(&over, principal(&[], Some("pg")), None), params)
            .await
            .unwrap();

        assert_eq!(whole.body.as_deref(), Some(BIG_BODY));
        assert_eq!(whole.raw, Some(format!("{BIG_HEAD}{BIG_BODY}")));
        assert!(!whole.truncated && whole.outline.is_none() && whole.note.is_none());
        assert_eq!((whole.lines, whole.chars), (15, BIG_BODY.len()));
        // Over the limit: the frontmatter, title and size, and an outline
        // in place of the body and `raw`.
        assert!(outlined.truncated);
        assert_eq!((outlined.body, outlined.raw), (None, None));
        assert_eq!(outlined.title.as_deref(), Some("Big"));
        assert_eq!(outlined.frontmatter["description"], "A big one.");
        assert_eq!((outlined.lines, outlined.chars), (15, BIG_BODY.len()));
        let outline: Vec<(String, usize, usize, usize)> = outlined
            .outline
            .unwrap()
            .into_iter()
            .map(|h| (h.slug, h.level, h.line, h.lines))
            .collect();
        assert_eq!(
            outline,
            [
                ("big".to_string(), 1, 6, 15),
                ("notes".to_string(), 2, 10, 5),
                ("detail".to_string(), 3, 13, 2),
                ("notes-1".to_string(), 2, 16, 2),
                ("last".to_string(), 2, 19, 2),
            ]
        );
        let note = outlined.note.unwrap();
        assert!(
            note.contains(&format!(
                "over this server's limit of {}",
                BIG_BODY.len() - 1
            )) && note.contains("`section`")
                && note.contains("`full: true`"),
            "{note}"
        );
        assert!(outlined.section.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn full_returns_the_whole_body_and_raw_over_the_limit() {
        let test = big_server(10).await;
        let ctx = context(&test, principal(&[], Some("pg")), None);
        let mut params = concept_params("big");
        params.full = true;
        params.raw = true;

        let concept = get_concept(&ctx, params).await.unwrap();

        assert_eq!(concept.body.as_deref(), Some(BIG_BODY));
        assert_eq!(concept.raw, Some(format!("{BIG_HEAD}{BIG_BODY}")));
        assert!(!concept.truncated && concept.outline.is_none() && concept.note.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn section_by_text_or_slug_returns_it_with_its_subsections() {
        let test = big_server(10).await;
        let ctx = context(&test, principal(&[], Some("pg")), None);
        let section = |want: &str| GetConceptParams {
            section: Some(want.to_string()),
            raw: true,
            ..concept_params("big")
        };

        let by_slug = get_concept(&ctx, section("notes")).await.unwrap();
        let second = get_concept(&ctx, section("#notes-1")).await.unwrap();
        let by_text = get_concept(&ctx, section("## LAST")).await.unwrap();
        let repeated = get_concept(&ctx, section("Notes")).await.unwrap_err();
        let unknown = get_concept(&ctx, section("Nowhere")).await.unwrap_err();

        assert_eq!(
            by_slug.body.as_deref(),
            Some("## Notes\nFirst.\n\n### Detail\nDeep.\n")
        );
        let entry = by_slug.section.as_ref().unwrap();
        assert_eq!((entry.slug.as_str(), entry.line), ("notes", 10));
        // Over the limit, a section still comes back, but `raw` does not.
        assert!(by_slug.truncated && by_slug.raw.is_none() && by_slug.outline.is_none());
        assert!(by_slug
            .note
            .unwrap()
            .contains("only the section is returned"));
        assert_eq!(second.body.as_deref(), Some("## Notes\nSecond.\n"));
        assert_eq!(by_text.body.as_deref(), Some("## Last\nEnd.\n"));
        assert_eq!(repeated.kind(), "Refused");
        assert!(
            repeated
                .to_string()
                .contains("pass the anchor slug instead: notes, notes-1"),
            "{repeated}"
        );
        assert_eq!(unknown.kind(), "Refused");
        let listed = unknown.to_string();
        assert!(
            listed.contains("no heading 'Nowhere'") && listed.contains("### Detail"),
            "{listed}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn section_within_the_limit_keeps_raw() {
        let test = big_server(BIG_BODY.len()).await;
        let ctx = context(&test, principal(&[], Some("pg")), None);
        let params = GetConceptParams {
            section: Some("last".to_string()),
            raw: true,
            ..concept_params("big")
        };

        let concept = get_concept(&ctx, params).await.unwrap();

        assert_eq!(concept.body.as_deref(), Some("## Last\nEnd.\n"));
        assert_eq!(concept.raw, Some(format!("{BIG_HEAD}{BIG_BODY}")));
        assert!(!concept.truncated && concept.note.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_outline_and_a_section_read_through_a_changesets_drafts() {
        let test = big_server(10).await;
        store_key(&test, "alice").await;
        let alice = context(
            &test,
            principal_as("alice", Access::Write, &[], Some("pg")),
            None,
        );
        let handle = open(&alice).await;
        let drafted = format!("{BIG_HEAD}{BIG_BODY}## Added\nNew.\n");
        let edit = PendingEdit {
            path: "big.md",
            text: Some(&drafted),
            at: Utc::now(),
        };
        pending_entry(&test, &handle, &edit).await;
        let added = GetConceptParams {
            section: Some("added".to_string()),
            ..through("big", &handle)
        };

        let outlined = get_concept(&alice, through("big", &handle)).await.unwrap();
        let section = get_concept(&alice, added).await.unwrap();
        let published = get_concept(&alice, concept_params("big")).await.unwrap();

        let slugs: Vec<String> = outlined
            .outline
            .unwrap()
            .into_iter()
            .map(|h| h.slug)
            .collect();
        assert_eq!(slugs.last().map(String::as_str), Some("added"));
        assert_eq!(section.body.as_deref(), Some("## Added\nNew.\n"));
        assert!(published
            .outline
            .unwrap()
            .iter()
            .all(|heading| heading.slug != "added"));
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
                    changeset: None,
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
    async fn get_concept_with_a_changeset_sees_its_draft() {
        let test = app_with(&[("pg", &[("hub.md", HUB)])], Config::default()).await;
        store_key(&test, "alice").await;
        store_key(&test, "bob").await;
        let alice = context(
            &test,
            principal_as("alice", Access::Write, &[], Some("pg")),
            None,
        );
        let bob = context(
            &test,
            principal_as("bob", Access::Write, &[], Some("pg")),
            None,
        );
        let handle = open(&alice).await;
        let edited = HUB.replace("The hub.", "The edited hub.");
        let edit = PendingEdit {
            path: "hub.md",
            text: Some(&edited),
            at: Utc::now(),
        };
        pending_entry(&test, &handle, &edit).await;

        let drafted = get_concept(&alice, through("hub", &handle)).await.unwrap();
        let published = get_concept(&alice, concept_params("hub")).await.unwrap();
        let not_bobs = get_concept(&bob, through("hub", &handle))
            .await
            .unwrap_err();

        assert_eq!(drafted.frontmatter["description"], "The edited hub.");
        assert_eq!(published.frontmatter["description"], "The hub.");
        assert_eq!(not_bobs.kind(), "UnknownChangeset");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_concept_refuses_a_changeset_outside_its_read_set() {
        let test = app_with(
            &[("pg", &[("hub.md", HUB)]), ("work", &[("hub.md", HUB)])],
            Config::default(),
        )
        .await;
        store_key(&test, "alice").await;
        let alice = |connection: Option<&[&str]>| {
            context(
                &test,
                principal_as("alice", Access::Write, &[], Some("pg")),
                connection,
            )
        };
        let handle = open(&alice(None)).await;
        let only_work = alice(Some(&["work"]));

        let refused = get_concept(&only_work, through("hub", &handle))
            .await
            .unwrap_err();
        let document = ReadDocumentParams {
            path: None,
            catalog: None,
            changeset: Some(handle.clone()),
        };
        let refused_read = read_document(&only_work, document).await.unwrap_err();

        assert_eq!(refused.kind(), "Refused");
        let message = refused.to_string();
        assert!(
            message.contains(&handle) && message.contains("\"pg\""),
            "{message}"
        );
        assert_eq!(refused_read.to_string(), message);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_broken_overlay_is_reported_not_hidden_behind_the_published_state() {
        let test = app_with(&[("pg", &[("hub.md", HUB)])], Config::default()).await;
        store_key(&test, "alice").await;
        let alice = context(
            &test,
            principal_as("alice", Access::Write, &[], Some("pg")),
            None,
        );
        let handle = open(&alice).await;
        // `hub.md` is a published file, so a pending `hub.md/x.md` cannot be
        // applied over it.
        let clash = PendingEdit {
            path: "hub.md/x.md",
            text: Some("# X\n"),
            at: Utc::now(),
        };
        pending_entry(&test, &handle, &clash).await;

        let error = get_concept(&alice, through("hub", &handle))
            .await
            .unwrap_err();

        // Falling back to the published `hub.md` would hide alice's own
        // pending edits; the error says which changeset to fix, and how.
        let message = error.to_string();
        assert_eq!(error.kind(), "InvalidPath");
        assert!(
            message.contains(&format!("in pending changeset {handle}")),
            "{message}"
        );
        assert!(message.contains("discard the clashing path"), "{message}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_broken_overlay_in_one_catalog_does_not_hide_a_later_catalog() {
        let test = app_with(
            &[
                ("pg", &[("a.md", HUB)]),
                ("work", &[("hub.md", HUB), ("index.md", "# Work\n")]),
            ],
            Config::default(),
        )
        .await;
        store_key(&test, "alice").await;
        let alice = context(
            &test,
            principal_as("alice", Access::Write, &[], None),
            Some(&["pg", "work"]),
        );
        // The write target is `pg`, the first selected catalog.
        let handle = open(&alice).await;
        // A pending `a.md/x.md` cannot be applied over the published `a.md`.
        let clash = PendingEdit {
            path: "a.md/x.md",
            text: Some("# X\n"),
            at: Utc::now(),
        };
        pending_entry(&test, &handle, &clash).await;
        let index = ReadDocumentParams {
            path: None,
            catalog: None,
            changeset: Some(handle.clone()),
        };

        let found = get_concept(&alice, through("hub", &handle)).await.unwrap();
        let document = read_document(&alice, index).await.unwrap();
        let missing = get_concept(&alice, through("nowhere", &handle))
            .await
            .unwrap_err();

        assert_eq!(found.catalog, "work");
        assert_eq!(document.catalog, "work");
        // Nothing resolves `nowhere`: the broken overlay, not `NotFound`.
        assert_eq!(missing.kind(), "InvalidPath");
        let message = missing.to_string();
        assert!(
            message.contains(&format!("in pending changeset {handle}")),
            "{message}"
        );
    }
}

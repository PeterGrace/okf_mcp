//! Browsing under `/api/v1` (web console design, "API": Browse): a catalog's
//! tree, a concept with its body rendered to safe HTML, its links, its
//! history, one revision with its diff, and the non-concept documents.
//!
//! An axum wildcard (`{*path}`) must be the last segment of a route, so the
//! per-concept routes put their kind before the path
//! (`/catalogs/{catalog}/links/{*path}`), not after it as the design wrote
//! (`/concepts/{*path}/links`).

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::auth::Principal;
use crate::catalog::Loaded;
use crate::error::OkfError;
use crate::http::api::context;
use crate::http::AppError;
use crate::mcp::blocking;
use crate::mcp::changesets::unified_diff;
use crate::mcp::documents::{
    get_concept, read_document, Concept, Document, GetConceptParams, InboundLink, OutboundLink,
    ReadDocumentParams,
};
use crate::mcp::history::{history, History, HistoryParams};
use crate::okf::outline::Outline;
use crate::okf::pystr::universal_newlines;
use crate::okf::snapshot::DocKind;
use crate::render::{to_html, Page};
use crate::store::history::content_at;
use crate::store::keys::Access;

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/catalogs/{catalog}/tree", get(tree))
        .route("/catalogs/{catalog}/concepts/{*path}", get(concept))
        .route("/catalogs/{catalog}/links/{*path}", get(links))
        .route("/catalogs/{catalog}/history/{*path}", get(revisions))
        .route(
            "/catalogs/{catalog}/revisions/{revision}/{*path}",
            get(revision),
        )
        .route("/catalogs/{catalog}/documents/{*path}", get(document))
}

/// One document in a catalog's tree.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct TreeDocument {
    /// `/`-prefixed path.
    pub path: String,
    /// `concept`, `index`, `log` or `machinery` (the root `AGENTS.md`).
    pub kind: &'static str,
    /// Its title, if it has one.
    pub title: Option<String>,
}

/// `GET /catalogs/{catalog}/tree`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct Tree {
    /// The catalog.
    pub catalog: String,
    /// Every document the console can open, in the catalog's walk order.
    /// The client builds the folders from the paths.
    pub documents: Vec<TreeDocument>,
}

/// `GET /catalogs/{catalog}/concepts/{*path}`: `get_concept`'s result with
/// the rendered body.
// No `Eq`: `Concept` holds JSON frontmatter, which is only `PartialEq`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct ConceptView {
    /// The concept, as `get_concept` returns it with `raw`; `outline` is
    /// always present (the whole outline, also within the limit).
    // `flatten` writes the concept's fields beside `body_html`.
    #[serde(flatten)]
    pub concept: Concept,
    /// The body as safe HTML ([`crate::render`]); `None` when the body is over
    /// `concept_outline_chars` and `full` was not passed.
    pub body_html: Option<String>,
}

/// `GET /catalogs/{catalog}/links/{*path}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct ConceptLinks {
    /// The catalog.
    pub catalog: String,
    /// `/`-prefixed path of the concept.
    pub path: String,
    /// Its outbound links, in-page links left out.
    pub links: Vec<OutboundLink>,
    /// The documents linking to it.
    pub backlinks: Vec<InboundLink>,
}

/// `GET /catalogs/{catalog}/revisions/{revision}/{*path}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct RevisionView {
    /// The catalog.
    pub catalog: String,
    /// `/`-prefixed path.
    pub path: String,
    /// The revision shown.
    pub revision: i32,
    /// `create`, `update`, `delete` or `import`.
    pub op: String,
    /// The file at this revision; `None` for a deletion or bytes that are not UTF-8.
    pub text: Option<String>,
    /// A unified diff from the previous revision (from nothing for revision 1).
    pub diff: String,
}

/// `GET /catalogs/{catalog}/documents/{*path}`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct DocumentView {
    /// The file, as `read_document` returns it (`catalog`, `path`, `text`).
    #[serde(flatten)]
    pub document: Document,
    /// The body as safe HTML.
    pub body_html: String,
}

/// `?full=` on the concept route.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
struct FullQuery {
    /// Render the whole body even over `concept_outline_chars`.
    // `default`: an absent parameter is `false`.
    #[serde(default)]
    full: bool,
}

/// `?limit=` on the history route.
#[derive(Debug, Clone, Copy, Default, Deserialize)]
struct LimitQuery {
    /// At most this many revisions (as `history`: default 20, at most 200).
    limit: Option<u32>,
}

/// The tree's name for a document kind, or `None` for a file the console
/// cannot open: `read_document` serves only the root `AGENTS.md` among the
/// machinery files.
///
/// # Arguments
/// * `kind` - the document's kind.
/// * `path` - its catalog-relative path.
fn kind_name(kind: DocKind, path: &str) -> Option<&'static str> {
    match kind {
        DocKind::Concept => Some("concept"),
        DocKind::Index => Some("index"),
        DocKind::Log => Some("log"),
        // `bool::then_some(x)` is `Some(x)` when true, else `None`.
        DocKind::Machinery => (path == "AGENTS.md").then_some("machinery"),
    }
}

/// `GET /catalogs/{catalog}/tree`.
///
/// # Errors
/// 404 for a catalog the key does not reach.
async fn tree(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(catalog): Path<String>,
) -> Result<Json<Tree>, AppError> {
    let ctx = context(&app, principal);
    let state = ctx.one(Some(&catalog))?;
    let loaded = state.current();
    let documents = loaded
        .snapshot()
        .docs()
        .iter()
        // `filter_map` keeps the `Some`s; `?` inside the closure skips a doc.
        .filter_map(|doc| {
            Some(TreeDocument {
                path: format!("/{}", doc.path()),
                kind: kind_name(doc.kind(), doc.path())?,
                title: doc.title().map(str::to_string),
            })
        })
        .collect();
    Ok(Json(Tree {
        catalog: state.name().to_string(),
        documents,
    }))
}

/// `GET /catalogs/{catalog}/concepts/{*path}?full=`: `get_concept` with
/// `raw`, plus `body_html` and the whole outline.
///
/// # Errors
/// As `get_concept`: 404 for an unreached catalog or a missing concept, 400
/// for a path that is not a concept.
async fn concept(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path((catalog, path)): Path<(String, String)>,
    Query(query): Query<FullQuery>,
) -> Result<Json<ConceptView>, AppError> {
    let ctx = context(&app, principal);
    let concept = get_concept(
        &ctx,
        GetConceptParams {
            path,
            catalog: Some(catalog),
            links: false,
            raw: true,
            section: None,
            full: query.full,
            changeset: None,
        },
    )
    .await?;
    let loaded = ctx.one(Some(&concept.catalog))?.current();
    // Rendering is CPU work proportional to the body: off the async threads.
    // `move` hands `concept` and `loaded` (an `Arc`) to the blocking thread.
    Ok(Json(blocking(move || with_html(concept, &loaded)).await))
}

/// Adds the rendered body and the whole outline to a concept.
///
/// # Arguments
/// * `concept` - `get_concept`'s result, read with `raw`.
/// * `loaded` - its catalog, for resolving links.
///
/// # Returns
/// The view. `raw` is present exactly when the body is (within the limit, or
/// `full`), so a concept without `raw` keeps `get_concept`'s outline and gets
/// no `body_html`.
fn with_html(mut concept: Concept, loaded: &Loaded) -> ConceptView {
    // `let ... else`: bind the `Some` value or return early.
    let Some(raw) = concept.raw.as_deref() else {
        return ConceptView {
            concept,
            body_html: None,
        };
    };
    // The outline's view of the file: `\r\n` read as `\n`, as the snapshot reads it.
    let text = universal_newlines(raw);
    let page = Page::new(
        &concept.catalog,
        concept.path.trim_start_matches('/'),
        loaded.workspace(),
    );
    let body_html = to_html(&text, &page);
    let outline = Outline::of(&text).entries();
    // The borrows of `concept.raw` (through `text`) end above, so the
    // concept can be changed and moved now.
    concept.outline = Some(outline);
    ConceptView {
        concept,
        body_html: Some(body_html),
    }
}

/// `GET /catalogs/{catalog}/links/{*path}`.
///
/// # Errors
/// As `get_concept`.
async fn links(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path((catalog, path)): Path<(String, String)>,
) -> Result<Json<ConceptLinks>, AppError> {
    let ctx = context(&app, principal);
    let concept = get_concept(
        &ctx,
        GetConceptParams {
            path,
            catalog: Some(catalog),
            links: true,
            raw: false,
            section: None,
            full: false,
            changeset: None,
        },
    )
    .await?;
    Ok(Json(ConceptLinks {
        catalog: concept.catalog,
        path: concept.path,
        // With `links: true` both are `Some`; `unwrap_or_default` keeps the
        // handler total anyway.
        links: concept.links.unwrap_or_default(),
        backlinks: concept.backlinks.unwrap_or_default(),
    }))
}

/// `GET /catalogs/{catalog}/history/{*path}?limit=`: the `history` tool.
///
/// # Errors
/// As `history`: 404 for an unreached catalog or a path without revisions.
async fn revisions(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path((catalog, path)): Path<(String, String)>,
    Query(query): Query<LimitQuery>,
) -> Result<Json<History>, AppError> {
    let ctx = context(&app, principal);
    let found = history(
        &ctx,
        HistoryParams {
            path,
            catalog: Some(catalog),
            limit: query.limit,
        },
    )
    .await?;
    Ok(Json(found))
}

/// `GET /catalogs/{catalog}/revisions/{revision}/{*path}`: the file at a
/// revision and a unified diff against the revision before it.
///
/// # Errors
/// 404 for an unreached catalog, a path without revisions, or a revision
/// the path never had; 400 for a revision that is not a number.
async fn revision(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path((catalog, revision, path)): Path<(String, i32, String)>,
) -> Result<Json<RevisionView>, AppError> {
    // This reads the store directly, so it checks access itself (before the
    // principal moves into the context).
    principal.require(Access::Read, &catalog)?;
    let ctx = context(&app, principal);
    // `history` resolves the path as the History tab spelled it (`.md`
    // optional) and answers 404 for a path that never existed.
    let found = history(
        &ctx,
        HistoryParams {
            path,
            catalog: Some(catalog),
            limit: Some(1),
        },
    )
    .await?;
    let state = ctx.one(Some(&found.catalog))?;
    let stored = found.path.trim_start_matches('/');
    let current = content_at(app.pool(), state.id(), stored, revision)
        .await?
        .ok_or_else(|| OkfError::NotFound {
            path: format!("{} (revision {revision})", found.path),
        })?;
    let previous = if revision > 1 {
        // `and_then`: a missing row or a deletion both mean "no text before".
        content_at(app.pool(), state.id(), stored, revision - 1)
            .await?
            .and_then(|row| row.content)
    } else {
        None
    };
    let diff = unified_diff(stored, previous.as_deref(), current.content.as_deref());
    // `.ok()` turns bytes that are not UTF-8 into `None`, like a deletion.
    let text = current
        .content
        .and_then(|bytes| String::from_utf8(bytes).ok());
    Ok(Json(RevisionView {
        catalog: found.catalog,
        path: found.path,
        revision,
        op: current.op,
        text,
        diff,
    }))
}

/// `GET /catalogs/{catalog}/documents/{*path}`: `read_document` (any
/// folder's `index.md` or `log.md`, the root `AGENTS.md`) with `body_html`.
///
/// # Errors
/// As `read_document`: 400 for any other path, 404 for an unreached catalog
/// or a missing file.
async fn document(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path((catalog, path)): Path<(String, String)>,
) -> Result<Json<DocumentView>, AppError> {
    let ctx = context(&app, principal);
    let document = read_document(
        &ctx,
        ReadDocumentParams {
            path: Some(path),
            catalog: Some(catalog),
            changeset: None,
        },
    )
    .await?;
    let loaded = ctx.one(Some(&document.catalog))?.current();
    let view = blocking(move || {
        // A block, so the borrow of `document` ends before it moves below.
        let body_html = {
            let text = universal_newlines(&document.text);
            let page = Page::new(
                &document.catalog,
                document.path.trim_start_matches('/'),
                loaded.workspace(),
            );
            to_html(&text, &page)
        };
        DocumentView {
            document,
            body_html,
        }
    })
    .await;
    Ok(Json(view))
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use serde_json::{json, Value};

    use crate::archive::files;
    use crate::config::Config;
    use crate::http::api::testing::{app, assert_matrix, keys, send, Row, MAIN};
    use crate::mcp::testing::app_with;
    use crate::store::import::{import, ImportOptions, ImportScope};

    /// The `main` files with `systems/alpha.md`'s last line changed.
    fn edited_main() -> Vec<(&'static str, String)> {
        MAIN.iter()
            .map(|(path, text)| {
                let text = if *path == "systems/alpha.md" {
                    text.replace("Nothing yet.", "Two settings.")
                } else {
                    (*text).to_string()
                };
                (*path, text)
            })
            .collect()
    }

    /// The entry for `path` in a tree response.
    fn entry<'a>(tree: &'a Value, path: &str) -> &'a Value {
        tree["documents"]
            .as_array()
            .expect("documents")
            .iter()
            .find(|doc| doc["path"] == path)
            .unwrap_or_else(|| panic!("{path} in {tree}"))
    }

    #[tokio::test]
    async fn every_browse_route_needs_read_on_its_catalog() {
        let test = app().await;
        let rows: Vec<Row> = [
            "/api/v1/catalogs/main/tree",
            "/api/v1/catalogs/main/concepts/systems/alpha.md",
            "/api/v1/catalogs/main/links/systems/alpha.md",
            "/api/v1/catalogs/main/history/systems/alpha.md",
            "/api/v1/catalogs/main/revisions/1/systems/alpha.md",
            "/api/v1/catalogs/main/documents/systems/index.md",
        ]
        .into_iter()
        .map(|uri| Row {
            method: "GET",
            uri: uri.to_string(),
            body: None,
            read: 200,
            write: 200,
            admin: 200,
            stranger: 404,
        })
        .collect();

        assert_matrix(&test, &rows).await;
    }

    #[tokio::test]
    async fn the_tree_lists_concepts_indexes_the_log_and_agents_with_titles() {
        let test = app().await;
        let keys = keys(&test).await;

        let (status, tree) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/tree",
            Some(&keys.read),
            None,
        )
        .await;

        assert_eq!(status.as_u16(), 200, "{tree}");
        assert_eq!(tree["catalog"], "main");
        assert_eq!(
            entry(&tree, "/systems/alpha.md"),
            &json!({"path": "/systems/alpha.md", "kind": "concept", "title": "Alpha"})
        );
        assert_eq!(entry(&tree, "/systems/index.md")["kind"], "index");
        assert_eq!(entry(&tree, "/log.md")["kind"], "log");
        assert_eq!(entry(&tree, "/AGENTS.md")["kind"], "machinery");
        assert_eq!(tree["documents"].as_array().map(Vec::len), Some(MAIN.len()));
    }

    #[tokio::test]
    async fn a_concept_comes_with_its_rendered_body_raw_text_and_outline() {
        let test = app().await;
        let keys = keys(&test).await;

        let (status, concept) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/concepts/systems/alpha.md",
            Some(&keys.read),
            None,
        )
        .await;

        assert_eq!(status.as_u16(), 200, "{concept}");
        assert_eq!(concept["path"], "/systems/alpha.md");
        assert_eq!(concept["frontmatter"]["title"], "Alpha");
        let html = concept["body_html"].as_str().expect("body_html");
        assert!(
            html.contains(r#"<h1 id="section-alpha" data-slug="alpha">Alpha</h1>"#),
            "{html}"
        );
        assert!(
            html.contains(r##"<a href="#/browse/main/systems/beta.md">Beta</a>"##),
            "{html}"
        );
        assert!(!html.contains("type: system"), "no frontmatter in {html}");
        assert!(concept["raw"]
            .as_str()
            .expect("raw")
            .starts_with("---\ntype: system\n"));
        let slugs: Vec<&str> = concept["outline"]
            .as_array()
            .expect("outline")
            .iter()
            .filter_map(|heading| heading["slug"].as_str())
            .collect();
        assert_eq!(slugs, ["alpha", "config"]);
    }

    #[tokio::test]
    async fn a_body_over_the_limit_is_outlined_until_full_is_asked_for() {
        let config = Config {
            concept_outline_chars: 10,
            ..Config::default()
        };
        let test = app_with(
            &[("main", MAIN), ("other", &[("index.md", "# Other\n")])],
            config,
        )
        .await;
        let keys = keys(&test).await;
        let uri = "/api/v1/catalogs/main/concepts/systems/alpha.md";

        let (_, outlined) = send(&test, "GET", uri, Some(&keys.read), None).await;
        let (_, full) = send(
            &test,
            "GET",
            &format!("{uri}?full=true"),
            Some(&keys.read),
            None,
        )
        .await;

        assert_eq!(outlined["truncated"], true);
        assert_eq!(outlined["body_html"], Value::Null);
        assert!(outlined.get("raw").is_none(), "{outlined}");
        assert_eq!(outlined["outline"][1]["slug"], "config");
        assert_eq!(full["truncated"], false);
        assert!(full["body_html"]
            .as_str()
            .is_some_and(|html| html.contains("Nothing yet.")));
    }

    #[tokio::test]
    async fn links_list_targets_and_backlinks() {
        let test = app().await;
        let keys = keys(&test).await;

        let (_, alpha) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/links/systems/alpha.md",
            Some(&keys.read),
            None,
        )
        .await;
        let (_, beta) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/links/systems/beta.md",
            Some(&keys.read),
            None,
        )
        .await;

        assert_eq!(alpha["links"][0]["target"], "/systems/beta.md");
        assert_eq!(alpha["links"][0]["broken"], false);
        let sources: Vec<&str> = beta["backlinks"]
            .as_array()
            .expect("backlinks")
            .iter()
            .filter_map(|link| link["path"].as_str())
            .collect();
        assert!(sources.contains(&"/systems/alpha.md"), "{beta}");
        assert!(sources.contains(&"/systems/index.md"), "{beta}");
    }

    #[tokio::test]
    async fn history_lists_revisions_and_a_revision_diffs_against_the_one_before() {
        let test = app().await;
        let keys = keys(&test).await;
        let edited = edited_main();
        let entries: Vec<(&str, &str)> = edited
            .iter()
            .map(|(path, text)| (*path, text.as_str()))
            .collect();
        let options = ImportOptions {
            replace: true,
            scope: ImportScope::Everything,
            expect_generation: None,
            actor: "editor",
            now: Utc::now(),
        };
        import(test.app.pool(), "main", files(&entries), &options)
            .await
            .expect("import the edit");

        let (status, listed) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/history/systems/alpha.md",
            Some(&keys.read),
            None,
        )
        .await;
        let (_, second) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/revisions/2/systems/alpha.md",
            Some(&keys.read),
            None,
        )
        .await;
        let (_, first) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/revisions/1/systems/alpha.md",
            Some(&keys.read),
            None,
        )
        .await;
        let (missing, _) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/revisions/9/systems/alpha.md",
            Some(&keys.read),
            None,
        )
        .await;
        let (bad, _) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/revisions/latest/systems/alpha.md",
            Some(&keys.read),
            None,
        )
        .await;

        assert_eq!(status.as_u16(), 200, "{listed}");
        assert_eq!(listed["revisions"][0]["revision"], 2);
        assert_eq!(listed["revisions"][0]["actor"], "editor");
        assert_eq!(second["op"], "import");
        let diff = second["diff"].as_str().expect("diff");
        assert!(
            diff.contains("-Nothing yet.\n") && diff.contains("+Two settings.\n"),
            "{diff}"
        );
        assert!(second["text"]
            .as_str()
            .is_some_and(|text| text.contains("Two settings.")));
        assert!(first["diff"]
            .as_str()
            .is_some_and(|diff| diff.starts_with("--- /dev/null")));
        assert_eq!(missing.as_u16(), 404);
        assert_eq!(bad.as_u16(), 400);
    }

    #[tokio::test]
    async fn documents_render_indexes_and_refuse_concepts() {
        let test = app().await;
        let keys = keys(&test).await;

        let (status, index) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/documents/systems/index.md",
            Some(&keys.read),
            None,
        )
        .await;
        let (concept, _) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/documents/systems/alpha.md",
            Some(&keys.read),
            None,
        )
        .await;
        let (unknown, _) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/concepts/systems/gamma.md",
            Some(&keys.read),
            None,
        )
        .await;

        assert_eq!(status.as_u16(), 200, "{index}");
        assert_eq!(
            index["text"],
            "# Systems\n\n- [Alpha](alpha.md)\n- [Beta](beta.md)\n"
        );
        let html = index["body_html"].as_str().expect("body_html");
        assert!(
            html.contains(r##"href="#/browse/main/systems/alpha.md""##),
            "{html}"
        );
        assert_eq!(concept.as_u16(), 400);
        assert_eq!(unknown.as_u16(), 404);
    }
}

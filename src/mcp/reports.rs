//! The reports: `open_questions`, `oldest`, `sprawl`, `links_report` and
//! `lint`, each on one catalog (Decision 1) and on a blocking thread. Each
//! takes an optional `changeset` handle to read through its drafts (M8
//! Decision 6); the report then defaults to the changeset's catalog.
//!
//! `oldest`, `sprawl`, `links_report` and `lint` answer with `okf.py`'s JSON,
//! the catalog's name added and lists wrapped in an object (every output
//! schema has an object root). `open_questions` numbers the questions that
//! `oldest` with `open_questions_only` already collects.

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
use crate::mcp::context::ToolContext;
use crate::mcp::{blocking, reply, OkfServer};
use crate::okf::analysis::links::{links_report, LinksReport};
use crate::okf::analysis::lint::LintReport;
use crate::okf::analysis::oldest::{oldest, OldestQuery, OldestRow};
use crate::okf::analysis::sprawl::{sprawl, SprawlQuery, SprawlReport};
use crate::overlay::View;

/// `okf.py`'s default `-n` for `oldest` and `sprawl`.
const DEFAULT_N: usize = 5;

/// `open_questions`' arguments: a catalog and a changeset.
// `Deserialize` reads them from the call's JSON; `JsonSchema` gives the tool
// its input schema, with the doc comments as the fields' descriptions.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct CatalogParam {
    /// The catalog (default: the changeset's, else the first selected catalog).
    pub catalog: Option<String>,
    /// Read through this changeset's drafts (its handle); the report then runs on its catalog unless `catalog` names it.
    pub changeset: Option<String>,
}

/// One queued question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Question {
    /// Its 1-based position (what `unquestion` takes).
    pub number: usize,
    /// The question, as Python's `str()` of the entry.
    pub text: String,
}

/// A concept's queued questions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct ConceptQuestions {
    /// `/`-prefixed path.
    pub path: String,
    /// Its title.
    pub title: Option<String>,
    /// In file order.
    pub questions: Vec<Question>,
}

/// `open_questions`' result.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct OpenQuestions {
    /// The catalog.
    pub catalog: String,
    /// Concepts with queued questions, stalest first (as `oldest` orders them).
    pub concepts: Vec<ConceptQuestions>,
}

/// `oldest`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct OldestParams {
    /// The catalog (default: the changeset's, else the first selected catalog).
    pub catalog: Option<String>,
    /// Read through this changeset's drafts (its handle); the report then runs on its catalog unless `catalog` names it.
    pub changeset: Option<String>,

    /// How many concepts (default 5; ignored with `open_questions_only`).
    pub n: Option<usize>,
    /// Only concepts with queued `open_questions`, all of them.
    // `#[serde(default)]` makes the field optional: absent means `false`.
    #[serde(default)]
    pub open_questions_only: bool,
}

/// `oldest`'s result.
// No `Eq`: `OldestRow` holds an `f64`, which is only `PartialEq` (NaN).
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Oldest {
    /// The catalog.
    pub catalog: String,
    /// `okf.py oldest --json`'s rows, stalest first.
    pub rows: Vec<OldestRow>,
}

/// `sprawl`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct SprawlParams {
    /// The catalog (default: the changeset's, else the first selected catalog).
    pub catalog: Option<String>,
    /// Read through this changeset's drafts (its handle); the report then runs on its catalog unless `catalog` names it.
    pub changeset: Option<String>,

    /// One concept to score (default: the worst `n`).
    pub path: Option<String>,
    /// How many concepts (default 5).
    pub n: Option<usize>,
}

/// `sprawl`'s result.
#[derive(Debug, Clone, PartialEq, Serialize, JsonSchema)]
pub struct Sprawl {
    /// The catalog.
    pub catalog: String,
    /// `okf.py sprawl --json`'s reports, worst first.
    pub reports: Vec<SprawlReport>,
}

/// `links_report`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct LinksParams {
    /// The catalog (default: the changeset's, else the first selected catalog).
    pub catalog: Option<String>,
    /// Read through this changeset's drafts (its handle); the report then runs on its catalog unless `catalog` names it.
    pub changeset: Option<String>,

    /// Also look for unlinked mentions of concept names (default true; the slowest part).
    pub mentions: Option<bool>,
}

/// `links_report`'s result: `okf.py links --json` plus the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Links {
    /// The catalog.
    pub catalog: String,
    /// The report; `flatten` writes its fields beside `catalog`.
    #[serde(flatten)]
    pub report: LinksReport,
}

/// `lint`'s arguments.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct LintParams {
    /// The catalog (default: the changeset's, else the first selected catalog).
    pub catalog: Option<String>,
    /// Lint this changeset's drafts (its handle) applied to the published catalog, instead of the published catalog; its changed paths count as uncommitted for `generated-after-commit`.
    pub changeset: Option<String>,
}

/// `lint`'s result: `okf.py lint --json` plus the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Lint {
    /// The catalog.
    pub catalog: String,
    /// Whether a changeset's drafts were linted: the call named `changeset`.
    pub pending: bool,
    /// The findings; `flatten` writes `errors` and `warnings` beside `catalog`.
    #[serde(flatten)]
    pub report: LintReport,
}

/// Every concept's queued questions, numbered from 1.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the catalog, if named.
///
/// # Returns
/// The concepts with questions, in `oldest`'s order (stalest first).
///
/// # Errors
/// A selection error; as [`ToolContext::view`].
pub(crate) async fn open_questions(
    ctx: &ToolContext,
    params: CatalogParam,
) -> Result<OpenQuestions, OkfError> {
    // The view holds an `Arc` handle to the snapshot; `move` hands it to the
    // blocking thread, which must own what it reads.
    let (state, view) =
        one_view(ctx, params.catalog.as_deref(), params.changeset.as_deref()).await?;
    let now = ctx.app().clock().now();
    let query = OldestQuery {
        n: 0,
        open_questions_only: true,
    };
    let rows = blocking(move || oldest(view.snapshot(), now, query)).await;
    let concepts = rows
        .into_iter()
        .map(|row| ConceptQuestions {
            path: row.path,
            title: row.title,
            questions: row
                .open_questions
                .into_iter()
                .enumerate()
                .map(|(index, text)| Question {
                    number: index + 1,
                    text,
                })
                .collect(),
        })
        .collect();
    Ok(OpenQuestions {
        catalog: state.name().to_string(),
        concepts,
    })
}

/// `okf.py oldest`.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the catalog, `n` and `open_questions_only`.
///
/// # Returns
/// The rows, stalest first.
///
/// # Errors
/// A selection error; as [`ToolContext::view`].
pub(crate) async fn oldest_report(
    ctx: &ToolContext,
    params: OldestParams,
) -> Result<Oldest, OkfError> {
    let (state, view) =
        one_view(ctx, params.catalog.as_deref(), params.changeset.as_deref()).await?;
    let now = ctx.app().clock().now();
    let query = OldestQuery {
        n: params.n.unwrap_or(DEFAULT_N),
        open_questions_only: params.open_questions_only,
    };
    let rows = blocking(move || oldest(view.snapshot(), now, query)).await;
    Ok(Oldest {
        catalog: state.name().to_string(),
        rows,
    })
}

/// `okf.py sprawl`.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the catalog, one concept's `path` or `n`.
///
/// # Returns
/// The reports, worst first (one, with `path`).
///
/// # Errors
/// A selection error; as [`ToolContext::view`]; [`OkfError::NotFound`] for
/// an unknown `path`, and [`OkfError::InvalidPath`] for one that is not a
/// concept.
pub(crate) async fn sprawl_report(
    ctx: &ToolContext,
    params: SprawlParams,
) -> Result<Sprawl, OkfError> {
    let (state, view) =
        one_view(ctx, params.catalog.as_deref(), params.changeset.as_deref()).await?;
    let n = params.n.unwrap_or(DEFAULT_N);
    let path = params.path;
    // The trailing `?` re-raises the analysis' error after the thread ends.
    let reports = blocking(move || {
        let query = SprawlQuery {
            concept: path.as_deref(),
            n,
        };
        sprawl(view.snapshot(), query)
    })
    .await?;
    Ok(Sprawl {
        catalog: state.name().to_string(),
        reports,
    })
}

/// `okf.py links`.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the catalog and whether to look for mentions.
///
/// # Returns
/// The link report.
///
/// # Errors
/// A selection error; as [`ToolContext::view`].
pub(crate) async fn links(ctx: &ToolContext, params: LinksParams) -> Result<Links, OkfError> {
    let (state, view) =
        one_view(ctx, params.catalog.as_deref(), params.changeset.as_deref()).await?;
    let mentions = params.mentions.unwrap_or(true);
    let report = blocking(move || links_report(view.snapshot(), mentions)).await;
    Ok(Links {
        catalog: state.name().to_string(),
        report,
    })
}

/// `okf.py lint`, on the published catalog with its commit times, or with
/// `changeset` on that changeset's drafts (whose paths count as
/// uncommitted; [`crate::overlay::Overlay::lint`]).
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the catalog, if named, and the changeset.
///
/// # Returns
/// The errors and warnings, and whether pending changes were linted.
///
/// # Errors
/// A selection error; as [`ToolContext::view`].
pub(crate) async fn lint_report(ctx: &ToolContext, params: LintParams) -> Result<Lint, OkfError> {
    let (state, view) =
        one_view(ctx, params.catalog.as_deref(), params.changeset.as_deref()).await?;
    let pending = view.pending();
    let now = ctx.app().clock().now();
    let report = blocking(move || view.lint(now)).await;
    Ok(Lint {
        catalog: state.name().to_string(),
        pending,
        report,
    })
}

/// The one catalog a report runs on, and the call's view of it.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `catalog` - the call's `catalog` argument.
/// * `changeset` - the call's `changeset` argument.
///
/// # Returns
/// The catalog ([`ToolContext::report_catalog`]) and its view: the
/// changeset's drafts with `changeset`, else published.
///
/// # Errors
/// As [`ToolContext::pin`]; as [`ToolContext::report_catalog`]; as
/// [`ToolContext::view`].
async fn one_view(
    ctx: &ToolContext,
    catalog: Option<&str>,
    changeset: Option<&str>,
) -> Result<(Arc<CatalogState>, View), OkfError> {
    let pinned = ctx.pin(changeset).await?;
    let state = ctx.report_catalog(catalog, pinned.as_ref())?;
    let view = ctx.view(&state, pinned.as_ref()).await?;
    Ok((state, view))
}

// `#[tool_router]` collects the `#[tool]` methods below into
// `OkfServer::reports_router()`, which `OkfServer::tools` joins with the rest.
#[tool_router(router = reports_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `open_questions` tool.
    #[tool(
        title = "List Open Questions",
        description = "Every queued open_questions entry in a catalog, grouped by concept \
                          and numbered from 1 (the number `unquestion` takes).",
        annotations(
            title = "List Open Questions",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn open_questions(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<CatalogParam>,
    ) -> Result<Json<OpenQuestions>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(open_questions(&ctx, params).await)
    }

    /// The `oldest` tool.
    #[tool(
        title = "Oldest Concepts",
        description = "The stalest concepts (okf.py oldest --json): last stamp, by whom, \
                          age in days, stale flag and queued questions.",
        annotations(
            title = "Oldest Concepts",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn oldest(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<OldestParams>,
    ) -> Result<Json<Oldest>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(oldest_report(&ctx, params).await)
    }

    /// The `sprawl` tool.
    #[tool(
        title = "Sprawl Report",
        description = "Sprawl scores (okf.py sprawl --json): which concepts have grown too \
                          long or volatile, with sections and split candidates.",
        annotations(
            title = "Sprawl Report",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn sprawl(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<SprawlParams>,
    ) -> Result<Json<Sprawl>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(sprawl_report(&ctx, params).await)
    }

    /// The `links_report` tool.
    #[tool(
        title = "Links Report",
        description = "The link report (okf.py links --json): broken links and anchors, \
                          relative links, orphans, dead ends, index drift and (unless \
                          `mentions` is false) unlinked mentions.",
        annotations(
            title = "Links Report",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn links_report(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<LinksParams>,
    ) -> Result<Json<Links>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(links(&ctx, params).await)
    }

    /// The `lint` tool.
    #[tool(
        title = "Lint Catalog",
        description = "Lint a catalog (okf.py lint --json): errors and warnings with file, \
                          line, rule and message. Without `changeset`, the published catalog; \
                          with `changeset` (a handle from open_changeset), that changeset's \
                          drafts applied to it, the changed paths counted as uncommitted.",
        annotations(
            title = "Lint Catalog",
            read_only_hint = true,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn lint(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<LintParams>,
    ) -> Result<Json<Lint>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(lint_report(&ctx, params).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use chrono::{TimeDelta, TimeZone, Utc};

    use crate::clock::FixedClock;
    use crate::config::Config;
    use crate::mcp::testing::{
        app_at, app_with, context, open, pending_entry, principal, principal_as, store_key,
        PendingEdit, TestApp,
    };
    use crate::store::keys::Access;

    const QUEUED: &str =
        "---\ntype: Note\ntitle: Q\nopen_questions:\n  - Is it up?\n  - Who owns it?\n---\nBody.\n";
    const PLAIN: &str = "---\ntype: Note\ntitle: P\n---\nSee [q](q.md) and [gone](gone.md).\n";

    /// `pg` holds a concept with two questions, one with a broken link and an
    /// `index.md` (not a concept); `work` holds one concept.
    async fn test_app() -> TestApp {
        app_with(
            &[
                (
                    "pg",
                    &[("q.md", QUEUED), ("p.md", PLAIN), ("index.md", "# Index\n")],
                ),
                ("work", &[("w.md", PLAIN)]),
            ],
            Config::default(),
        )
        .await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn open_questions_are_numbered_from_one_per_concept() {
        let test = test_app().await;
        let ctx = context(&test, principal(&[], Some("pg")), None);

        let found = open_questions(
            &ctx,
            CatalogParam {
                catalog: None,
                changeset: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(found.catalog, "pg");
        assert_eq!(found.concepts.len(), 1);
        assert_eq!(found.concepts[0].path, "/q.md");
        assert_eq!(found.concepts[0].title.as_deref(), Some("Q"));
        let numbered: Vec<(usize, &str)> = found.concepts[0]
            .questions
            .iter()
            .map(|q| (q.number, q.text.as_str()))
            .collect();
        assert_eq!(numbered, [(1, "Is it up?"), (2, "Who owns it?")]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn reports_run_on_the_first_selected_catalog_unless_one_is_named() {
        let test = test_app().await;
        let ctx = context(&test, principal(&[], None), Some(&["work", "pg"]));

        let default = links(
            &ctx,
            LinksParams {
                catalog: None,
                mentions: None,
                changeset: None,
            },
        )
        .await
        .unwrap();
        let named = links(
            &ctx,
            LinksParams {
                catalog: Some("pg".to_string()),
                mentions: Some(false),
                changeset: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(default.catalog, "work");
        assert_eq!(named.catalog, "pg");
        let json = serde_json::to_value(&named).unwrap();
        assert_eq!(json["catalog"], "pg");
        assert_eq!(json["summary"]["broken"], 1);
        assert_eq!(json["broken"][0]["link"], "gone.md");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn oldest_sprawl_and_lint_answer_with_okf_py_s_shapes() {
        let test = test_app().await;
        let ctx = context(&test, principal(&[], Some("pg")), None);

        let rows = oldest_report(
            &ctx,
            OldestParams {
                catalog: None,
                n: Some(1),
                open_questions_only: false,
                changeset: None,
            },
        )
        .await
        .unwrap();
        let reports = sprawl_report(
            &ctx,
            SprawlParams {
                catalog: None,
                path: Some("q".to_string()),
                n: None,
                changeset: None,
            },
        )
        .await
        .unwrap();
        let missing = sprawl_report(
            &ctx,
            SprawlParams {
                catalog: None,
                path: Some("nowhere".to_string()),
                n: None,
                changeset: None,
            },
        )
        .await;
        let findings = lint_report(
            &ctx,
            LintParams {
                catalog: None,
                changeset: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(rows.catalog, "pg");
        assert_eq!(rows.rows.len(), 1);
        assert_eq!(reports.reports.len(), 1);
        assert_eq!(reports.reports[0].path, "/q.md");
        assert!(matches!(missing, Err(OkfError::NotFound { .. })));
        let json = serde_json::to_value(&findings).unwrap();
        assert_eq!(json["catalog"], "pg");
        assert!(json["errors"].is_array() && json["warnings"].is_array());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_tools_answer_selection_and_path_errors_with_error_results() {
        let test = test_app().await;
        let server = OkfServer::new(std::sync::Arc::clone(&test.app));
        // The request a tool sees, with a key granted only `pg`, as the auth
        // layer would leave it.
        let (mut parts, _) = axum::http::Request::new(()).into_parts();
        parts.extensions.insert(principal(&["pg"], Some("pg")));

        let ungranted = server
            .lint(
                Extension(parts.clone()),
                Parameters(LintParams {
                    catalog: Some("work".to_string()),
                    changeset: None,
                }),
            )
            .await;
        let not_a_concept = server
            .sprawl(
                Extension(parts),
                Parameters(SprawlParams {
                    catalog: None,
                    path: Some("index".to_string()),
                    n: None,
                    changeset: None,
                }),
            )
            .await;

        let ungranted = ungranted.err().expect("an error result");
        assert_eq!(ungranted.is_error, Some(true));
        let body = ungranted.structured_content.expect("a JSON body");
        assert_eq!(body["error"], "CatalogNotSelected");
        assert_eq!(body["catalog"], "work");
        let not_a_concept = not_a_concept.err().expect("an error result");
        assert_eq!(
            not_a_concept.structured_content.expect("a JSON body")["error"],
            "InvalidPath"
        );
    }

    #[test]
    fn every_report_tool_is_listed_with_an_object_output_schema() {
        let tools = OkfServer::reports_router().list_all();
        // rmcp sorts `list_all` (its tests/test_tool_routers.rs asserts it);
        // sorting here keeps the test independent of that.
        let mut names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
        names.sort_unstable();

        assert_eq!(
            names,
            ["links_report", "lint", "oldest", "open_questions", "sprawl"]
        );
        for tool in &tools {
            let schema = tool.output_schema.as_ref().expect("output schema");
            assert_eq!(
                schema.get("type"),
                Some(&serde_json::json!("object")),
                "{}",
                tool.name
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn lint_counts_only_the_pinned_changeset_s_paths_as_uncommitted() {
        let imported = Utc.with_ymd_and_hms(2026, 11, 3, 12, 0, 0).unwrap();
        // Stamped after the import: `generated-after-commit` on a clean file.
        let stamped =
            "---\ntype: Note\ngenerated: { by: t, at: 2026-11-03T12:10:00Z }\n---\nBody\n";
        let clock = Arc::new(FixedClock(imported + TimeDelta::minutes(30)));
        let test = app_at(
            &[("pg", &[("later.md", stamped), ("other.md", stamped)])],
            Config::default(),
            imported,
            clock,
        )
        .await;
        store_key(&test, "writer").await;
        let ctx = context(
            &test,
            principal_as("writer", Access::Write, &[], Some("pg")),
            None,
        );
        let handle = open(&ctx).await;
        let edited = format!("{stamped}More.\n");
        let edit = PendingEdit {
            path: "later.md",
            text: Some(&edited),
            at: imported + TimeDelta::minutes(20),
        };
        pending_entry(&test, &handle, &edit).await;
        let params = |changeset: Option<&str>| LintParams {
            catalog: None,
            changeset: changeset.map(str::to_string),
        };

        let published = lint_report(&ctx, params(None)).await.unwrap();
        let pending = lint_report(&ctx, params(Some(&handle))).await.unwrap();

        let flagged = |lint: &Lint| -> Vec<String> {
            lint.report
                .warnings
                .iter()
                .filter(|finding| finding.rule == "generated-after-commit")
                .map(|finding| finding.file.clone())
                .collect()
        };
        assert!(!published.pending);
        assert!(pending.pending);
        assert_eq!(flagged(&published), ["later.md", "other.md"]);
        assert_eq!(flagged(&pending), ["other.md"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_report_defaults_to_the_pinned_changeset_s_catalog() {
        let test = test_app().await;
        store_key(&test, "writer").await;
        let in_pg = context(
            &test,
            principal_as("writer", Access::Write, &[], Some("pg")),
            None,
        );
        // `work` first: without a changeset, reports run on `work`.
        let connection = context(
            &test,
            principal_as("writer", Access::Write, &[], None),
            Some(&["work", "pg"]),
        );
        let handle = open(&in_pg).await;
        let edit = PendingEdit {
            path: "new.md",
            text: Some("---\ntype: Note\ntitle: N\nopen_questions:\n  - Why?\n---\nBody.\n"),
            at: Utc::now(),
        };
        pending_entry(&test, &handle, &edit).await;
        let questions = |catalog: Option<&str>, changeset: Option<&str>| CatalogParam {
            catalog: catalog.map(str::to_string),
            changeset: changeset.map(str::to_string),
        };

        let plain = open_questions(&connection, questions(None, None))
            .await
            .unwrap();
        let pinned = open_questions(&connection, questions(None, Some(&handle)))
            .await
            .unwrap();
        let lint = lint_report(
            &connection,
            LintParams {
                catalog: None,
                changeset: Some(handle.clone()),
            },
        )
        .await
        .unwrap();
        let elsewhere = open_questions(&connection, questions(Some("work"), Some(&handle)))
            .await
            .unwrap_err();
        // The changeset's catalog stands in for a `catalog` argument, which
        // wins over the connection's catalogs: a `work`-only connection
        // reports on `pg` too.
        let work_only = context(
            &test,
            principal_as("writer", Access::Write, &[], None),
            Some(&["work"]),
        );
        let outside_selection = open_questions(&work_only, questions(None, Some(&handle)))
            .await
            .unwrap();

        assert_eq!(plain.catalog, "work");
        assert_eq!(pinned.catalog, "pg");
        let paths: Vec<&str> = pinned
            .concepts
            .iter()
            .map(|concept| concept.path.as_str())
            .collect();
        assert_eq!(paths, ["/new.md", "/q.md"]);
        assert_eq!((lint.catalog.as_str(), lint.pending), ("pg", true));
        assert_eq!(elsewhere.kind(), "Refused");
        assert!(elsewhere.to_string().contains("\"pg\""), "{elsewhere}");
        assert_eq!(outside_selection.catalog, "pg");
    }
}

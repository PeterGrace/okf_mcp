//! The reports: `open_questions`, `oldest`, `sprawl`, `links_report` and
//! `lint`, each on one catalog (Decision 1) and on a blocking thread.
//!
//! `oldest`, `sprawl`, `links_report` and `lint` answer with `okf.py`'s JSON,
//! the catalog's name added and lists wrapped in an object (every output
//! schema has an object root). `open_questions` numbers the questions that
//! `oldest` with `open_questions_only` already collects.

use axum::http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::CallToolResult;
use rmcp::{tool, tool_router, Json};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

use crate::error::OkfError;
use crate::mcp::context::ToolContext;
use crate::mcp::{blocking, reply, OkfServer};
use crate::okf::analysis::links::{links_report, LinksReport};
use crate::okf::analysis::lint::LintReport;
use crate::okf::analysis::oldest::{oldest, OldestQuery, OldestRow};
use crate::okf::analysis::sprawl::{sprawl, SprawlQuery, SprawlReport};

/// `okf.py`'s default `-n` for `oldest` and `sprawl`.
const DEFAULT_N: usize = 5;

/// A report's only argument.
// `Deserialize` reads it from the call's JSON; `JsonSchema` gives the tool its
// input schema, with the doc comments as the fields' descriptions.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, JsonSchema)]
pub struct CatalogParam {
    /// The catalog (default: the first selected catalog).
    pub catalog: Option<String>,
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
    /// The catalog (default: the first selected catalog).
    pub catalog: Option<String>,
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
    /// The catalog (default: the first selected catalog).
    pub catalog: Option<String>,
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
    /// The catalog (default: the first selected catalog).
    pub catalog: Option<String>,
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

/// `lint`'s result: `okf.py lint --json` plus the catalog.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, JsonSchema)]
pub struct Lint {
    /// The catalog.
    pub catalog: String,
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
/// A selection error.
pub(crate) async fn open_questions(
    ctx: &ToolContext,
    params: CatalogParam,
) -> Result<OpenQuestions, OkfError> {
    let state = ctx.one(params.catalog.as_deref())?;
    // `current()` is an `Arc` handle to the snapshot; `move` hands it to the
    // blocking thread, which must own what it reads.
    let loaded = state.current();
    let now = ctx.app().clock().now();
    let query = OldestQuery {
        n: 0,
        open_questions_only: true,
    };
    let rows = blocking(move || oldest(loaded.snapshot(), now, query)).await;
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
/// A selection error.
pub(crate) async fn oldest_report(
    ctx: &ToolContext,
    params: OldestParams,
) -> Result<Oldest, OkfError> {
    let state = ctx.one(params.catalog.as_deref())?;
    let loaded = state.current();
    let now = ctx.app().clock().now();
    let query = OldestQuery {
        n: params.n.unwrap_or(DEFAULT_N),
        open_questions_only: params.open_questions_only,
    };
    let rows = blocking(move || oldest(loaded.snapshot(), now, query)).await;
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
/// A selection error; [`OkfError::NotFound`] for an unknown `path`, and
/// [`OkfError::InvalidPath`] for one that is not a concept.
pub(crate) async fn sprawl_report(
    ctx: &ToolContext,
    params: SprawlParams,
) -> Result<Sprawl, OkfError> {
    let state = ctx.one(params.catalog.as_deref())?;
    let loaded = state.current();
    let n = params.n.unwrap_or(DEFAULT_N);
    let path = params.path;
    // The trailing `?` re-raises the analysis' error after the thread ends.
    let reports = blocking(move || {
        let query = SprawlQuery {
            concept: path.as_deref(),
            n,
        };
        sprawl(loaded.snapshot(), query)
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
/// A selection error.
pub(crate) async fn links(ctx: &ToolContext, params: LinksParams) -> Result<Links, OkfError> {
    let state = ctx.one(params.catalog.as_deref())?;
    let loaded = state.current();
    let mentions = params.mentions.unwrap_or(true);
    let report = blocking(move || links_report(loaded.snapshot(), mentions)).await;
    Ok(Links {
        catalog: state.name().to_string(),
        report,
    })
}

/// `okf.py lint`, on the published catalog with its commit times.
///
/// # Arguments
/// * `ctx` - the call's context.
/// * `params` - the catalog, if named.
///
/// # Returns
/// The errors and warnings.
///
/// # Errors
/// A selection error.
pub(crate) async fn lint_report(ctx: &ToolContext, params: CatalogParam) -> Result<Lint, OkfError> {
    let state = ctx.one(params.catalog.as_deref())?;
    let loaded = state.current();
    let now = ctx.app().clock().now();
    let report = blocking(move || loaded.lint(now)).await;
    Ok(Lint {
        catalog: state.name().to_string(),
        report,
    })
}

// `#[tool_router]` collects the `#[tool]` methods below into
// `OkfServer::reports_router()`, which `OkfServer::tools` joins with the rest.
#[tool_router(router = reports_router, vis = "pub(crate)")]
impl OkfServer {
    /// The `open_questions` tool.
    #[tool(
        description = "Every queued open_questions entry in a catalog, grouped by concept \
                          and numbered from 1 (the number `unquestion` takes)."
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
        description = "The stalest concepts (okf.py oldest --json): last stamp, by whom, \
                          age in days, stale flag and queued questions."
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
        description = "Sprawl scores (okf.py sprawl --json): which concepts have grown too \
                          long or volatile, with sections and split candidates."
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
        description = "The link report (okf.py links --json): broken links and anchors, \
                          relative links, orphans, dead ends, index drift and (unless \
                          `mentions` is false) unlinked mentions."
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
        description = "Lint the published catalog (okf.py lint --json): errors and warnings \
                          with file, line, rule and message."
    )]
    async fn lint(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(params): Parameters<CatalogParam>,
    ) -> Result<Json<Lint>, CallToolResult> {
        let ctx = self.context(&parts)?;
        reply(lint_report(&ctx, params).await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use crate::mcp::testing::{app_with, context, principal, TestApp};

    const QUEUED: &str =
        "---\ntype: Note\ntitle: Q\nopen_questions:\n  - Is it up?\n  - Who owns it?\n---\nBody.\n";
    const PLAIN: &str = "---\ntype: Note\ntitle: P\n---\nSee [q](q.md) and [gone](gone.md).\n";

    /// `pg` holds a concept with two questions and one with a broken link;
    /// `work` holds one concept.
    async fn test_app() -> TestApp {
        app_with(
            &[
                ("pg", &[("q.md", QUEUED), ("p.md", PLAIN)]),
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

        let found = open_questions(&ctx, CatalogParam { catalog: None })
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
            },
        )
        .await
        .unwrap();
        let named = links(
            &ctx,
            LinksParams {
                catalog: Some("pg".to_string()),
                mentions: Some(false),
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
            },
        )
        .await;
        let findings = lint_report(&ctx, CatalogParam { catalog: None })
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
}

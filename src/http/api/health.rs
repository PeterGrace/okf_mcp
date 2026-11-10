//! `/api/v1/catalogs/{catalog}/health` and `/api/v1/catalogs/{catalog}/reports/{report}`
//! (web console design, "API": Health). The counts and the reports come from
//! the functions behind the `lint`, `sprawl`, `links_report`, `oldest` and
//! `open_questions` tools, so the console never disagrees with them.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::{Deserialize, Serialize};

use crate::app::App;
use crate::auth::Principal;
use crate::error::OkfError;
use crate::http::api;
use crate::http::AppError;
use crate::mcp::reports::{
    self as tools, CatalogParam, Links, LinksParams, Lint, LintParams, Oldest, OldestParams,
    OpenQuestions, Sprawl, SprawlParams,
};
use crate::okf::analysis::oldest::OldestRow;

/// The lint rule `lint` raises for a concept above the sprawl threshold.
const SPRAWL_RULE: &str = "sprawl";
/// Rows the `oldest` and `sprawl` reports return when `?n=` is absent.
const DEFAULT_ROWS: usize = 20;
/// The most rows `?n=` may ask for.
const MAX_ROWS: usize = 200;

/// One catalog's health in numbers, for the console's tiles.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub(crate) struct HealthCounts {
    /// The catalog.
    pub catalog: String,
    /// `lint` errors (they block `publish`).
    pub lint_errors: usize,
    /// `lint` warnings, sprawl included.
    pub lint_warnings: usize,
    /// Concepts above the sprawl threshold (`lint`'s `sprawl` warnings).
    pub sprawl: usize,
    /// Concepts no other concept links to.
    pub orphans: usize,
    /// Links whose target does not exist.
    pub broken_links: usize,
    /// The stalest concept's `oldest` row (`None` without concepts).
    pub oldest: Option<OldestRow>,
    /// Queued `open_questions` entries, over every concept.
    pub open_questions: usize,
}

/// One report, serialized exactly as its tool's result: `untagged` writes the
/// inner value without naming the variant (like a Python union type).
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
pub(crate) enum Report {
    /// `lint`.
    Lint(Lint),
    /// `sprawl`.
    Sprawl(Sprawl),
    /// `links_report`. Boxed: it is several times larger than the other
    /// variants, and an enum is as large as its largest variant
    /// (clippy's `large_enum_variant`).
    Links(Box<Links>),
    /// `oldest`.
    Oldest(Oldest),
    /// `open_questions`.
    Questions(OpenQuestions),
}

/// `?n=` (`oldest`, `sprawl`) and `?mentions=` (`links`).
#[derive(Debug, Deserialize)]
struct ReportQuery {
    /// Rows, clamped to 1..=[`MAX_ROWS`] (default [`DEFAULT_ROWS`]).
    n: Option<usize>,
    /// Look for unlinked mentions (default true, as the tool).
    mentions: Option<bool>,
}

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/catalogs/{catalog}/health", get(health))
        .route("/catalogs/{catalog}/reports/{report}", get(report))
}

/// `GET /catalogs/{catalog}/health`: the tiles' numbers.
///
/// # Errors
/// 404 for a catalog that does not exist or that the key does not reach.
async fn health(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(catalog): Path<String>,
) -> Result<Json<HealthCounts>, AppError> {
    let ctx = api::context(&app, principal);
    let named = Some(catalog);
    // `try_join!` polls the four reports at once (each runs on its own
    // blocking thread) and returns the first error, like
    // `asyncio.gather` that cancels on failure. Mentions are skipped: they
    // are the slowest part and no count uses them.
    let (lint, links, oldest, questions) = tokio::try_join!(
        tools::lint_report(
            &ctx,
            LintParams {
                catalog: named.clone(),
                changeset: None
            }
        ),
        tools::links(
            &ctx,
            LinksParams {
                catalog: named.clone(),
                changeset: None,
                mentions: Some(false)
            }
        ),
        tools::oldest_report(
            &ctx,
            OldestParams {
                catalog: named.clone(),
                changeset: None,
                n: Some(1),
                open_questions_only: false,
            }
        ),
        tools::open_questions(
            &ctx,
            CatalogParam {
                catalog: named,
                changeset: None
            }
        ),
    )?;
    let sprawl = lint
        .report
        .warnings
        .iter()
        .filter(|finding| finding.rule == SPRAWL_RULE)
        .count();
    Ok(Json(HealthCounts {
        catalog: lint.catalog,
        lint_errors: lint.report.errors.len(),
        lint_warnings: lint.report.warnings.len(),
        sprawl,
        orphans: links.report.summary.orphans,
        broken_links: links.report.summary.broken,
        oldest: oldest.rows.into_iter().next(),
        open_questions: questions
            .concepts
            .iter()
            .map(|concept| concept.questions.len())
            .sum(),
    }))
}

/// `GET /catalogs/{catalog}/reports/{report}`: one report, as its tool returns it.
///
/// # Errors
/// 404 for an unknown report name (checked first), or for a catalog that
/// does not exist or that the key does not reach; 400 for a malformed query.
async fn report(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path((catalog, name)): Path<(String, String)>,
    Query(query): Query<ReportQuery>,
) -> Result<Json<Report>, AppError> {
    let ctx = api::context(&app, principal);
    let catalog = Some(catalog);
    let n = Some(query.n.unwrap_or(DEFAULT_ROWS).clamp(1, MAX_ROWS));
    // A string has no closed set of values, so this `match` needs the `_` arm.
    let report = match name.as_str() {
        "lint" => Report::Lint(
            tools::lint_report(
                &ctx,
                LintParams {
                    catalog,
                    changeset: None,
                },
            )
            .await?,
        ),
        "sprawl" => Report::Sprawl(
            tools::sprawl_report(
                &ctx,
                SprawlParams {
                    catalog,
                    changeset: None,
                    path: None,
                    n,
                },
            )
            .await?,
        ),
        "links" => Report::Links(Box::new(
            tools::links(
                &ctx,
                LinksParams {
                    catalog,
                    changeset: None,
                    mentions: query.mentions,
                },
            )
            .await?,
        )),
        "oldest" => Report::Oldest(
            tools::oldest_report(
                &ctx,
                OldestParams {
                    catalog,
                    changeset: None,
                    n,
                    open_questions_only: false,
                },
            )
            .await?,
        ),
        "questions" => Report::Questions(
            tools::open_questions(
                &ctx,
                CatalogParam {
                    catalog,
                    changeset: None,
                },
            )
            .await?,
        ),
        _ => {
            return Err(OkfError::NotFound {
                path: format!("reports/{name}"),
            }
            .into())
        }
    };
    Ok(Json(report))
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::{DEFAULT_ROWS, MAX_ROWS};
    use crate::config::Config;
    use crate::http::api::testing::{app, assert_matrix, keys, send, Row};
    use crate::mcp::testing::{app_with, TestApp};

    /// A `main` with known problems: `gamma` has no `type` (a lint error) and
    /// nothing links to it (an orphan); `alpha` links to a missing `gone.md`
    /// (a broken link); `beta` queues one open question.
    const SICK: &[(&str, &str)] = &[
        (
            "index.md",
            "# Main\n\n- [Alpha](systems/alpha.md): the first system\n",
        ),
        (
            "systems/alpha.md",
            "---\ntype: system\ntitle: Alpha\ndescription: The first system.\n---\n# Alpha\n\n\
             Alpha runs [Beta](beta.md) and [Gone](gone.md).\n",
        ),
        (
            "systems/beta.md",
            "---\ntype: system\ntitle: Beta\ndescription: The second system.\n\
             open_questions:\n  - Who owns Beta?\n---\n# Beta\n\nBeta serves [Alpha](alpha.md).\n",
        ),
        (
            "systems/gamma.md",
            "---\ntitle: Gamma\n---\n# Gamma\n\nNothing links here.\n",
        ),
    ];

    async fn sick() -> TestApp {
        app_with(
            &[("main", SICK), ("other", &[("index.md", "# Other\n")])],
            Config::default(),
        )
        .await
    }

    /// GETs `uri` with `key`, asserts 200 and returns the body.
    async fn get_ok(test: &TestApp, key: &str, uri: &str) -> serde_json::Value {
        let (status, body) = send(test, "GET", uri, Some(key), None).await;
        assert_eq!(status.as_u16(), 200, "GET {uri}: {body}");
        body
    }

    #[tokio::test]
    async fn health_counts_match_the_reports() {
        let test = sick().await;
        let keys = keys(&test).await;
        let base = "/api/v1/catalogs/main";
        let health = get_ok(&test, &keys.read, &format!("{base}/health")).await;
        let lint = get_ok(&test, &keys.read, &format!("{base}/reports/lint")).await;
        let links = get_ok(&test, &keys.read, &format!("{base}/reports/links")).await;
        let oldest = get_ok(&test, &keys.read, &format!("{base}/reports/oldest?n=1")).await;
        let questions = get_ok(&test, &keys.read, &format!("{base}/reports/questions")).await;

        assert_eq!(health["catalog"], "main");
        let errors = lint["errors"].as_array().expect("lint errors");
        let warnings = lint["warnings"].as_array().expect("lint warnings");
        assert!(errors
            .iter()
            .any(|finding| finding["file"] == "systems/gamma.md" && finding["rule"] == "no-type"));
        assert_eq!(health["lint_errors"], json!(errors.len()));
        assert_eq!(health["lint_warnings"], json!(warnings.len()));
        let sprawling = warnings
            .iter()
            .filter(|finding| finding["rule"] == "sprawl")
            .count();
        assert_eq!(health["sprawl"], json!(sprawling));
        assert_eq!(links["orphans"], json!(["systems/gamma.md"]));
        assert_eq!(health["orphans"], 1);
        assert_eq!(links["summary"]["broken"], 1);
        assert_eq!(health["broken_links"], 1);
        assert_eq!(health["oldest"], oldest["rows"][0]);
        assert_eq!(questions["concepts"][0]["path"], "/systems/beta.md");
        assert_eq!(health["open_questions"], 1);
    }

    #[tokio::test]
    async fn reports_are_the_tool_results_with_bounded_row_counts() {
        let test = sick().await;
        let keys = keys(&test).await;
        let base = "/api/v1/catalogs/main/reports";
        // Default 20 rows: all three concepts.
        let oldest = get_ok(&test, &keys.read, &format!("{base}/oldest")).await;
        assert_eq!(oldest["rows"].as_array().expect("rows").len(), 3);
        let sprawl = get_ok(&test, &keys.read, &format!("{base}/sprawl?n=1")).await;
        assert_eq!(sprawl["reports"].as_array().expect("reports").len(), 1);
        // `n` is clamped to 1..=200.
        let clamped = get_ok(&test, &keys.read, &format!("{base}/oldest?n=0")).await;
        assert_eq!(clamped["rows"].as_array().expect("rows").len(), 1);
        let quiet = get_ok(&test, &keys.read, &format!("{base}/links?mentions=false")).await;
        assert_eq!(quiet["summary"]["unlinked_mentions"], 0);
        let lint = get_ok(&test, &keys.read, &format!("{base}/lint")).await;
        assert_eq!(lint["pending"], false);
    }

    #[tokio::test]
    async fn row_limits_default_to_20_and_clamp_to_1_through_200() {
        // More concepts than MAX_ROWS, so every bound can be told apart.
        let docs: Vec<(String, String)> = (0..=MAX_ROWS)
            .map(|i| {
                (
                    format!("systems/c{i:03}.md"),
                    format!("---\ntype: system\ntitle: C{i}\n---\n# C{i}\n\nConcept {i}.\n"),
                )
            })
            .collect();
        // `app_with` takes borrowed pairs; `as_str` borrows from `docs`,
        // which outlives the slice (a Python list of tuples needs no such step).
        let pairs: Vec<(&str, &str)> = docs
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str()))
            .collect();
        let other: &[(&str, &str)] = &[("index.md", "# Other\n")];
        let test = app_with(&[("main", &pairs), ("other", other)], Config::default()).await;
        let keys = keys(&test).await;
        let base = "/api/v1/catalogs/main/reports";
        for (report, field) in [("oldest", "rows"), ("sprawl", "reports")] {
            for (query, expected) in [
                ("", DEFAULT_ROWS),
                ("?n=0", 1),
                ("?n=200", MAX_ROWS),
                ("?n=201", MAX_ROWS),
            ] {
                let body = get_ok(&test, &keys.read, &format!("{base}/{report}{query}")).await;
                let rows = body[field].as_array().expect("report rows").len();
                assert_eq!(rows, expected, "{report}{query}");
            }
        }
    }

    #[tokio::test]
    async fn bad_report_requests_are_refused() {
        let test = sick().await;
        let keys = keys(&test).await;
        let (status, body) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/reports/bogus",
            Some(&keys.read),
            None,
        )
        .await;
        assert_eq!(status.as_u16(), 404);
        assert_eq!(body["error"], "NotFound");
        let (status, _) = send(
            &test,
            "GET",
            "/api/v1/catalogs/main/reports/oldest?n=x",
            Some(&keys.read),
            None,
        )
        .await;
        assert_eq!(status.as_u16(), 400);
    }

    #[tokio::test]
    async fn health_routes_need_a_key_that_reaches_the_catalog() {
        let test = app().await;
        let row = |uri: String, reached: u16, stranger: u16| Row {
            method: "GET",
            uri,
            body: None,
            read: reached,
            write: reached,
            admin: reached,
            stranger,
        };
        let mut rows = vec![
            row("/api/v1/catalogs/main/health".into(), 200, 404),
            row("/api/v1/catalogs/nope/health".into(), 404, 404),
            row("/api/v1/catalogs/main/reports/bogus".into(), 404, 404),
        ];
        rows.extend(
            ["lint", "sprawl", "links", "oldest", "questions"]
                .iter()
                .map(|name| row(format!("/api/v1/catalogs/main/reports/{name}"), 200, 404)),
        );
        assert_matrix(&test, &rows).await;
    }
}

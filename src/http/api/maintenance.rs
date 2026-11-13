//! `/api/v1/catalogs/{catalog}/maintenance/*` (web console design, "API":
//! Maintenance): runs, findings, finding counts and dispositions. Each
//! handler calls the matching `maintenance_*` tool function, so access
//! (read to list, write to dispose), paging and idempotency are the tool's.

use std::sync::Arc;

use axum::extract::{Path, Query, State};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;

use crate::app::App;
use crate::auth::Principal;
use crate::http::api;
use crate::http::AppError;
use crate::maintenance::types::Disposition;
use crate::mcp::maintenance::reports::{
    self as tools, MaintenanceDisposeParams, MaintenanceGetFindingParams, MaintenanceGetRunParams,
    MaintenanceListFindingsParams, MaintenanceListRunsParams,
};
use crate::mcp::maintenance::MaintenanceResult;
use crate::store::maintenance::reports::{FindingCounts, FindingRecord};
use crate::store::maintenance::runs::{Page, RunReport, RunSummary};

/// `?after=&limit=` for runs.
#[derive(Debug, Deserialize)]
struct RunsQuery {
    /// The last run id of the previous page.
    after: Option<String>,
    /// Page size, 1..=200 (default 20).
    limit: Option<u32>,
}

/// `?after=&limit=&disposition=` for findings.
#[derive(Debug, Deserialize)]
struct FindingsQuery {
    /// The last finding id of the previous page.
    after: Option<String>,
    /// Page size, 1..=200 (default 20).
    limit: Option<u32>,
    /// Keep only this current disposition (an unknown value is a 400).
    disposition: Option<Disposition>,
}

/// `POST .../disposition`'s body: the tool's arguments without the ids,
/// which come from the path. Unknown fields are refused (422), as the tool
/// refuses them.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DisposeBody {
    /// The decision.
    disposition: Disposition,
    /// Why, recorded with the key's actor.
    rationale: String,
    /// The caller's retry key (`crypto.randomUUID()` in the console).
    idempotency_key: String,
}

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/catalogs/{catalog}/maintenance/runs", get(list_runs))
        .route("/catalogs/{catalog}/maintenance/runs/{run}", get(get_run))
        .route(
            "/catalogs/{catalog}/maintenance/findings",
            get(list_findings),
        )
        .route(
            "/catalogs/{catalog}/maintenance/findings/{finding}",
            get(get_finding),
        )
        .route(
            "/catalogs/{catalog}/maintenance/findings/{finding}/disposition",
            post(dispose),
        )
        .route("/catalogs/{catalog}/maintenance/counts", get(counts))
}

/// `GET .../maintenance/runs`: one page of runs, oldest first.
///
/// # Errors
/// As `maintenance_list_runs`: 404 unreached catalog or unknown cursor, 409 bad limit.
async fn list_runs(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(catalog): Path<String>,
    Query(query): Query<RunsQuery>,
) -> Result<Json<MaintenanceResult<Page<RunSummary>>>, AppError> {
    let ctx = api::context(&app, principal);
    let params = MaintenanceListRunsParams {
        catalog,
        after: query.after,
        limit: query.limit,
    };
    Ok(Json(tools::maintenance_list_runs(&ctx, params).await?))
}

/// `GET .../maintenance/runs/{run}`: a run and its work items.
///
/// # Errors
/// As `maintenance_get_run`: 404 unreached catalog or unknown run.
async fn get_run(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path((catalog, run_id)): Path<(String, String)>,
) -> Result<Json<MaintenanceResult<RunReport>>, AppError> {
    let ctx = api::context(&app, principal);
    let params = MaintenanceGetRunParams { catalog, run_id };
    Ok(Json(tools::maintenance_get_run(&ctx, params).await?))
}

/// `GET .../maintenance/findings`: one page of findings, oldest first,
/// optionally of one current disposition.
///
/// # Errors
/// As `maintenance_list_findings`; 400 for an unknown disposition.
async fn list_findings(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(catalog): Path<String>,
    Query(query): Query<FindingsQuery>,
) -> Result<Json<MaintenanceResult<Page<FindingRecord>>>, AppError> {
    let ctx = api::context(&app, principal);
    let params = MaintenanceListFindingsParams {
        catalog,
        after: query.after,
        limit: query.limit,
    };
    Ok(Json(
        tools::maintenance_list_findings_by(&ctx, params, query.disposition).await?,
    ))
}

/// `GET .../maintenance/findings/{finding}`.
///
/// # Errors
/// As `maintenance_get_finding`: 404 unreached catalog or unknown finding.
async fn get_finding(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path((catalog, finding_id)): Path<(String, String)>,
) -> Result<Json<MaintenanceResult<FindingRecord>>, AppError> {
    let ctx = api::context(&app, principal);
    let params = MaintenanceGetFindingParams {
        catalog,
        finding_id,
    };
    Ok(Json(tools::maintenance_get_finding(&ctx, params).await?))
}

/// `POST .../maintenance/findings/{finding}/disposition`: appends a decision.
///
/// # Errors
/// As `maintenance_dispose`: 403 below write; 404 unreached catalog or
/// unknown finding; 409 for a reused key with a different body; 400 for a
/// secret in the rationale; 422 for a malformed body.
async fn dispose(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path((catalog, finding_id)): Path<(String, String)>,
    // `Json` reads the body, so it must be the last extractor.
    Json(body): Json<DisposeBody>,
) -> Result<Json<MaintenanceResult<FindingRecord>>, AppError> {
    let ctx = api::context(&app, principal);
    let params = MaintenanceDisposeParams {
        catalog,
        finding_id,
        disposition: body.disposition,
        rationale: body.rationale,
        idempotency_key: body.idempotency_key,
    };
    Ok(Json(tools::maintenance_dispose(&ctx, params).await?))
}

/// `GET .../maintenance/counts`: findings per current disposition.
///
/// # Errors
/// 404 for an unreached catalog.
async fn counts(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(catalog): Path<String>,
) -> Result<Json<MaintenanceResult<FindingCounts>>, AppError> {
    let ctx = api::context(&app, principal);
    Ok(Json(
        tools::maintenance_finding_counts(&ctx, &catalog).await?,
    ))
}

#[cfg(test)]
mod tests {
    use chrono::{Duration, Utc};
    use serde_json::json;

    use crate::http::api::testing::{app, assert_matrix, keys, send, Row};
    use crate::maintenance::types::{
        Actor, Assessment, Category, Evidence, FindingInput, Relationship, Submission, WorkOutcome,
    };
    use crate::mcp::testing::TestApp;
    use crate::store::keys::{self as key_rows, Access, Grants, NewKey};
    use crate::store::maintenance::{reports as finding_store, runs as run_store};

    const BASE: &str = "/api/v1/catalogs/main/maintenance";

    /// One completed run on `main` with one open finding, written straight
    /// through the store (tests/maintenance.rs covers the reporter's MCP path).
    ///
    /// # Returns
    /// The run id and the finding id.
    async fn seed(test: &TestApp) -> (String, String) {
        let state = test.app.catalog("main").expect("main is loaded");
        let pool = test.app.pool();
        let now = test.app.clock().now();
        let reporter = Actor {
            key_id: "reporter".into(),
            actor: "agent/reporter".into(),
        };
        let claimed = run_store::claim(
            pool,
            state.id(),
            &reporter,
            "seed-claim",
            1,
            Duration::days(30),
            now,
        )
        .await
        .expect("claim one concept");
        let lease = claimed.leases.first().expect("one lease");
        // Both MAIN concepts contain this line, whichever one is leased.
        let evidence = Evidence {
            pin: lease.pin.clone(),
            excerpt: "type: system".into(),
        };
        let submission = Submission {
            outcome: WorkOutcome::Completed,
            assessment: Assessment::SupportedCurrent,
            summary: "Synthetic investigation.".into(),
            evidence: vec![evidence.clone()],
            findings: vec![FindingInput {
                category: Category::UnverifiableClaim,
                assertion: "The owner of this system is not recorded.".into(),
                follow_up: "Ask the owner.".into(),
                uncertainty: "Synthetic only.".into(),
                relationship: Relationship::DirectStatement,
                evidence: vec![evidence],
                external: vec![],
            }],
            reported_tokens: None,
        };
        let receipt = finding_store::submit(
            pool,
            state.id(),
            &reporter,
            &lease.work_id,
            &lease.token,
            "seed-submit",
            &submission,
            now,
        )
        .await
        .expect("submit the work");
        let finding = receipt.finding_ids.first().expect("one finding").clone();
        (claimed.run_id.clone(), finding)
    }

    fn dispose_body(disposition: &str, rationale: &str, key: &str) -> serde_json::Value {
        json!({"disposition": disposition, "rationale": rationale, "idempotency_key": key})
    }

    #[tokio::test]
    async fn maintenance_routes_follow_the_tools_access() {
        let test = app().await;
        let (run, finding) = seed(&test).await;
        let read = |uri: String| Row {
            method: "GET",
            uri,
            body: None,
            read: 200,
            write: 200,
            admin: 200,
            stranger: 404,
        };
        assert_matrix(
            &test,
            &[
                read(format!("{BASE}/runs")),
                read(format!("{BASE}/runs/{run}")),
                read(format!("{BASE}/findings")),
                read(format!("{BASE}/findings?disposition=open")),
                read(format!("{BASE}/findings/{finding}")),
                read(format!("{BASE}/counts")),
                // Write, as maintenance_dispose. Admin is a different
                // credential, so its same idempotency key is a new request.
                Row {
                    method: "POST",
                    uri: format!("{BASE}/findings/{finding}/disposition"),
                    body: Some(dispose_body("needs_input", "Ask the owner.", "matrix")),
                    read: 403,
                    write: 200,
                    admin: 200,
                    stranger: 404,
                },
            ],
        )
        .await;
    }

    #[tokio::test]
    async fn the_filter_and_the_counts_follow_the_current_disposition() {
        let test = app().await;
        let (_, finding) = seed(&test).await;
        let keys = keys(&test).await;
        let get = |uri: String| {
            let key = keys.read.clone();
            let test = &test;
            async move { send(test, "GET", &uri, Some(&key), None).await }
        };
        let (status, open) = get(format!("{BASE}/findings?disposition=open")).await;
        assert_eq!(status.as_u16(), 200, "{open}");
        assert_eq!(open["catalog"], "main");
        assert_eq!(open["items"][0]["id"], finding.as_str());
        assert!(open["items"][0]["input"]["evidence"][0]["pin"]["path"]
            .as_str()
            .is_some_and(|path| path.starts_with("/systems/")));
        let (_, counts) = get(format!("{BASE}/counts")).await;
        assert_eq!(
            counts,
            json!({"catalog": "main", "open": 1, "accepted_for_follow_up": 0,
                   "dismissed": 0, "needs_input": 0})
        );

        let (status, disposed) = send(
            &test,
            "POST",
            &format!("{BASE}/findings/{finding}/disposition"),
            Some(&keys.write),
            Some(dispose_body("dismissed", "Covered elsewhere.", "k1")),
        )
        .await;
        assert_eq!(status.as_u16(), 200, "{disposed}");
        assert_eq!(disposed["disposition"], "dismissed");
        assert_eq!(disposed["dispositions"][0]["actor"], "writer");

        let (_, open) = get(format!("{BASE}/findings?disposition=open")).await;
        assert_eq!(open["items"], json!([]));
        assert_eq!(open["next_after"], serde_json::Value::Null);
        let (_, dismissed) = get(format!("{BASE}/findings?disposition=dismissed")).await;
        assert_eq!(dismissed["items"][0]["id"], finding.as_str());
        let (_, all) = get(format!("{BASE}/findings")).await;
        assert_eq!(all["items"].as_array().expect("items").len(), 1);
        let (_, counts) = get(format!("{BASE}/counts")).await;
        assert_eq!(counts["open"], 0);
        assert_eq!(counts["dismissed"], 1);
    }

    #[tokio::test]
    async fn a_retried_disposition_replays_and_a_changed_one_is_refused() {
        let test = app().await;
        let (_, finding) = seed(&test).await;
        let keys = keys(&test).await;
        let uri = format!("{BASE}/findings/{finding}/disposition");
        let body = dispose_body("accepted_for_follow_up", "Worth a look.", "retry-1");
        let (first_status, first) =
            send(&test, "POST", &uri, Some(&keys.write), Some(body.clone())).await;
        let (again_status, again) = send(&test, "POST", &uri, Some(&keys.write), Some(body)).await;
        assert_eq!((first_status.as_u16(), again_status.as_u16()), (200, 200));
        assert_eq!(first, again, "a retry returns the original response");
        assert_eq!(again["dispositions"].as_array().expect("history").len(), 1);
        let (status, refused) = send(
            &test,
            "POST",
            &uri,
            Some(&keys.write),
            Some(dispose_body(
                "accepted_for_follow_up",
                "Changed my mind.",
                "retry-1",
            )),
        )
        .await;
        assert_eq!(status.as_u16(), 409);
        assert_eq!(refused["error"], "Refused");
    }

    #[tokio::test]
    async fn bad_maintenance_requests_are_refused() {
        let test = app().await;
        let (_, finding) = seed(&test).await;
        let keys = keys(&test).await;
        let get = |uri: String| {
            let key = keys.read.clone();
            let test = &test;
            async move { send(test, "GET", &uri, Some(&key), None).await.0.as_u16() }
        };
        assert_eq!(get(format!("{BASE}/findings?disposition=bogus")).await, 400);
        assert_eq!(get(format!("{BASE}/findings/missing")).await, 404);
        assert_eq!(get(format!("{BASE}/runs/missing")).await, 404);
        assert_eq!(get(format!("{BASE}/findings?after=missing")).await, 404);
        // Out-of-range page sizes are the tool's `Refused`.
        assert_eq!(get(format!("{BASE}/runs?limit=0")).await, 409);
        // The spec's `note` is not a field: the body is the tool's.
        let (status, _) = send(
            &test,
            "POST",
            &format!("{BASE}/findings/{finding}/disposition"),
            Some(&keys.write),
            Some(json!({"disposition": "dismissed", "note": "No.", "idempotency_key": "n"})),
        )
        .await;
        assert_eq!(status.as_u16(), 422);
    }

    #[tokio::test]
    async fn a_reporter_cannot_decide_and_an_unknown_catalog_is_404() {
        let test = app().await;
        let (_, finding) = seed(&test).await;
        let keys = keys(&test).await;
        // A read-level key with the reporting flag, as the runner holds.
        let grants = ["main".to_owned()];
        let reporter = key_rows::create_with_id(
            test.app.pool(),
            &NewKey {
                name: "runner",
                access: Access::Read,
                maintenance_report: true,
                grants: Grants::Catalogs(&grants),
                default_catalog: Some("main"),
                actor: "runner",
                user: crate::db::TEST_USER,
                self_issued: false,
                expires_at: None,
                levels: &[],
            },
            Utc::now(),
        )
        .await
        .expect("create a reporter key")
        .key;
        test.app.poll().await.expect("poll the reporter in");
        let (status, body) = send(
            &test,
            "POST",
            &format!("{BASE}/findings/{finding}/disposition"),
            Some(&reporter),
            Some(dispose_body("dismissed", "Not mine to decide.", "reporter")),
        )
        .await;
        assert_eq!(status.as_u16(), 403, "{body}");
        let missing = "/api/v1/catalogs/missing/maintenance";
        let decision = dispose_body("dismissed", "No such catalog.", "missing");
        for (method, uri, body) in [
            ("GET", format!("{missing}/runs"), None),
            ("GET", format!("{missing}/runs/{finding}"), None),
            ("GET", format!("{missing}/findings"), None),
            ("GET", format!("{missing}/findings/{finding}"), None),
            ("GET", format!("{missing}/counts"), None),
            (
                "POST",
                format!("{missing}/findings/{finding}/disposition"),
                Some(decision),
            ),
        ] {
            let (status, _) = send(&test, method, &uri, Some(&keys.admin), body).await;
            assert_eq!(status.as_u16(), 404, "{method} {uri}");
        }
    }

    #[tokio::test]
    async fn a_run_lists_its_work_and_findings() {
        let test = app().await;
        let (run, finding) = seed(&test).await;
        let keys = keys(&test).await;
        let (_, runs) = send(
            &test,
            "GET",
            &format!("{BASE}/runs"),
            Some(&keys.read),
            None,
        )
        .await;
        assert_eq!(runs["items"][0]["id"], run.as_str());
        assert_eq!(runs["items"][0]["state"], "completed");
        let (status, report) = send(
            &test,
            "GET",
            &format!("{BASE}/runs/{run}"),
            Some(&keys.read),
            None,
        )
        .await;
        assert_eq!(status.as_u16(), 200, "{report}");
        assert_eq!(report["run"]["completed_count"], 1);
        assert_eq!(report["work"][0]["finding_ids"], json!([finding]));
        assert!(report["work"][0]["pin"]["path"]
            .as_str()
            .is_some_and(|path| path.starts_with('/')));
    }

    /// M13 Task 3: every maintenance route refuses a suspended catalog to a
    /// non-admin key with `Suspended` (after the level check), answers an
    /// admin as before, and still answers 404 to a key that does not reach it.
    #[tokio::test]
    async fn maintenance_routes_refuse_a_suspended_catalog_but_to_an_admin() {
        use crate::http::api::testing::{assert_answers, Expect};
        use crate::mcp::testing::suspend_catalogs;
        let test = app().await;
        let keys = keys(&test).await;
        let (run, finding) = seed(&test).await;
        suspend_catalogs(&test, &["main"]).await;
        let reads = [
            format!("{BASE}/runs"),
            format!("{BASE}/runs/{run}"),
            format!("{BASE}/findings"),
            format!("{BASE}/findings?disposition=open"),
            format!("{BASE}/findings/{finding}"),
            format!("{BASE}/counts"),
        ];
        let dispose = format!("{BASE}/findings/{finding}/disposition");
        let body = || Some(dispose_body("needs_input", "Ask the owner.", "suspended"));
        let mut expected = Vec::new();
        for uri in &reads {
            for (who, key, status, error) in [
                ("read", &keys.read, 403, Some("Suspended")),
                ("write", &keys.write, 403, Some("Suspended")),
                ("stranger", &keys.stranger, 404, Some("CatalogNotSelected")),
                ("admin", &keys.admin, 200, None),
            ] {
                expected.push(Expect {
                    method: "GET",
                    uri,
                    body: None,
                    key,
                    who,
                    status,
                    error,
                });
            }
        }
        // The tool selects its catalog before checking `write`, so since M13
        // Task 4 the selection refuses the read key as suspended first (it
        // reaches `main`, so it learns nothing it could not list).
        for (who, key, status, error) in [
            ("read", &keys.read, 403, Some("Suspended")),
            ("write", &keys.write, 403, Some("Suspended")),
            ("stranger", &keys.stranger, 404, Some("CatalogNotSelected")),
            ("admin", &keys.admin, 200, None),
        ] {
            expected.push(Expect {
                method: "POST",
                uri: &dispose,
                body: body(),
                key,
                who,
                status,
                error,
            });
        }

        assert_answers(&test, &expected).await;
    }
}

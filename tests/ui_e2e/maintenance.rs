//! Findings and Runs: a writer dismisses a seeded finding and watches its
//! state and the badge change; a reader sees the run, its work and the
//! finding, but no decision buttons.

use okf_mcp::config::Config;
use okf_mcp::store::keys::Access;
use serde_json::{json, Value};

use crate::harness::drive;
use crate::support::server::{call, files, structured, TestServer};

const ALPHA: &str = "---\ntype: system\ntitle: Alpha\ndescription: The first system.\n---\n# Alpha\n\nAlpha runs [Beta](/systems/beta.md).\n";
const BETA: &str = "---\ntype: system\ntitle: Beta\ndescription: The second system.\n---\n# Beta\n\nBeta serves [Alpha](/systems/alpha.md).\n";
const CLAIM: &str = "The owner of this system is not recorded.";

/// A server whose `main` has one completed run with one open finding,
/// reported the way the runner reports: a reporter key through MCP.
///
/// # Returns
/// The server, the run id and the finding id.
async fn seeded() -> (TestServer, String, String) {
    let server = TestServer::start(
        vec![(
            "main",
            files(&[
                ("index.md", "# Main\n"),
                ("systems/alpha.md", ALPHA),
                ("systems/beta.md", BETA),
            ]),
        )],
        Config::default(),
    )
    .await;
    let reporter = server.reporter_key("console-reporter", &["main"]).await;
    let client = server
        .client(&reporter, None)
        .await
        .expect("a reporter client");
    let claimed: Value = structured(
        &call(
            &client,
            "maintenance_claim",
            json!({"catalog": "main", "idempotency_key": "seed-claim", "batch_size": 1}),
        )
        .await,
    )
    .clone();
    let lease = &claimed["leases"][0];
    // Both concepts contain this line, whichever one is leased.
    let evidence = json!([{"pin": lease["pin"], "excerpt": "type: system"}]);
    let submission = json!({
        "outcome": "completed", "assessment": "supported_current",
        "summary": "Synthetic investigation.", "evidence": evidence,
        "findings": [{"category": "unverifiable_claim", "assertion": CLAIM,
                      "follow_up": "Ask the owner.", "uncertainty": "Synthetic only.",
                      "relationship": "direct_statement", "evidence": evidence, "external": []}],
        "reported_tokens": null,
    });
    let receipt: Value = structured(
        &call(
            &client,
            "maintenance_submit",
            json!({"catalog": "main", "work_id": lease["work_id"], "token": lease["token"],
                   "idempotency_key": "seed-submit", "submission": submission}),
        )
        .await,
    )
    .clone();
    let run = claimed["run_id"].as_str().expect("a run id").to_string();
    let finding = receipt["finding_ids"][0]
        .as_str()
        .expect("a finding id")
        .to_string();
    (server, run, finding)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_writer_dismisses_a_finding_and_its_state_changes() {
    let (server, _run, finding) = seeded().await;
    let key = server
        .key("console-writer", Access::Write, &["main"], Some("main"))
        .await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.wait_text("#findings-badge", "1");
        ui.open("/findings");
        ui.wait_text("#findings-table tbody", CLAIM);
        ui.wait_text("#findings-table tbody", "Open");
        ui.click("#findings-table tbody tr.clickable");
        ui.wait_text("#drawer", &finding);
        ui.wait_text("#finding-evidence", "/systems/");
        ui.wait_text("#finding-history", "No decisions yet.");
        ui.fill("#dispose-rationale", "Covered by the owner list.");
        ui.click("#dispose-dismissed");
        ui.wait_text("#finding-state", "Dismissed");
        ui.wait_text("#finding-history", "console-writer");
        ui.wait_hidden("#findings-badge");
        ui.click("#drawer-close");
        // The Open view no longer lists it; the Dismissed view does.
        ui.wait_text("#findings-table tbody", "No findings here.");
        ui.click("#findings-filter .seg[data-value=dismissed]");
        ui.wait_text("#findings-table tbody", "Dismissed");
        ui.wait_text("#findings-table tbody", CLAIM);
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reader_sees_the_run_and_its_work_but_cannot_decide() {
    let (server, run, finding) = seeded().await;
    let key = server
        .key("console-reader", Access::Read, &["main"], Some("main"))
        .await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.open("/runs");
        ui.wait_text("#runs-table tbody", &run);
        ui.wait_text("#runs-table tbody", "completed");
        ui.click("#runs-table tbody tr.clickable");
        ui.wait_text("#run-state", "completed");
        ui.wait_text("#run-work", "/systems/");
        ui.wait_text("#run-work", &finding);
        // The work item's finding link opens that finding's drawer, read-only.
        ui.click(&format!("#run-work a[href='#/findings/{finding}']"));
        ui.wait_text("#finding-claim", CLAIM);
        assert!(!ui.page.is_visible("#dispose-actions").expect("visible"));
    })
    .await;
}

//! Findings and Runs: a writer dismisses a seeded finding and watches its
//! state and the badge change; a reader sees the run, its work and the
//! finding, but no decision buttons; and two delayed-response cases, where
//! an older answer must never overwrite a newer one (the badge) or mix the
//! previous filter's pages into the new one (the findings table).

use okf_mcp::archive::Files;
use okf_mcp::config::Config;
use okf_mcp::store::keys::Access;
use serde_json::{json, Value};

use crate::harness::{drive, Ui};
use crate::support::server::{call, files, structured, Client, TestServer};

const CLAIM: &str = "The owner of this system is not recorded.";

/// A catalog's files: an index and `concepts` system concepts, each one a
/// claimable maintenance candidate whose text contains `type: system`.
fn catalog(concepts: usize) -> Files {
    let pages: Vec<(String, String)> = (1..=concepts)
        .map(|n| {
            (
                format!("systems/system-{n:02}.md"),
                format!(
                    "---\ntype: system\ntitle: System {n}\ndescription: Synthetic system {n}.\n\
                     ---\n# System {n}\n\nA synthetic system.\n"
                ),
            )
        })
        .collect();
    // `files` takes borrowed pairs; `pages` owns the strings they borrow.
    let mut borrowed: Vec<(&str, &str)> = vec![("index.md", "# Index\n")];
    borrowed.extend(
        pages
            .iter()
            .map(|(path, text)| (path.as_str(), text.as_str())),
    );
    files(&borrowed)
}

/// Reports one completed run on `catalog` the way the runner does (a
/// reporter client through MCP): `concepts` work items, each with
/// `per_work` open findings (at most 20, the submission limit).
///
/// # Returns
/// The run id and every finding id.
async fn report(
    client: &Client,
    catalog: &str,
    concepts: u32,
    per_work: usize,
) -> (String, Vec<String>) {
    let claimed: Value = structured(
        &call(
            client,
            "maintenance_claim",
            json!({"catalog": catalog, "idempotency_key": format!("claim-{catalog}"),
                   "batch_size": concepts}),
        )
        .await,
    )
    .clone();
    let leases = claimed["leases"].as_array().expect("leases");
    assert_eq!(leases.len(), concepts as usize, "every concept leased");
    let mut findings = Vec::new();
    for (index, lease) in leases.iter().enumerate() {
        let evidence = json!([{"pin": lease["pin"], "excerpt": "type: system"}]);
        let finding = json!({"category": "unverifiable_claim", "assertion": CLAIM,
                             "follow_up": "Ask the owner.", "uncertainty": "Synthetic only.",
                             "relationship": "direct_statement", "evidence": evidence,
                             "external": []});
        let submission = json!({
            "outcome": "completed", "assessment": "supported_current",
            "summary": "Synthetic investigation.", "evidence": evidence,
            "findings": vec![finding; per_work], "reported_tokens": null,
        });
        let receipt: Value = structured(
            &call(
                client,
                "maintenance_submit",
                json!({"catalog": catalog, "work_id": lease["work_id"], "token": lease["token"],
                       "idempotency_key": format!("submit-{catalog}-{index}"),
                       "submission": submission}),
            )
            .await,
        )
        .clone();
        findings.extend(
            receipt["finding_ids"]
                .as_array()
                .expect("finding ids")
                .iter()
                .map(|id| id.as_str().expect("a finding id").to_string()),
        );
    }
    let run = claimed["run_id"].as_str().expect("a run id").to_string();
    (run, findings)
}

/// A server with the given catalogs (name, concept count) and a reporter
/// client that reaches them all.
async fn server_with(catalogs: &[(&str, usize)]) -> (TestServer, Client) {
    let server = TestServer::start(
        catalogs
            .iter()
            .map(|(name, concepts)| (*name, catalog(*concepts)))
            .collect(),
        Config::default(),
    )
    .await;
    let names: Vec<&str> = catalogs.iter().map(|(name, _)| *name).collect();
    let reporter = server.reporter_key("console-reporter", &names).await;
    let client = server
        .client(&reporter, None)
        .await
        .expect("a reporter client");
    (server, client)
}

/// A server whose `main` has one completed run with one open finding.
///
/// # Returns
/// The server, the run id and the finding id.
async fn seeded() -> (TestServer, String, String) {
    let (server, client) = server_with(&[("main", 2)]).await;
    let (run, findings) = report(&client, "main", 1, 1).await;
    (server, run, findings[0].clone())
}

/// Wraps the page's `fetch` so a request whose URL contains a pattern
/// (set with [`hold`]) is sent at once but its answer is held until
/// [`release`]. `api.js` calls the global `fetch` on every request, so the
/// wrapper sees the console's own calls. Each held entry keeps the real
/// answer's promise, so a journey can wait until the server has answered.
fn install_hold(ui: &Ui) {
    ui.eval(
        "() => { \
           const real = window.fetch; window.heldAnswers = []; window.holdPattern = null; \
           window.fetch = (url, options) => { \
             const answer = real(url, options); \
             if (!window.holdPattern || !String(url).includes(window.holdPattern)) return answer; \
             return new Promise(resolve => window.heldAnswers.push({ answer, deliver: fail => \
               resolve(fail ? new Response('{}', { status: 500 }) : answer) })); }; }",
        json!(null),
    );
}

/// Holds the answers of requests whose URL contains `pattern` from now on.
fn hold(ui: &Ui, pattern: &str) {
    ui.eval(
        "(pattern) => { window.holdPattern = pattern; }",
        json!(pattern),
    );
}

/// Waits until one held request exists and the server has answered it.
fn wait_held_answered(ui: &Ui) {
    ui.eval(
        "async () => { while (window.heldAnswers.length === 0) \
           await new Promise(resolve => setTimeout(resolve, 10)); \
           await window.heldAnswers[0].answer; }",
        json!(null),
    );
}

/// Stops holding and delivers every held answer, as a 500 when `fail`.
///
/// # Returns
/// How many answers were held.
fn release(ui: &Ui, fail: bool) -> u64 {
    ui.eval(
        "(fail) => { window.holdPattern = null; const held = window.heldAnswers.splice(0); \
           held.forEach(entry => entry.deliver(fail)); return held.length; }",
        json!(fail),
    )
    .as_u64()
    .expect("a count")
}

/// Selects `catalog` in the switcher, as a user would.
fn switch_to(ui: &Ui, catalog: &str) {
    ui.eval(
        "(catalog) => { const select = document.getElementById('catalog-select'); \
           select.value = catalog; select.dispatchEvent(new Event('change')); }",
        json!(catalog),
    );
}

/// Starts a badge refresh through the module main.js uses, without waiting.
fn refresh_badge(ui: &Ui) {
    ui.eval(
        "async () => { (await import('/ui/js/pages/findings.js')).refreshFindingsBadge(); }",
        json!(null),
    );
}

/// The badge's text, or "hidden", once a released answer has had time to land.
fn badge(ui: &Ui) -> String {
    ui.eval(
        "async () => { await new Promise(resolve => setTimeout(resolve, 300)); \
           const b = document.getElementById('findings-badge'); \
           return b.hidden ? 'hidden' : b.textContent; }",
        json!(null),
    )
    .as_str()
    .expect("text")
    .to_string()
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_older_badge_answer_never_overwrites_a_newer_one() {
    let (server, client) = server_with(&[("main", 1), ("other", 1)]).await;
    let (_, main_findings) = report(&client, "main", 1, 2).await;
    report(&client, "other", 1, 3).await;
    let key = server
        .key(
            "console-badge",
            Access::Write,
            &["main", "other"],
            Some("main"),
        )
        .await;
    let finding = main_findings[0].clone();
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.wait_text("#findings-badge", "2");
        install_hold(ui);

        // main's count is pending when the user switches to `other`, whose
        // count arrives first; main's late failure must not hide it.
        hold(ui, "/catalogs/main/maintenance/counts");
        refresh_badge(ui);
        wait_held_answered(ui);
        switch_to(ui, "other");
        ui.wait_text("#findings-badge", "3");
        assert_eq!(release(ui, true), 1, "main's count was held");
        assert_eq!(badge(ui), "3", "a stale failure left the badge alone");

        // Back on main: a count taken before a decision arrives after the
        // one taken after it, and must not restore the old number.
        switch_to(ui, "main");
        ui.wait_text("#findings-badge", "2");
        hold(ui, "/catalogs/main/maintenance/counts");
        refresh_badge(ui);
        wait_held_answered(ui);
        let status = ui.eval(
            "async (finding) => (await fetch( \
               `/api/v1/catalogs/main/maintenance/findings/${finding}/disposition`, { \
                 method: 'POST', \
                 headers: { Authorization: `Bearer ${localStorage.getItem('okf.key')}`, \
                            'Content-Type': 'application/json' }, \
                 body: JSON.stringify({ disposition: 'dismissed', rationale: 'Done.', \
                                        idempotency_key: 'badge-order' }) })).status",
            json!(finding),
        );
        assert_eq!(status, json!(200));
        hold(ui, "no request matches this");
        refresh_badge(ui);
        ui.wait_text("#findings-badge", "1");
        assert_eq!(release(ui, false), 1, "the older count was held");
        assert_eq!(badge(ui), "1", "the older count did not win");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_filter_change_drops_the_old_pages_cursor_at_once() {
    // 11 work items of 20 findings: 220 open, more than one 200-row page.
    let (server, client) = server_with(&[("main", 11)]).await;
    let (_, findings) = report(&client, "main", 11, 20).await;
    let key = server
        .key("console-filter", Access::Write, &["main"], Some("main"))
        .await;
    let writer = server.client(&key, None).await.expect("a writer client");
    structured(
        &call(
            &writer,
            "maintenance_dispose",
            json!({"catalog": "main", "finding_id": findings[0], "disposition": "dismissed",
                   "rationale": "Covered elsewhere.", "idempotency_key": "filter-dismiss"}),
        )
        .await,
    );
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.open("/findings");
        ui.wait_visible("#findings-more");
        install_hold(ui);
        hold(ui, "disposition=dismissed");
        ui.click("#findings-filter .seg[data-value=dismissed]");
        // While the Dismissed page is pending, Open's "Load more" is gone,
        // and even a scripted click on it sends nothing.
        let state = ui.eval(
            "() => { const more = document.getElementById('findings-more'); \
               const hidden = more.hidden; more.click(); \
               return [hidden, window.heldAnswers.length]; }",
            json!(null),
        );
        assert_eq!(
            state,
            json!([true, 1]),
            "hidden, and only the reset was sent"
        );
        release(ui, false);
        ui.wait_text("#findings-table tbody", "Dismissed");
        let rows = ui.eval(
            "() => [...document.querySelectorAll('#findings-table tbody tr')] \
               .map(row => row.textContent.includes('Dismissed'))",
            json!(null),
        );
        assert_eq!(rows, json!([true]), "exactly the one dismissed finding");
        assert!(!ui.page.is_visible("#findings-more").expect("visible"));
    })
    .await;
}

//! The Changesets page: an admin finds a changeset an agent left open,
//! reads its diff and discards it.

use okf_mcp::config::Config;
use okf_mcp::store::keys::Access;
use serde_json::json;

use crate::harness::drive;
use crate::support::server::{call, files, open_changeset, structured, TestServer};

const ALPHA: &str = "---\ntype: system\ntitle: Alpha\n---\n# Alpha\n\nAlpha runs.\n";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_discards_a_changeset_from_the_console() {
    let server = TestServer::start(
        vec![(
            "main",
            files(&[
                ("index.md", "# Main\n\n- [Alpha](systems/alpha.md)\n"),
                ("systems/alpha.md", ALPHA),
            ]),
        )],
        Config::default(),
    )
    .await;
    let key = server
        .key("console-admin", Access::Admin, &["main"], Some("main"))
        .await;
    // An agent's unfinished work: one edit in an open changeset.
    let client = server.client(&key, None).await.expect("connect");
    let handle = open_changeset(&client, Some("main")).await;
    let edited = call(
        &client,
        "edit_document",
        json!({
            "path": "/systems/alpha.md", "old_str": "Alpha runs.", "new_str": "Alpha runs well.",
            "changeset": handle,
        }),
    )
    .await;
    structured(&edited);

    let shown = handle.clone();
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.open("/changesets");
        ui.wait_text("#changesets-table", &shown);
        ui.click("[data-test=changeset-handle]");
        ui.wait_text("#changeset-diff", "+Alpha runs well.");
        ui.click("#changeset-discard");
        ui.click("#confirm-ok");
        ui.wait_text("#changesets-table", "No open changesets");
        ui.wait_hidden("#drawer");
    })
    .await;

    // The server agrees: nothing is open any more.
    let listed = call(&client, "list_changesets", json!({})).await;
    assert_eq!(structured(&listed)["changesets"], json!([]));
}

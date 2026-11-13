//! The whole console (M9 Task 8): an admin visits every page in turn from
//! the sidebar, each page renders its content or its empty state, and no
//! page logs a console error (`drive` fails the journey on any).

use okf_mcp::config::Config;
use okf_mcp::store::keys::Access;

use crate::harness::drive;
use crate::support::server::{files, TestServer};

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_visits_every_page_without_a_console_error() {
    let server = TestServer::start(
        vec![(
            "main",
            files(&[
                (
                    "index.md",
                    "# Main\n\n- [Alpha](systems/alpha.md): the first system\n",
                ),
                ("systems/index.md", "# Systems\n\n- [Alpha](alpha.md)\n"),
                (
                    "systems/alpha.md",
                    "---\ntype: system\ntitle: Alpha\n---\n# Alpha\n\nAlpha runs.\n",
                ),
            ]),
        )],
        Config::default(),
    )
    .await;
    let key = server
        .key("console-smoke", Access::Admin, &["main"], Some("main"))
        .await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);

        ui.click("a[data-nav=health]");
        ui.wait_visible("#n-lint");
        ui.wait_visible("#n-oldest");

        ui.click("a[data-nav=browse]");
        ui.wait_text("#concept-title", "/index.md");
        ui.open("/browse/main/systems/alpha.md");
        ui.wait_text("#concept-title", "Alpha");
        ui.wait_text("#markdown-body", "Alpha runs.");

        ui.click("a[data-nav=search]");
        ui.fill("#search-query", "alpha");
        ui.click("#search-form button[type=submit]");
        ui.wait_text("#search-count", "hit");
        assert!(ui
            .text("#search-results")
            .contains("main /systems/alpha.md"));

        ui.click("a[data-nav=findings]");
        ui.wait_text("#findings-table tbody", "No findings here.");

        ui.click("a[data-nav=runs]");
        ui.wait_text("#runs-table tbody", "No maintenance runs yet.");

        ui.click("a[data-nav=changesets]");
        ui.wait_text("#changesets-table", "No open changesets.");

        ui.click("a[data-nav=catalogs]");
        ui.wait_visible("[data-catalog=main]");

        ui.click("a[data-nav=transfer]");
        ui.click("#export-btn");
        ui.wait_visible("#export-link");

        ui.click("a[data-nav=keys]");
        ui.wait_visible("[data-test=key-console-smoke]");

        ui.click("a[data-nav=settings]");
        ui.wait_visible("#settings-table");
        ui.wait_visible("#mail-queue-table");
    })
    .await;
}

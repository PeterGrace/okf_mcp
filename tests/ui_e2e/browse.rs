//! Browse and Search (M9 Task 4): the tree, a rendered concept and a link
//! in its body, raw HTML shown as text, the Links and History tabs with a
//! revision's diff, and search.

use chrono::Utc;
use okf_mcp::archive::Files;
use okf_mcp::config::Config;
use okf_mcp::store::import::{import, ImportOptions, ImportScope};
use okf_mcp::store::keys::Access;
use serde_json::json;

use crate::harness::drive;
use crate::support::server::{files, TestServer};

/// `systems/alpha.md` as first imported.
const ALPHA: &str = "---\ntype: system\ntitle: Alpha\n---\n# Alpha\n\n\
                     Alpha runs [Beta](beta.md).\n\n## Config\n\nNothing yet.\n";

/// A concept that tries to run script three ways.
const HOSTILE: &str = "---\ntype: note\ntitle: Hostile\n---\n# Hostile\n\n\
                       <script>window.owned = true</script>\n\n\
                       Inline <img src=x onerror=\"window.owned = true\"> and \
                       [a link](javascript:window.owned=true).\n";

/// The synthetic `main` catalog, with `alpha` as given.
fn main_catalog(alpha: &str) -> Files {
    files(&[
        (
            "index.md",
            "# Main\n\n- [Alpha](systems/alpha.md): the first system\n",
        ),
        ("log.md", "# Log\n"),
        ("AGENTS.md", "# Agents\n"),
        (
            "systems/index.md",
            "# Systems\n\n- [Alpha](alpha.md)\n- [Beta](beta.md)\n",
        ),
        ("systems/alpha.md", alpha),
        (
            "systems/beta.md",
            "---\ntype: system\ntitle: Beta\n---\n# Beta\n\nBeta.\n",
        ),
        ("notes/hostile.md", HOSTILE),
    ])
}

/// A server holding `main` and a read key on it.
async fn server() -> (TestServer, String) {
    let server = TestServer::start(vec![("main", main_catalog(ALPHA))], Config::default()).await;
    let key = server
        .key("browse-reader", Access::Read, &["main"], Some("main"))
        .await;
    (server, key)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_tree_opens_a_concept_and_a_link_in_its_body_opens_the_next() {
    let (server, key) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.open("/browse/main");
        ui.wait_text("#concept-title", "/index.md");
        ui.wait_visible("#markdown-body");
        let folder_open = |ui: &crate::harness::Ui| {
            ui.eval(
                "() => document.querySelector(\"[data-folder='systems']\").open",
                json!(null),
            )
        };
        assert_eq!(folder_open(ui), json!(false), "folders start closed");
        ui.click("[data-folder='systems'] > summary");
        assert_eq!(folder_open(ui), json!(true));
        ui.click("[data-path='/systems/alpha.md']");
        ui.wait_text("#concept-title", "Alpha");
        ui.wait_visible("#markdown-body h2[data-slug='config']");
        assert_eq!(ui.text("#markdown-body h1"), "Alpha");
        assert!(ui.text("#provenance").contains("0 sources"));
        assert!(
            !ui.text("#markdown-body").contains("type: system"),
            "no frontmatter"
        );
        ui.click("#markdown-body a[href='#/browse/main/systems/beta.md']");
        ui.wait_text("#concept-title", "Beta");
        assert_eq!(
            ui.eval("() => location.hash", json!(null)),
            json!("#/browse/main/systems/beta.md")
        );
        // A section route opens the concept at that heading.
        ui.open("/browse/main/systems/alpha.md/config");
        ui.wait_text("#concept-title", "Alpha");
        ui.wait_visible("#outline a[href='#/browse/main/systems/alpha.md/config']");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn raw_html_and_script_links_in_a_concept_render_as_text() {
    let (server, key) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.open("/browse/main/notes/hostile.md");
        ui.wait_text("#markdown-body", "<script>window.owned = true</script>");
        assert!(ui.text("#markdown-body").contains("<img src=x"));
        let injected = ui.eval(
            "() => document.querySelectorAll('#markdown-body script, #markdown-body img, \
             #markdown-body [onerror], #markdown-body a[href^=\"javascript\"]').length",
            json!(null),
        );
        assert_eq!(injected, json!(0));
        assert_eq!(
            ui.eval("() => window.owned === undefined", json!(null)),
            json!(true)
        );
        // drive() then fails the journey on any console error, CSP
        // violations included.
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_links_tab_shows_backlinks_and_a_revision_opens_its_diff() {
    let (server, key) = server().await;
    // A second revision of alpha, as a replacing import writes it.
    let options = ImportOptions {
        replace: true,
        scope: ImportScope::Everything,
        expect_generation: None,
        actor: "editor",
        audit: &okf_mcp::store::audit::AuditActor::cli(),
        now: Utc::now(),
    };
    let edited = ALPHA.replace("Nothing yet.", "Two settings.");
    import(&server.db.pool, "main", main_catalog(&edited), &options)
        .await
        .expect("import the edit");
    server.app.poll().await.expect("load the edit");
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.open("/browse/main/systems/beta.md");
        ui.wait_text("#concept-title", "Beta");
        ui.click("#concept-tabs [data-value='links']");
        ui.wait_text("#backlinks", "/systems/alpha.md");
        // The row from alpha (CSS cannot select by text, so a script clicks it).
        ui.eval(
            "() => [...document.querySelectorAll('#backlinks tbody tr')]\
             .find(row => row.textContent.includes('/systems/alpha.md')).click()",
            json!(null),
        );
        ui.wait_text("#concept-title", "Alpha");
        ui.wait_text("#markdown-body", "Two settings.");
        ui.click("#concept-tabs [data-value='history']");
        ui.wait_text("#history", "editor");
        ui.click("#history tbody tr:first-child");
        ui.wait_visible("#drawer");
        let diff = ui.wait_text("#revision-diff", "+Two settings.");
        assert!(diff.contains("-Nothing yet."), "{diff}");
        ui.click("#drawer-close");
        ui.wait_hidden("#drawer");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn search_finds_alpha_and_opens_it() {
    let (server, key) = server().await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.open("/search");
        ui.fill("#search-query", "alpha");
        ui.click("#search-form button[type=submit]");
        ui.wait_text("#search-count", "hit");
        assert!(ui
            .text("#search-results")
            .contains("main /systems/alpha.md"));
        ui.click("#search-results a[data-path='/systems/alpha.md']");
        ui.wait_text("#concept-title", "Alpha");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn switching_catalogs_while_browsing_opens_the_new_root_once() {
    let server = TestServer::start(
        vec![
            ("main", main_catalog(ALPHA)),
            ("other", files(&[("index.md", "# Other\n")])),
        ],
        Config::default(),
    )
    .await;
    let key = server
        .key(
            "browse-both",
            Access::Read,
            &["main", "other"],
            Some("main"),
        )
        .await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        // Several Browse renders: the page must not add a switcher listener
        // on each, or one switch would render the new catalog several times.
        for route in [
            "/browse/main",
            "/browse/main/systems/alpha.md",
            "/browse/main/systems/beta.md",
        ] {
            ui.open(route);
            ui.wait_visible("#markdown-body");
        }
        ui.wait_text("#concept-title", "Beta");
        // Count the tree requests for `other`, then switch to it.
        ui.eval(
            "() => { window.otherTrees = 0; const real = window.fetch; \
             window.fetch = (url, opts) => { \
             if (String(url).endsWith('/catalogs/other/tree')) window.otherTrees += 1; \
             return real(url, opts); }; \
             const select = document.querySelector('#catalog-select'); \
             select.value = 'other'; \
             select.dispatchEvent(new Event('change')); }",
            json!(null),
        );
        ui.wait_text("#concept-title", "/index.md");
        ui.wait_text("#markdown-body", "Other");
        assert_eq!(
            ui.eval("() => location.hash", json!(null)),
            json!("#/browse/other")
        );
        assert_eq!(ui.eval("() => window.otherTrees", json!(null)), json!(1));
    })
    .await;
}

/// Where the `deep` heading's top is in the viewport, polled for up to 120
/// frames until it is in view (the page renders after its request and
/// scrolls in a `requestAnimationFrame`); `top` is `null` while it is missing.
const DEEP_TOP: &str = "() => new Promise(done => { let frames = 0; const look = () => { \
    const heading = document.querySelector(\"#markdown-body [data-slug='deep']\"); \
    const top = heading ? heading.getBoundingClientRect().top : null; \
    const truncated = document.querySelector('#concept-truncated') !== null; \
    const inView = top !== null && top >= -2 && top < innerHeight; \
    if ((inView && !truncated) || ++frames > 120) done({ top, truncated, height: innerHeight }); \
    else requestAnimationFrame(look); }; look(); })";

/// Asserts the `deep` heading is rendered, with the whole body, and scrolled
/// into view.
fn assert_deep_in_view(ui: &crate::harness::Ui) {
    ui.wait_visible("#markdown-body h2[data-slug='deep']");
    let place = ui.eval(DEEP_TOP, json!(null));
    assert_eq!(place["truncated"], json!(false), "the whole body: {place}");
    let top = place["top"].as_f64().expect("the heading's top");
    let height = place["height"].as_f64().expect("the viewport height");
    assert!((-2.0..height).contains(&top), "deep is in view: {place}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_section_link_on_a_long_concept_loads_the_body_and_reaches_the_heading() {
    // Enough paragraphs to push `## Deep` well below the first screen.
    let filler = "A paragraph of filler text.\n\n".repeat(120);
    let long = format!(
        "---\ntype: note\ntitle: Long\n---\n# Long\n\n## Top\n\n{filler}## Deep\n\nFound.\n"
    );
    let config = Config {
        concept_outline_chars: 200,
        ..Config::default()
    };
    let server = TestServer::start(
        vec![(
            "main",
            files(&[("index.md", "# Main\n"), ("notes/long.md", long.as_str())]),
        )],
        config,
    )
    .await;
    let key = server
        .key("browse-long", Access::Read, &["main"], Some("main"))
        .await;
    drive(server.base.clone(), move |ui| {
        let deep_link = "#outline a[href='#/browse/main/notes/long.md/deep']";
        ui.login(&key);
        // Over the limit: only the outline, until a section link is followed.
        ui.open("/browse/main/notes/long.md");
        ui.wait_visible("#concept-truncated");
        ui.click(deep_link);
        assert_deep_in_view(ui);
        // After "Load the whole body", a section link keeps the whole body.
        ui.open("/browse/main/notes/long.md");
        ui.wait_visible("#concept-full");
        ui.click("#concept-full");
        ui.wait_visible("#markdown-body");
        ui.click(deep_link);
        assert_deep_in_view(ui);
    })
    .await;
}

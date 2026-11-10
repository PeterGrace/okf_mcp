//! Signing in and out, a refused key, the theme toggle, and what a read key sees.

use okf_mcp::config::Config;
use okf_mcp::store::keys::Access;

use crate::harness::drive;
use crate::support::server::{files, TestServer};

async fn server() -> TestServer {
    TestServer::start(
        vec![("main", files(&[("index.md", "# Main\n")]))],
        Config::default(),
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_signs_in_sees_every_group_and_signs_out() {
    let server = server().await;
    let key = server
        .key("console-admin", Access::Admin, &["main"], Some("main"))
        .await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.wait_text("#whoami", "admin");
        assert_eq!(
            ui.eval(
                "() => document.querySelector('#catalog-select').value",
                serde_json::json!(null)
            ),
            "main"
        );
        assert!(ui.page.is_visible("[data-need=admin]").expect("visible"));
        ui.click("#signout");
        ui.wait_visible("#login");
        // Signed out means the key is gone: a reload stays on the login card.
        ui.open("");
        ui.wait_visible("#login");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wrong_key_is_refused_on_the_login_card() {
    let server = server().await;
    drive(server.base.clone(), |ui| {
        ui.open("");
        ui.wait_visible("#login");
        ui.fill("#login-form input[name=key]", "okf_not-a-real-key");
        ui.click("#login-form button[type=submit]");
        ui.wait_text("#login-error", "not accepted");
        assert!(!ui.page.is_visible("#app").expect("visible"));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_key_sees_no_administer_group_and_stays_signed_in() {
    let server = server().await;
    let key = server
        .key("console-reader", Access::Read, &["main"], Some("main"))
        .await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.wait_text("#whoami", "read");
        assert!(!ui.page.is_visible("[data-need=admin]").expect("visible"));
        ui.open("");
        ui.wait_visible("#app"); // localStorage kept the key across the reload
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sign_in_works_when_storage_throws() {
    let server = server().await;
    let key = server
        .key("console-nostorage", Access::Read, &["main"], Some("main"))
        .await;
    drive(server.base.clone(), move |ui| {
        ui.open("");
        ui.wait_visible("#login");
        // Every storage call now throws, as with blocked site data.
        ui.eval(
            "() => { for (const m of ['getItem', 'setItem', 'removeItem']) \
             Storage.prototype[m] = () => { throw new DOMException('denied', 'SecurityError'); }; }",
            serde_json::json!(null),
        );
        ui.fill("#login-form input[name=key]", &key);
        ui.click("#login-form button[type=submit]");
        ui.wait_visible("#app");
        ui.wait_text("#whoami", "read");
        ui.click("#theme-toggle"); // a preference write must not throw either
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_slow_page_render_never_lands_in_a_newer_page() {
    let server = server().await;
    let key = server
        .key("console-router", Access::Read, &["main"], Some("main"))
        .await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        // Two navigations through the real router: the first render is
        // still awaiting when the second runs, then appends late.
        let result = ui.eval(
            "async () => { \
               const router = await import('/ui/js/core/router.js'); \
               let release; \
               const slow = router.mount(async view => { \
                 await new Promise(resolve => { release = resolve; }); \
                 view.append(Object.assign(document.createElement('p'), { id: 'stale' })); }, []); \
               await router.mount(async view => { \
                 view.append(Object.assign(document.createElement('p'), { id: 'fresh' })); }, []); \
               release(); await slow; \
               return [!!document.getElementById('stale'), !!document.getElementById('fresh')]; }",
            serde_json::json!(null),
        );
        assert_eq!(result, serde_json::json!([false, true]));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_theme_toggle_flips_and_persists() {
    let server = server().await;
    let key = server
        .key("console-theme", Access::Read, &["main"], Some("main"))
        .await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        let theme = |ui: &crate::harness::Ui| {
            ui.eval(
                "() => document.documentElement.dataset.theme",
                serde_json::json!(null),
            )
        };
        let before = theme(ui);
        ui.click("#theme-toggle");
        let after = theme(ui);
        assert_ne!(before, after);
        ui.open("");
        ui.wait_visible("#app");
        assert_eq!(theme(ui), after);
    })
    .await;
}

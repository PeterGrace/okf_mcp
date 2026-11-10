//! The API keys page: create a key and see its secret once, sign in with
//! it, revoke the signed-in key, and what a read key sees.

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
async fn an_admin_creates_a_key_sees_its_secret_once_and_signs_in_with_it() {
    let server = server().await;
    let admin = server
        .key("console-admin", Access::Admin, &["main"], Some("main"))
        .await;
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/keys");
        ui.wait_visible("[data-test=key-console-admin]");
        ui.click("#key-new");
        ui.fill("#key-form input[name=name]", "console-made");
        ui.click("#key-form input[name=catalog][value=main]");
        ui.click("#key-form button[type=submit]");
        ui.wait_visible("#key-secret");
        let secret = ui.text("#key-secret");
        assert!(secret.starts_with("okf_"), "a key, not {secret:?}");
        ui.click("#key-secret-done");
        ui.wait_hidden("#key-secret");
        ui.wait_visible("[data-test=key-console-made]");
        // Once dismissed, the secret is nowhere on the page.
        let page = ui.eval("() => document.body.textContent", serde_json::json!(null));
        assert!(!page.as_str().unwrap_or_default().contains(&secret));
        ui.click("#signout");
        ui.wait_visible("#login");
        ui.login(&secret);
        ui.wait_text("#whoami", "console-made");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn revoking_the_signed_in_key_returns_the_browser_to_login() {
    let server = server().await;
    let key = server
        .key("console-self", Access::Admin, &["main"], Some("main"))
        .await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.open("/keys");
        ui.click("[data-test=key-console-self]");
        ui.wait_visible("#key-revoke");
        ui.click("#key-revoke");
        ui.click("#confirm-ok");
        // The list reload after the revoke is the key's next request: a 401.
        ui.wait_visible("#login");
        ui.wait_text("#login-error", "refused");
        ui.open("");
        ui.wait_visible("#login");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_key_never_sees_the_keys_page() {
    let server = server().await;
    let key = server
        .key("console-reader", Access::Read, &["main"], Some("main"))
        .await;
    drive(server.base.clone(), move |ui| {
        ui.login(&key);
        ui.wait_text("#whoami", "read");
        assert!(!ui.page.is_visible("a[data-nav=keys]").expect("visible"));
        // Typed in by hand, the page explains instead of calling the API.
        ui.open("/keys");
        ui.wait_text("#main", "needs an admin key");
    })
    .await;
}

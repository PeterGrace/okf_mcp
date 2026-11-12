//! The Users page (M11 Task 8): create a user (and see its personal team),
//! open its detail and delete it; the delete a held key refuses; and what an
//! admin that does not reach every catalog may do.

use okf_mcp::config::Config;
use okf_mcp::store::keys::Access;

use crate::harness::drive;
use crate::support::server::{files, TestServer, OWNER};

async fn server() -> TestServer {
    TestServer::start(
        vec![("main", files(&[("index.md", "# Main\n")]))],
        Config::default(),
    )
    .await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_creates_a_user_and_deletes_it() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/users");
        ui.wait_visible(&format!("[data-test=user-{OWNER}]"));
        ui.click("#user-new");
        ui.fill("#user-form input[name=name]", "alice");
        ui.fill("#user-form input[name=display_name]", "Alice Example");
        ui.click("#user-form button[type=submit]");
        ui.wait_text("[data-test=user-alice]", "alice");
        ui.click("[data-test=user-alice]");
        ui.wait_text("#user-detail", "Personal team: alice");
        ui.click("#user-delete");
        ui.click("#confirm-ok");
        ui.wait_hidden("[data-test=user-alice]");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_user_holding_a_key_shows_why_it_cannot_be_deleted() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/users");
        // The fixture user holds console-admin itself.
        ui.click(&format!("[data-test=user-{OWNER}]"));
        ui.wait_text("#user-keys", "console-admin");
        ui.wait_visible("#user-delete[disabled]");
        ui.wait_text("#user-delete-reason", "key");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scoped_admin_lists_users_but_cannot_change_them() {
    let server = server().await;
    let scoped = server
        .key("console-scoped", Access::Admin, &["main"], Some("main"))
        .await;
    drive(server.base.clone(), move |ui| {
        ui.login(&scoped);
        // No badge: the unassigned listing needs every catalog.
        ui.wait_text("#whoami", "console-scoped");
        assert!(!ui.page.is_visible("#unassigned-badge").expect("visible"));
        ui.open("/users");
        ui.wait_visible(&format!("[data-test=user-{OWNER}]"));
        ui.wait_visible("#user-new[disabled]");
        ui.open("/users/unassigned");
        ui.wait_text("#unassigned-denied", "every catalog");
    })
    .await;
}

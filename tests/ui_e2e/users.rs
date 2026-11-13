//! The Users page (M11 Task 8): create a user (and see its personal team),
//! open its detail and delete it; the deletes a held key, another team or
//! a catalog its personal team owns refuse, and that clearing the cause
//! enables Delete; and what an admin that does not reach every catalog may
//! do.

use chrono::Utc;
use okf_mcp::config::Config;
use okf_mcp::store::catalogs;
use okf_mcp::store::keys::Access;
use okf_mcp::store::teams::{self, NewTeam, Role};
use okf_mcp::store::users::{self, NewUser};

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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn another_team_or_an_owned_catalog_refuses_the_delete_until_cleared() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    let pool = &server.db.pool;
    // Neither user holds a credential: alice is in a shared team, and bob's
    // personal team owns a catalog.
    for name in ["alice", "bob"] {
        let user = NewUser {
            name,
            display_name: "",
        };
        users::create(pool, &user, Utc::now())
            .await
            .expect("create a user");
    }
    let family = NewTeam {
        name: "family",
        display_name: "",
        owner: OWNER,
    };
    teams::create(pool, &family, Utc::now())
        .await
        .expect("create a team");
    teams::set_member(pool, "family", "alice", Role::Member, Utc::now())
        .await
        .expect("add alice");
    catalogs::create(pool, "bobs", "", "bob", Utc::now())
        .await
        .expect("create bob's catalog");
    server.app.poll().await.expect("poll");
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        // alice: another team.
        ui.open("/users");
        ui.click("[data-test=user-alice]");
        ui.wait_visible("#user-delete[disabled]");
        ui.wait_text("#user-delete-reason", "belongs to other teams: family");
        ui.click("#drawer-close");
        ui.open("/teams");
        ui.click("[data-test=team-family]");
        ui.click("[data-test=member-remove-alice]");
        ui.wait_hidden("[data-test=member-remove-alice]");
        ui.click("#drawer-close");
        ui.open("/users");
        ui.click("[data-test=user-alice]");
        ui.wait_visible("#user-delete:not([disabled])");
        assert!(!ui.page.is_visible("#user-delete-reason").expect("visible"));
        ui.click("#drawer-close");
        // bob: a catalog his personal team owns.
        ui.click("[data-test=user-bob]");
        ui.wait_visible("#user-delete[disabled]");
        ui.wait_text("#user-delete-reason", "personal team owns catalogs: bobs");
        ui.click("#drawer-close");
        ui.open("/catalogs");
        ui.click("[data-catalog=bobs]");
        ui.select("#catalog-owner-select", OWNER);
        ui.click("#catalog-owner-save");
        ui.wait_text("[data-test=catalog-owner-bobs]", OWNER);
        ui.open("/users");
        ui.click("[data-test=user-bob]");
        ui.wait_visible("#user-delete:not([disabled])");
        assert!(!ui.page.is_visible("#user-delete-reason").expect("visible"));
    })
    .await;
}

/// The text nodes directly inside the page's view: a conditional `null`
/// passed to the DOM's own `append` shows up here as `"null"` (issue #302).
const STRAY_TEXT: &str = "() => [...document.querySelector('#main .view').childNodes] \
    .filter(node => node.nodeType === Node.TEXT_NODE).map(node => node.textContent)";

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_page_prints_a_stray_null_under_its_content() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    let writer = server
        .key("console-writer", Access::Write, &["main"], Some("main"))
        .await;
    drive(server.base.clone(), move |ui| {
        // An admin reaching every catalog: no "needs every catalog" note
        // on Users, Teams or Catalogs.
        ui.login(&admin);
        for (route, loaded) in [
            ("/users", format!("[data-test=user-{OWNER}]")),
            ("/teams", format!("[data-test=team-{OWNER}]")),
            ("/catalogs", "code[data-catalog=main]".to_string()),
        ] {
            ui.open(route);
            ui.wait_visible(&loaded);
            assert_eq!(
                ui.eval(STRAY_TEXT, serde_json::json!(null)),
                serde_json::json!([]),
                "{route}"
            );
        }
        ui.click("#signout");
        ui.wait_visible("#login");
        // A write key: no import panel on Import & export.
        ui.login(&writer);
        ui.open("/transfer");
        ui.wait_visible("#export-panel");
        assert!(!ui.page.is_visible("#import-panel").expect("visible"));
        assert_eq!(
            ui.eval(STRAY_TEXT, serde_json::json!(null)),
            serde_json::json!([])
        );
    })
    .await;
}

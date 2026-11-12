//! The Teams page and the unassigned view (M11 Task 8): the users and teams
//! design's journey (a team, a member, a catalog it owns, a key for the
//! member), membership changes and team deletion (the last-owner refusal,
//! Remove, Delete, and what a personal team keeps), and assigning a key
//! from before users existed.

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
async fn create_team_add_member_create_owned_catalog_and_key_for_user() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        // A user to add.
        ui.open("/users");
        ui.click("#user-new");
        ui.fill("#user-form input[name=name]", "alice");
        ui.click("#user-form button[type=submit]");
        ui.wait_visible("[data-test=user-alice]");
        // A team owned by the fixture user, with alice as a member.
        ui.open("/teams");
        ui.wait_visible(&format!("[data-test=team-{OWNER}]"));
        ui.click("#team-new");
        ui.fill("#team-form input[name=name]", "family");
        ui.select("#team-form select[name=owner]", OWNER);
        ui.click("#team-form button[type=submit]");
        ui.click("[data-test=team-family]");
        ui.wait_text("#team-members", OWNER);
        ui.select("#member-form select[name=user]", "alice");
        ui.click("#member-form button[type=submit]");
        ui.wait_text("#team-members", "alice");
        // alice becomes an owner too, so the fixture user could leave.
        ui.select("[data-test=member-role-alice]", "owner");
        ui.wait_text("#toasts", "alice is now owner of family");
        ui.click("#drawer-close");
        // A catalog the team owns.
        ui.open("/catalogs");
        ui.click("#catalog-create");
        ui.fill("#catalog-create-form input[name=name]", "shared");
        ui.select("#catalog-create-form select[name=owner]", "family");
        ui.click("#catalog-create-form button[type=submit]");
        ui.wait_text("[data-catalog=shared]", "shared");
        ui.wait_text("[data-test=catalog-owner-shared]", "family");
        // The team now names the catalog, and cannot be deleted.
        ui.open("/teams");
        ui.click("[data-test=team-family]");
        ui.wait_visible("#team-delete[disabled]");
        ui.wait_text("#team-delete-reason", "shared");
        ui.click("#drawer-close");
        // A key for alice.
        ui.open("/keys");
        ui.click("#key-new");
        ui.fill("#key-form input[name=name]", "alice-laptop");
        ui.select("#key-form select[name=user]", "alice");
        ui.click("#key-form input[name=catalog][value=shared]");
        ui.click("#key-form button[type=submit]");
        ui.click("#key-secret-done");
        ui.wait_text("[data-test=key-user-alice-laptop]", "alice");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_legacy_key_is_assigned_from_the_unassigned_view() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    server.legacy_key("legacy").await;
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.wait_text("#unassigned-badge", "1");
        ui.open("/users/unassigned");
        ui.select("[data-test=assign-key-legacy] select", OWNER);
        ui.click("[data-test=assign-key-legacy] button");
        ui.wait_text("#unassigned-empty", "Nothing is unassigned");
        ui.wait_hidden("#unassigned-badge");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_last_owner_is_kept_then_replaced_removed_and_the_team_deleted() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/users");
        ui.click("#user-new");
        ui.fill("#user-form input[name=name]", "alice");
        ui.click("#user-form button[type=submit]");
        ui.wait_visible("[data-test=user-alice]");
        // A personal team keeps its own user and cannot be deleted.
        ui.open("/teams");
        ui.click(&format!("[data-test=team-{OWNER}]"));
        ui.wait_visible(&format!("[data-test=member-remove-{OWNER}][disabled]"));
        ui.wait_visible("#team-delete[disabled]");
        ui.wait_text("#team-delete-reason", "personal team");
        ui.click("#drawer-close");
        // A shared team, owned by the fixture user alone.
        ui.click("#team-new");
        ui.fill("#team-form input[name=name]", "family");
        ui.select("#team-form select[name=owner]", OWNER);
        ui.click("#team-form button[type=submit]");
        ui.click("[data-test=team-family]");
        let owner_role = format!("[data-test=member-role-{OWNER}]");
        ui.wait_visible(&owner_role);
        // Demoting the last owner is refused, and the select shows the role
        // it kept.
        ui.select(&owner_role, "member");
        ui.wait_text("#toasts", "last owner");
        let role = ui.eval(
            "(selector) => document.querySelector(selector).value",
            serde_json::json!(owner_role),
        );
        assert_eq!(role, serde_json::json!("owner"));
        // Recovery: alice joins as an owner, then the fixture user leaves.
        ui.select("#member-form select[name=user]", "alice");
        ui.select("#member-form select[name=role]", "owner");
        ui.click("#member-form button[type=submit]");
        ui.wait_text("#team-members", "alice");
        ui.click(&format!("[data-test=member-remove-{OWNER}]"));
        ui.wait_hidden(&owner_role);
        ui.wait_text("#team-members", "alice");
        // A shared team that owns no catalog is deleted.
        ui.click("#team-delete");
        ui.click("#confirm-ok");
        ui.wait_hidden("[data-test=team-family]");
    })
    .await;
}

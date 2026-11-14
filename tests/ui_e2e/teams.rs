//! The Teams page and the unassigned view (M11 Task 8): the users and teams
//! design's journey (a team, a member, a catalog it owns, a key for the
//! member), membership changes and team deletion (the last-owner refusal,
//! Remove, Delete, and what a personal team keeps), and assigning a key
//! from before users existed, and an OAuth grant, a sign-in code and a
//! catalog.

use chrono::{TimeDelta, Utc};
use okf_mcp::config::Config;
use okf_mcp::store::keys::{self as key_rows, Access, Grants};
use okf_mcp::store::oauth::clients;
use okf_mcp::store::oauth::codes::{self, NewSignInCode};
use okf_mcp::store::teams::{self, NewTeam};
use okf_mcp::store::users::{self, NewUser};
use serde_json::json;

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
    server.legacy_key("legacy", Access::Read).await;
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn assigning_the_signed_in_key_from_the_unassigned_view_updates_the_shell_identity() {
    let server = server().await;
    let legacy = server.legacy_key("legacy-admin", Access::Admin).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&legacy);
        ui.wait_text("#whoami", "legacy-admin - admin");
        assert!(!ui.text("#whoami").contains(" / "), "no user yet");
        ui.wait_text("#unassigned-badge", "1");
        ui.open("/users/unassigned");
        ui.select("[data-test=assign-key-legacy-admin] select", OWNER);
        ui.click("[data-test=assign-key-legacy-admin] button");
        ui.wait_text("#unassigned-empty", "Nothing is unassigned");
        // The shell follows without a reload.
        ui.wait_text("#whoami", &format!("{OWNER} / legacy-admin"));
        ui.open("/users");
        ui.wait_text(&format!("[data-test=user-{OWNER}]"), "you");
    })
    .await;
}

/// A sign-in code called `name` on `main`, for the fixture user.
fn new_code<'a>(name: &'a str, main: &'a [String]) -> NewSignInCode<'a> {
    NewSignInCode {
        name,
        access: Access::Read,
        grants: Grants::Catalogs(main),
        default_catalog: None,
        actor: "",
        expires_at: Utc::now() + TimeDelta::hours(1),
        user: OWNER,
    }
}

/// Seeds what a deployment from before users existed holds: an OAuth grant
/// and a sign-in code with no user, and a catalog (`orphan`) with no owner.
/// No store function makes those any more, so each is made the current way
/// and its user or owner cleared with SQL; the admin generation is bumped
/// so the poll reloads the keys and catalogs.
///
/// # Returns
/// The grant's name.
async fn seed_unassigned(server: &TestServer) -> String {
    let pool = &server.db.pool;
    let client_id = "https://client.example.com/client.json";
    sqlx::query(
        "INSERT INTO oauth_clients \
         (client_id, kind, client_name, redirect_uris, metadata, fetched_at, cache_expires_at, \
          created_at) \
         VALUES ($1, 'cimd', 'Example Client', ARRAY['https://client.example.com/callback'], \
                 '{}'::jsonb, now(), now() + interval '1 hour', now())",
    )
    .bind(client_id)
    .execute(pool)
    .await
    .expect("insert a client");
    let client = clients::find(pool, client_id)
        .await
        .expect("read the client")
        .expect("the client exists");
    let main = ["main".to_string()];
    let redeemed = codes::create(pool, &new_code("laptop", &main), Utc::now())
        .await
        .expect("mint the code a grant comes from");
    let grant_id = codes::redeem(pool, &redeemed.id, &client, Utc::now())
        .await
        .expect("redeem the code");
    codes::create(pool, &new_code("tablet", &main), Utc::now())
        .await
        .expect("mint a code");
    sqlx::query("UPDATE api_keys SET user_id = NULL WHERE id = $1")
        .bind(&grant_id)
        .execute(pool)
        .await
        .expect("clear the grant's user");
    let statements = [
        "UPDATE sign_in_codes SET user_id = NULL WHERE name = 'tablet'",
        "UPDATE catalogs SET owner_team_id = NULL WHERE name = 'orphan'",
        "UPDATE server_state SET admin_generation = admin_generation + 1",
    ];
    for statement in statements {
        sqlx::query(statement)
            .execute(pool)
            .await
            .expect("clear a user or owner");
    }
    server.app.poll().await.expect("poll");
    key_rows::list(pool)
        .await
        .expect("list keys")
        .into_iter()
        .find(|key| key.id == grant_id)
        .expect("the grant")
        .name
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_grant_a_code_and_a_catalog_are_assigned_from_the_unassigned_view() {
    let server = TestServer::start(
        vec![
            ("main", files(&[("index.md", "# Main\n")])),
            ("orphan", files(&[("index.md", "# Orphan\n")])),
        ],
        Config::default(),
    )
    .await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    let alice = NewUser {
        name: "alice",
        display_name: "",
    };
    users::create(&server.db.pool, &alice, Utc::now())
        .await
        .expect("create alice");
    let family = NewTeam {
        name: "family",
        display_name: "",
        owner: "alice",
    };
    teams::create(&server.db.pool, &family, Utc::now())
        .await
        .expect("create a team");
    let grant = seed_unassigned(&server).await;
    // Quoted: a grant's name has dots.
    let grant_row = format!("[data-test=\"assign-key-{grant}\"]");
    let ran = drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.wait_text("#unassigned-badge", "3");
        ui.open("/users/unassigned");
        ui.wait_text(&grant_row, "Grant");
        ui.select(&format!("{grant_row} select"), "alice");
        ui.click(&format!("{grant_row} button"));
        ui.wait_hidden(&grant_row);
        ui.wait_text("#unassigned-badge", "2");
        ui.select("[data-test=assign-code-tablet] select", "alice");
        ui.click("[data-test=assign-code-tablet] button");
        ui.wait_hidden("[data-test=assign-code-tablet]");
        ui.wait_text("#unassigned-badge", "1");
        ui.select("[data-test=assign-catalog-orphan] select", "family");
        ui.click("[data-test=assign-catalog-orphan] button");
        ui.wait_text("#unassigned-empty", "Nothing is unassigned");
        ui.wait_hidden("#unassigned-badge");
        // The list pages show what was persisted.
        ui.open("/catalogs");
        ui.wait_text("[data-test=catalog-owner-orphan]", "family");
        ui.open("/keys");
        ui.wait_text("[data-test=code-user-tablet]", "alice");
        let users = ui.eval(
            "() => [...document.querySelectorAll('[data-test^=\"key-user-\"]')] \
             .map(cell => cell.textContent)",
            json!(null),
        );
        assert!(
            !users.to_string().contains("unassigned"),
            "every key has a user: {users}"
        );
    })
    .await;
    if !ran {
        return;
    }
    let pool = &server.db.pool;
    let keys = key_rows::list(pool).await.expect("list keys");
    let grant_user = keys
        .iter()
        .find(|key| key.name == grant)
        .and_then(|key| key.user.clone());
    assert_eq!(grant_user.as_deref(), Some("alice"));
    let code_user = codes::list(pool)
        .await
        .expect("list codes")
        .into_iter()
        .find(|code| code.name == "tablet")
        .and_then(|code| code.user);
    assert_eq!(code_user.as_deref(), Some("alice"));
    let owned = teams::find(pool, "family").await.expect("read the team");
    assert_eq!(owned.catalogs, ["orphan"]);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_team_is_suspended_with_a_notice_shown_on_its_catalogs_then_lifted() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    let ran = drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/teams");
        ui.click(&format!("[data-test=team-{OWNER}]"));
        ui.fill(
            "#team-suspend-form textarea[name=notice]",
            "Paused for review.",
        );
        ui.click("#team-suspend");
        ui.click("#confirm-ok");
        ui.wait_text("#toasts", &format!("Suspended team {OWNER}."));
        ui.wait_text("#team-suspension", "Paused for review.");
        ui.wait_visible(&format!("[data-test=team-suspended-{OWNER}]"));
        ui.click("#drawer-close");
        // Its catalog is marked, with the notice as the pill's title.
        ui.open("/catalogs");
        ui.wait_visible("[data-test=catalog-suspended-main]");
        assert_eq!(
            ui.eval(
                "() => document.querySelector('[data-test=catalog-suspended-main]').title",
                json!(null)
            ),
            json!("Paused for review.")
        );
        ui.open("/teams");
        ui.click(&format!("[data-test=team-{OWNER}]"));
        ui.click("#team-unsuspend");
        ui.wait_text(
            "#toasts",
            &format!("Lifted the suspension of team {OWNER}."),
        );
        ui.wait_hidden(&format!("[data-test=team-suspended-{OWNER}]"));
        // Suspended without a notice: no notice paragraph, and never the
        // text "null" (core/dom.js, issue #302).
        ui.click("#team-suspend");
        ui.click("#confirm-ok");
        ui.wait_text("#team-suspension", "Suspended since");
        ui.wait_hidden("#team-suspension-notice");
        ui.wait_text_gone("#team-suspension", "null");
        ui.wait_visible(&format!("[data-test=team-suspended-{OWNER}]"));
    })
    .await;
    if !ran {
        return;
    }
    let team = teams::find(&server.db.pool, OWNER)
        .await
        .expect("read the team");
    let suspension = team.suspension.expect("suspended again");
    assert_eq!(suspension.notice, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_sees_the_notice_on_browse_with_a_link_to_export() {
    let server = server().await;
    let reader = server
        .key("member-reader", Access::Read, &["main"], Some("main"))
        .await;
    teams::suspend(
        &server.db.pool,
        OWNER,
        Some("Paused for review."),
        Utc::now(),
    )
    .await
    .expect("suspend the owner team");
    server.app.poll().await.expect("poll");
    drive(server.base.clone(), move |ui| {
        ui.login(&reader);
        ui.open("/browse/main");
        ui.wait_visible("#browse-suspended");
        ui.wait_text("#browse-suspended", "Paused for review.");
        // The 403's own message is toasted, not "cannot do this".
        ui.wait_text("#toasts", "is suspended");
        ui.wait_text_gone("#toasts", "cannot do this");
        // Export still works from the page the notice links to.
        ui.click("#browse-suspended a[href='#/transfer/main']");
        ui.wait_visible("#export-btn");
        ui.click("#export-btn");
        ui.wait_visible("#export-link");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_suspension_write_runs_once_and_settles_in_a_reopened_drawer() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    let team_row = format!("[data-test=team-{OWNER}]");
    let ran = drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/teams");
        ui.click(&team_row);
        // Suspend, holding the write: Suspend is disabled while it runs,
        // and in the drawer closed and opened again meanwhile too.
        ui.fill("#team-suspend-form textarea[name=notice]", "Held.");
        ui.hold_next("PUT", "/suspension");
        ui.click("#team-suspend");
        ui.click("#confirm-ok");
        ui.wait_visible("#team-suspend[disabled]");
        ui.click("#drawer-close");
        ui.click(&team_row);
        ui.wait_visible("#team-suspend-form textarea[disabled]");
        assert!(
            ui.click_twice("#team-suspend"),
            "the reopened Suspend is not disabled"
        );
        assert_eq!(ui.held_matches(), 1, "a second suspend was sent");
        // Once it settles, the reopened drawer shows the suspension.
        ui.release_held();
        ui.wait_text("#team-suspension-notice", "Held.");
        ui.wait_visible("#team-unsuspend:not([disabled])");
        // Lift, held: a double click sends one request, and so does the
        // button of the drawer opened again while it runs.
        ui.hold_next("PUT", "/suspension");
        assert!(ui.click_twice("#team-unsuspend"));
        assert_eq!(ui.held_matches(), 1, "a second lift was sent");
        ui.mark("#team-unsuspend");
        ui.click("#drawer-close");
        ui.click(&team_row);
        ui.wait_redrawn("#team-unsuspend");
        assert!(
            ui.click_twice("#team-unsuspend"),
            "the redrawn Lift is not disabled"
        );
        assert_eq!(ui.held_matches(), 1, "the redrawn Lift sent a second lift");
        ui.release_held();
        ui.wait_text(
            "#toasts",
            &format!("Lifted the suspension of team {OWNER}."),
        );
        ui.wait_visible("#team-suspend:not([disabled])");
    })
    .await;
    if !ran {
        return;
    }
    let team = teams::find(&server.db.pool, OWNER)
        .await
        .expect("read the team");
    assert_eq!(team.suspension, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_lift_keeps_its_reason_and_a_late_read_never_draws_over_a_newer_one() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    teams::suspend(&server.db.pool, OWNER, Some("Paused."), Utc::now())
        .await
        .expect("suspend the owner team");
    server.app.poll().await.expect("poll");
    let ran = drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/teams");
        ui.click(&format!("[data-test=team-{OWNER}]"));
        ui.wait_visible("#team-unsuspend:not([disabled])");
        // Refused, and reading the team again fails too: the reason shows,
        // the drawer is marked out of date, and Lift can be tried again.
        // No console control gets a full admin's lift refused, so the
        // page's fetch answers this one.
        let team_path = format!("/teams/{OWNER}");
        ui.fail_next("PUT", "/suspension", 409, "refused by the test");
        ui.fail_next("GET", &team_path, 503, "the server is unavailable");
        ui.click("#team-unsuspend");
        ui.wait_visible("#team-stale");
        ui.wait_visible("#team-suspension-error");
        ui.wait_text("#team-suspension-error", "refused by the test");
        ui.wait_visible("#team-unsuspend:not([disabled])");
        // Refused again, and its read answers late, after a lift that
        // succeeded: the late answer (still suspended) is dropped.
        ui.fail_next("PUT", "/suspension", 409, "refused again");
        ui.hold_answer("GET", &team_path, || ui.click("#team-unsuspend"));
        ui.click("#team-unsuspend");
        ui.wait_text(
            "#toasts",
            &format!("Lifted the suspension of team {OWNER}."),
        );
        ui.wait_visible("#team-suspend-form");
        ui.wait_hidden("#team-stale");
        ui.wait_hidden("#team-suspension-error");
        ui.release_held();
        // release_held gave the page time to draw the late answer.
        let section = ui.text("#team-suspension");
        assert!(
            section.contains("Active") && !section.contains("Suspended since"),
            "the late read drew the team as suspended: {section}"
        );
        assert!(!ui.text("#drawer").contains("refused again"));
    })
    .await;
    if !ran {
        return;
    }
    let team = teams::find(&server.db.pool, OWNER)
        .await
        .expect("read the team");
    assert_eq!(team.suspension, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_written_suspension_shows_when_reading_the_users_again_fails() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    let ran = drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/teams");
        ui.click(&format!("[data-test=team-{OWNER}]"));
        // The suspend succeeds, and the drawer's read of the users after it
        // fails: the drawer shows the suspension the PUT answered, noting
        // only that the users may be out of date.
        ui.fill("#team-suspend-form textarea[name=notice]", "Paused.");
        ui.fail_next("GET", "/api/v1/users", 503, "the server is unavailable");
        ui.click("#team-suspend");
        ui.click("#confirm-ok");
        ui.wait_text("#toasts", &format!("Suspended team {OWNER}."));
        ui.wait_text("#team-suspension-notice", "Paused.");
        ui.wait_visible("#team-unsuspend:not([disabled])");
        ui.wait_visible("#team-stale");
        ui.wait_text("#team-stale", "users");
        // The same for a lift.
        ui.fail_next("GET", "/api/v1/users", 503, "the server is unavailable");
        ui.click("#team-unsuspend");
        ui.wait_text(
            "#toasts",
            &format!("Lifted the suspension of team {OWNER}."),
        );
        ui.wait_visible("#team-suspend-form");
        ui.wait_visible("#team-stale");
        // The next good read clears the note.
        ui.click("#drawer-close");
        ui.click(&format!("[data-test=team-{OWNER}]"));
        ui.wait_visible("#team-suspend-form");
        ui.wait_hidden("#team-stale");
    })
    .await;
    if !ran {
        return;
    }
    let team = teams::find(&server.db.pool, OWNER)
        .await
        .expect("read the team");
    assert_eq!(team.suspension, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_older_writes_late_list_read_never_redraws_over_a_newer_write() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    let team_row = format!("[data-test=team-{OWNER}]");
    let ran = drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/teams");
        ui.click(&team_row);
        // The suspend succeeds; the list read after it answers late.
        ui.fill("#team-suspend-form textarea[name=notice]", "First.");
        ui.hold_answer("GET", "/api/v1/teams", || {
            ui.click("#team-suspend");
            ui.click("#confirm-ok");
        });
        // Meanwhile the drawer is opened again (it reads the suspension),
        // and a lift succeeds and is drawn.
        ui.click("#drawer-close");
        ui.click(&team_row);
        ui.wait_visible("#team-unsuspend:not([disabled])");
        ui.click("#team-unsuspend");
        ui.wait_text(
            "#toasts",
            &format!("Lifted the suspension of team {OWNER}."),
        );
        ui.wait_visible("#team-suspend-form");
        // The suspend's late list answer must not redraw it as suspended.
        ui.release_held();
        let section = ui.text("#team-suspension");
        assert!(
            section.contains("Active") && !section.contains("Suspended since"),
            "the older suspend redrew the drawer: {section}"
        );
    })
    .await;
    if !ran {
        return;
    }
    let team = teams::find(&server.db.pool, OWNER)
        .await
        .expect("read the team");
    assert_eq!(team.suspension, None);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_settling_for_one_team_never_cancels_opening_another() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    let family = NewTeam {
        name: "family",
        display_name: "",
        owner: OWNER,
    };
    teams::create(&server.db.pool, &family, Utc::now())
        .await
        .expect("create a second team");
    server.app.poll().await.expect("poll");
    let ran = drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/teams");
        ui.click(&format!("[data-test=team-{OWNER}]"));
        // A's suspend is held; A's drawer is closed, and B's drawer is
        // asked for with its read held too.
        ui.hold_next("PUT", "/suspension");
        ui.click("#team-suspend");
        ui.click("#confirm-ok");
        ui.eval(
            "() => { window.releaseFirst = window.releaseHeld; }",
            json!(null),
        );
        ui.click("#drawer-close");
        ui.hold_next("GET", "/teams/family");
        ui.click("[data-test=team-family]");
        // A's suspend settles first, then B's read answers: B opens.
        ui.eval("() => window.releaseFirst()", json!(null));
        ui.wait_text("#toasts", &format!("Suspended team {OWNER}."));
        ui.release_held();
        ui.wait_visible("#drawer");
        ui.wait_text("#drawer-title", "family");
        ui.wait_visible("#team-suspend-form");
    })
    .await;
    if !ran {
        return;
    }
    let team = teams::find(&server.db.pool, OWNER)
        .await
        .expect("read the team");
    assert!(team.suspension.is_some());
}

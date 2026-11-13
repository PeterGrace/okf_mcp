//! The Users page (M11 Task 8): create a user (and see its personal team),
//! open its detail and delete it; the deletes a held key, another team or
//! a catalog its personal team owns refuse, and that clearing the cause
//! enables Delete; and what an admin that does not reach every catalog may
//! do. M12 Task 15 adds the drawer's account sections: the email, the admin
//! flag (and the last-admin refusal), Send setup email, entitlements, the
//! user's console sessions and recent sign-ins.

use std::time::Duration;

use chrono::Utc;
use okf_mcp::config::Config;
use okf_mcp::store::catalogs;
use okf_mcp::store::ceiling;
use okf_mcp::store::keys::Access;
use okf_mcp::store::sign_in_events::{self, Method, Outcome};
use okf_mcp::store::teams::{self, NewTeam, Role};
use okf_mcp::store::user_sessions;
use okf_mcp::store::users::{self, NewUser};

use crate::catalogs::switcher;
use crate::harness::drive;
use crate::self_service::{console_base, login_server, member, EMAIL, PASSWORD};
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
        assert!(!ui.text("#user-detail").contains("null"));
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

/// How many mail jobs (sent or not: finished jobs stay for a week) are
/// addressed to `to`.
async fn mail_jobs_to(server: &TestServer, to: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM apalis.jobs \
         WHERE convert_from(job, 'UTF8')::jsonb -> 'mail' ->> 'to' = $1",
    )
    .bind(to)
    .fetch_one(&server.db.pool)
    .await
    .expect("count mail jobs")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_sets_an_email_and_the_admin_flag_and_sends_a_setup_email() {
    let server = login_server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    let user = NewUser {
        name: "alice",
        display_name: "",
    };
    users::create(&server.db.pool, &user, Utc::now())
        .await
        .expect("create alice");
    server.app.poll().await.expect("poll");
    let ran = drive(console_base(&server), move |ui| {
        ui.login(&admin);
        ui.open("/users");
        ui.click("[data-test=user-alice]");
        // No email yet: no setup email either, and the reason is given.
        ui.wait_visible("#user-invite[disabled]");
        ui.wait_text("#user-invite-reason", "Set an email first");
        ui.wait_text("#user-password", "not set");
        ui.fill("#user-email-form input[name=email]", EMAIL);
        ui.click("#user-email-form button[type=submit]");
        ui.wait_visible("#user-invite:not([disabled])");
        ui.wait_text("[data-test=user-alice-email]", EMAIL);
        // The list shows it too, read again after the change.
        ui.wait_text("#users-table", EMAIL);
        ui.click("#user-invite");
        ui.wait_text("#toasts", "Queued a setup email to alice@example.com");
        ui.click("#user-admin-toggle");
        ui.wait_text("#toasts", "alice is now an admin");
        ui.wait_visible("#user-admin-toggle:checked");
        ui.wait_text("[data-test=user-alice]", "admin");
        // A full admin key gets no "needs every catalog" note, and the
        // drawer carries no stray "null" (issue #302's pattern).
        assert!(!ui
            .page
            .is_visible("#user-account-readonly")
            .expect("visible"));
        assert!(!ui.text("#user-detail").contains("null"));
    })
    .await;
    if !ran {
        return;
    }
    let alice = users::find_by_email(&server.db.pool, EMAIL)
        .await
        .expect("find")
        .expect("alice has the email");
    assert!(alice.is_admin);
    assert_eq!(mail_jobs_to(&server, EMAIL).await, 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn clearing_the_last_admin_is_refused_with_the_reason_and_the_drawer_stays_usable() {
    let server = login_server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    server.user_with_password("alice", EMAIL, PASSWORD).await;
    users::set_admin(&server.db.pool, "alice", true, false)
        .await
        .expect("make alice the one admin user");
    server.app.poll().await.expect("poll");
    let ran = drive(console_base(&server), move |ui| {
        ui.login(&admin);
        ui.open("/users");
        ui.click("[data-test=user-alice]");
        ui.wait_visible("#user-admin-toggle:checked");
        ui.click("#user-admin-toggle");
        // The server's reason, in a toast and in the drawer, and the
        // switch springs back to the server's state.
        ui.wait_text("#toasts", "last admin user");
        ui.wait_text("#user-refusal", "last admin user");
        ui.wait_visible("#user-admin-toggle:checked:not([disabled])");
        // The drawer still works: the next action goes through and clears
        // the refusal.
        ui.fill("#user-email-form input[name=email]", "alice@example.org");
        ui.click("#user-email-form button[type=submit]");
        ui.wait_text("[data-test=user-alice-email]", "alice@example.org");
        ui.wait_hidden("#user-refusal");
    })
    .await;
    if !ran {
        return;
    }
    let alice = users::find_by_email(&server.db.pool, "alice@example.org")
        .await
        .expect("find")
        .expect("alice has the new email");
    assert!(alice.is_admin);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_entitles_catalogs_and_the_member_sees_them() {
    let server = login_server().await;
    // alice exists with a password, but reaches nothing yet.
    server.user_with_password("alice", EMAIL, PASSWORD).await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    let ran = drive(console_base(&server), move |ui| {
        ui.login(&admin);
        ui.open("/users");
        ui.click("[data-test=user-alice]");
        ui.wait_text("#entitlement-list", "No entitlements");
        ui.select("#entitlement-form select[name=catalog]", "main");
        ui.select("#entitlement-form select[name=access]", "read");
        ui.click("#entitlement-form button[type=submit]");
        ui.wait_visible("[data-test=entitlement-main]");
        ui.select("#entitlement-form select[name=catalog]", "other");
        ui.select("#entitlement-form select[name=access]", "read");
        ui.click("#entitlement-form button[type=submit]");
        ui.wait_visible("[data-test=entitlement-other]");
        // Change a level in place, then remove the other entitlement.
        ui.select("[data-test=entitlement-level-other]", "write");
        ui.wait_text("#toasts", "write on other");
        ui.click("[data-test=entitlement-remove-main]");
        ui.wait_hidden("[data-test=entitlement-main]");
        ui.click("#drawer-close");
        ui.click("#signout");
        ui.wait_visible("#login");
        // alice sees exactly what she was entitled to.
        ui.sign_in_password(EMAIL, PASSWORD);
        ui.wait_text("#whoami", "alice");
        assert_eq!(switcher(ui), vec!["other".to_string()]);
        // My keys (Task 14a) offers exactly the ceiling, at its levels.
        ui.open("/my-keys");
        ui.click("#my-key-new");
        ui.wait_visible("#my-key-form");
        assert_eq!(
            ui.eval(
                "() => [...document.querySelectorAll('#my-key-form select[name=level-other] option')] \
                 .map(option => option.value)",
                serde_json::json!(null)
            ),
            serde_json::json!(["read", "write"])
        );
    })
    .await;
    if !ran {
        return;
    }
    let entitlements = ceiling::entitlements(&server.db.pool, "alice")
        .await
        .expect("entitlements");
    assert_eq!(entitlements.len(), 1);
    assert_eq!(entitlements[0].catalog, "other");
    assert_eq!(entitlements[0].access, Access::Write);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_signs_a_user_out_and_sees_their_sign_ins() {
    let server = login_server().await;
    member(&server, Access::Write).await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    let pool = &server.db.pool;
    let alice = users::find_by_email(pool, EMAIL)
        .await
        .expect("find")
        .expect("alice");
    for agent in ["phone browser", "tablet browser", "laptop browser"] {
        user_sessions::create(
            pool,
            &alice.id,
            None,
            agent,
            Duration::from_secs(7 * 86_400),
            Utc::now(),
        )
        .await
        .expect("a session of alice's");
    }
    sign_in_events::record(
        pool,
        Some(&alice.id),
        Method::Password,
        Outcome::Failed,
        None,
        Utc::now(),
    )
    .await
    .expect("record a failed sign-in");
    let ran = drive(console_base(&server), move |ui| {
        ui.login(&admin);
        ui.open("/users");
        ui.click("[data-test=user-alice]");
        ui.wait_text("#user-session-table", "phone browser");
        ui.wait_text("#user-sign-in-table", "failed");
        // One session, by its own button.
        let phone = ui.eval(
            "() => [...document.querySelectorAll('#user-session-table tbody tr')] \
             .find(row => row.textContent.includes('phone browser')) \
             ?.querySelector('button')?.dataset.test ?? null",
            serde_json::json!(null),
        );
        let phone = phone.as_str().expect("a revoke button").to_string();
        ui.click(&format!("[data-test={phone}]"));
        ui.wait_text_gone("#user-session-table", "phone browser");
        // Then the rest: the confirmation counts what is left, read again
        // after the first sign-out.
        ui.wait_text("#user-session-table", "tablet browser");
        ui.click("#user-sessions-revoke-all");
        ui.wait_text("dialog.confirm", "Ends 2 console sessions");
        ui.click("#confirm-ok");
        ui.wait_text("#toasts", "out of 2 console sessions");
        ui.wait_text("#user-session-table", "No console sessions");
    })
    .await;
    if !ran {
        return;
    }
    let left = user_sessions::list(pool, &alice.id, None)
        .await
        .expect("list");
    assert!(left.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scoped_admin_sees_the_sign_in_controls_disabled() {
    let server = login_server().await;
    member(&server, Access::Write).await;
    let scoped = server
        .key("console-scoped", Access::Admin, &["main"], Some("main"))
        .await;
    drive(console_base(&server), move |ui| {
        ui.login(&scoped);
        ui.open("/users");
        ui.click("[data-test=user-alice]");
        ui.wait_visible("#user-admin-toggle[disabled]");
        ui.wait_visible("#user-invite[disabled]");
        ui.wait_visible("#user-email-form button[type=submit][disabled]");
        ui.wait_visible("[data-test=entitlement-level-main][disabled]");
        ui.wait_text("#user-account-readonly", "every catalog");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_session_sign_out_clicked_twice_sends_one_request() {
    let server = login_server().await;
    member(&server, Access::Write).await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    let pool = &server.db.pool;
    let alice = users::find_by_email(pool, EMAIL)
        .await
        .expect("find")
        .expect("alice");
    for agent in ["phone browser", "tablet browser"] {
        user_sessions::create(
            pool,
            &alice.id,
            None,
            agent,
            Duration::from_secs(7 * 86_400),
            Utc::now(),
        )
        .await
        .expect("a session of alice's");
    }
    let ran = drive(console_base(&server), move |ui| {
        ui.login(&admin);
        ui.open("/users");
        ui.click("[data-test=user-alice]");
        ui.wait_text("#user-session-table", "phone browser");
        let phone = ui.eval(
            "() => [...document.querySelectorAll('#user-session-table tbody tr')] \
             .find(row => row.textContent.includes('phone browser')) \
             ?.querySelector('button')?.dataset.test ?? null",
            serde_json::json!(null),
        );
        let phone = phone.as_str().expect("a revoke button").to_string();
        let prefix = phone
            .strip_prefix("user-session-revoke-")
            .expect("the button's data-test names the session");
        // Hold the DELETE, then click twice: the button is disabled while
        // the first request runs, so the second click sends nothing.
        ui.hold_next("DELETE", &format!("/sessions/{prefix}"));
        let disabled = ui.eval(
            "(selector) => { const button = document.querySelector(selector); \
             button.click(); button.click(); return button.disabled; }",
            serde_json::json!(format!("[data-test={phone}]")),
        );
        assert_eq!(ui.held_matches(), 1, "a second DELETE was sent");
        assert_eq!(disabled, serde_json::json!(true));
        ui.release_held();
        ui.wait_text_gone("#user-session-table", "phone browser");
        ui.wait_text("#user-session-table", "tablet browser");
    })
    .await;
    if !ran {
        return;
    }
    let left = user_sessions::list(pool, &alice.id, None)
        .await
        .expect("list");
    assert_eq!(left.len(), 1);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refusal_whose_reread_fails_keeps_its_reason_and_working_controls() {
    let server = login_server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    server.user_with_password("alice", EMAIL, PASSWORD).await;
    users::set_admin(&server.db.pool, "alice", true, false)
        .await
        .expect("make alice the one admin user");
    server.app.poll().await.expect("poll");
    let ran = drive(console_base(&server), move |ui| {
        ui.login(&admin);
        ui.open("/users");
        ui.click("[data-test=user-alice]");
        ui.wait_visible("#user-admin-toggle:checked");
        // The server refuses the change (the last admin), and reading the
        // user again then fails.
        ui.fail_next("GET", "/users/alice", 503, "the server is unavailable");
        ui.click("#user-admin-toggle");
        ui.wait_visible("#user-stale");
        ui.wait_text("#user-refusal", "last admin user");
        ui.wait_visible("#user-admin-toggle:checked:not([disabled])");
        ui.wait_visible("#user-email-form button[type=submit]:not([disabled])");
        // The drawer still works, and the next read clears both notes.
        ui.fill("#user-email-form input[name=email]", "alice@example.org");
        ui.click("#user-email-form button[type=submit]");
        ui.wait_text("[data-test=user-alice-email]", "alice@example.org");
        ui.wait_hidden("#user-stale");
        ui.wait_hidden("#user-refusal");
    })
    .await;
    if !ran {
        return;
    }
    let alice = users::find_by_email(&server.db.pool, "alice@example.org")
        .await
        .expect("find")
        .expect("alice has the new email");
    assert!(alice.is_admin);
}

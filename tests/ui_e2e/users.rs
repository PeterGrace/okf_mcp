//! The Users page (M11 Task 8): create a user (and see its personal team),
//! open its detail and delete it; and what an admin that does not reach
//! every catalog may do. M14 Task 4 (#379): a user's keys, revoked ones
//! included, go with it, a catalog its personal team owns disables Delete,
//! the last owner of a shared team is refused with the reason kept in the
//! drawer, and Delete is one pending write. M12 Task 15 adds the drawer's account sections: the email, the admin
//! flag (and the last-admin refusal), Send setup email, entitlements, the
//! user's console sessions and recent sign-ins.

use std::time::Duration;

use chrono::Utc;
use okf_mcp::config::Config;
use okf_mcp::store::catalogs;
use okf_mcp::store::ceiling;
use okf_mcp::store::keys::{self, Access, Grants, NewKey};
use okf_mcp::store::sign_in_events::{self, Method, Outcome};
use okf_mcp::store::teams::{self, NewTeam, Role};
use okf_mcp::store::user_sessions;
use okf_mcp::store::users::{self, NewUser};

use crate::catalogs::switcher;
use crate::harness::drive;
use crate::self_service::{console_base, login_server, member, EMAIL, PASSWORD};
use crate::support::server::{files, TestServer, OWNER};
use okf_mcp::store::audit::AuditActor;
use okf_mcp::store::users::AdminChange;

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

/// Gives user `user` one key on `main`, revoked when `revoked`.
async fn key_of(server: &TestServer, user: &str, name: &str, revoked: bool) {
    let catalogs = ["main".to_string()];
    let created = keys::create_with_id(
        &server.db.pool,
        &NewKey {
            name,
            access: Access::Read,
            maintenance_report: false,
            grants: Grants::Catalogs(&catalogs),
            default_catalog: None,
            actor: name,
            user,
            self_issued: false,
            expires_at: None,
            levels: &[],
        },
        &okf_mcp::store::audit::AuditActor::cli(),
        Utc::now(),
    )
    .await
    .expect("create a key");
    if revoked {
        keys::revoke_by_id(
            &server.db.pool,
            &created.id,
            &okf_mcp::store::audit::AuditActor::cli(),
            Utc::now(),
        )
        .await
        .expect("revoke the key");
    }
}

/// How many keys and grants name user `user`.
async fn keys_of(server: &TestServer, user: &str) -> i64 {
    sqlx::query_scalar(
        "SELECT count(*) FROM api_keys k JOIN users u ON u.id = k.user_id WHERE u.name = $1",
    )
    .bind(user)
    .fetch_one(&server.db.pool)
    .await
    .expect("count keys")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_user_whose_only_key_is_revoked_is_deleted_from_the_console() {
    // Issue #379: the revoked key used to keep Delete disabled.
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    let alice = NewUser {
        name: "alice",
        display_name: "",
    };
    users::create(&server.db.pool, &alice, &AuditActor::cli(), Utc::now())
        .await
        .expect("create alice");
    key_of(&server, "alice", "alice-laptop", true).await;
    server.app.poll().await.expect("poll");
    let ran = drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/users");
        ui.click("[data-test=user-alice]");
        ui.wait_text("#user-keys", "alice-laptop");
        ui.wait_visible("#user-delete:not([disabled])");
        assert!(!ui.page.is_visible("#user-delete-reason").expect("visible"));
        ui.click("#user-delete");
        ui.wait_text(
            "dialog.confirm",
            "keys, grants and sign-in codes (revoked ones too)",
        );
        ui.click("#confirm-ok");
        ui.wait_text("#toasts", "Deleted user alice.");
        ui.wait_hidden("#user-detail");
        ui.wait_hidden("[data-test=user-alice]");
    })
    .await;
    if !ran {
        return;
    }
    assert!(users::find(&server.db.pool, "alice").await.is_err());
    let left: i64 = sqlx::query_scalar("SELECT count(*) FROM api_keys WHERE name = 'alice-laptop'")
        .fetch_one(&server.db.pool)
        .await
        .expect("count");
    assert_eq!(left, 0, "the revoked key went with its user");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_delete_clicked_again_while_it_runs_sends_one_request() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    let alice = NewUser {
        name: "alice",
        display_name: "",
    };
    users::create(&server.db.pool, &alice, &AuditActor::cli(), Utc::now())
        .await
        .expect("create alice");
    key_of(&server, "alice", "alice-laptop", false).await;
    server.app.poll().await.expect("poll");
    assert_eq!(keys_of(&server, "alice").await, 1);
    let ran = drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/users");
        ui.click("[data-test=user-alice]");
        ui.wait_visible("#user-delete:not([disabled])");
        // Hold the DELETE: while it runs, Delete is disabled, in this
        // drawer and in the one drawn when alice is opened again.
        ui.hold_next("DELETE", "/users/alice");
        ui.click("#user-delete");
        ui.click("#confirm-ok");
        ui.wait_visible("#user-delete[disabled]");
        ui.mark("#user-delete");
        ui.click("#drawer-close");
        ui.click("[data-test=user-alice]");
        ui.wait_redrawn("#user-delete");
        ui.wait_visible("#user-delete[disabled]");
        assert!(
            ui.click_twice("#user-delete"),
            "the redrawn Delete is enabled"
        );
        assert_eq!(ui.held_matches(), 1, "a second DELETE was sent");
        // Once it settles, the drawer that shows alice now closes.
        ui.release_held();
        ui.wait_hidden("#user-detail");
        ui.wait_hidden("[data-test=user-alice]");
        assert_eq!(ui.held_matches(), 1);
    })
    .await;
    if !ran {
        return;
    }
    assert!(users::find(&server.db.pool, "alice").await.is_err());
    assert_eq!(keys_of(&server, "alice").await, 0);
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
async fn a_last_owner_or_an_owned_catalog_refuses_the_delete_until_cleared() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    let pool = &server.db.pool;
    // alice is the only owner of a shared team, and bob's personal team owns
    // a catalog; carol is a plain member of that team, which never stops a
    // deletion.
    for name in ["alice", "bob", "carol"] {
        let user = NewUser {
            name,
            display_name: "",
        };
        users::create(pool, &user, &AuditActor::cli(), Utc::now())
            .await
            .expect("create a user");
    }
    let family = NewTeam {
        name: "family",
        display_name: "",
        owner: "alice",
    };
    teams::create(
        pool,
        &family,
        &okf_mcp::store::audit::AuditActor::cli(),
        Utc::now(),
    )
    .await
    .expect("create a team");
    teams::set_member(
        pool,
        &okf_mcp::store::teams::Membership {
            team: "family",
            user: "carol",
            role: Role::Member,
        },
        &okf_mcp::store::audit::AuditActor::cli(),
        Utc::now(),
    )
    .await
    .expect("add carol");
    catalogs::create(
        pool,
        &okf_mcp::store::catalogs::NewCatalog {
            name: "bobs",
            description: "",
            owner_team: "bob",
        },
        &okf_mcp::store::audit::AuditActor::cli(),
        Utc::now(),
    )
    .await
    .expect("create bob's catalog");
    server.app.poll().await.expect("poll");
    let ran = drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        // alice: the server refuses, and reading her again then fails; the
        // drawer keeps the reason, marked stale, with Delete usable again.
        ui.open("/users");
        ui.click("[data-test=user-alice]");
        ui.wait_visible("#user-delete:not([disabled])");
        ui.fail_next("GET", "/users/alice", 503, "the server is unavailable");
        ui.click("#user-delete");
        ui.click("#confirm-ok");
        ui.wait_text("#toasts", "last owner of team(s)");
        ui.wait_visible("#user-stale");
        ui.wait_visible("#user-refusal");
        ui.wait_text("#user-refusal", "last owner of team(s) \"family\"");
        ui.wait_visible("#user-delete:not([disabled])");
        ui.wait_visible("[data-test=user-alice]");
        ui.click("#drawer-close");
        // carol: a member of family, deleted with her membership.
        ui.click("[data-test=user-carol]");
        ui.wait_visible("#user-delete:not([disabled])");
        ui.click("#user-delete");
        ui.click("#confirm-ok");
        ui.wait_hidden("[data-test=user-carol]");
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
    if !ran {
        return;
    }
    assert!(
        users::find(pool, "alice").await.is_ok(),
        "the refusal kept alice"
    );
    assert!(users::find(pool, "carol").await.is_err());
    let family = teams::find(pool, "family").await.expect("family");
    assert_eq!(family.members.len(), 1, "only alice is left in family");
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
    users::create(&server.db.pool, &user, &AuditActor::cli(), Utc::now())
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
    users::set_admin(
        &server.db.pool,
        "alice",
        AdminChange {
            on: true,
            allow_last: false,
        },
        &AuditActor::cli(),
        Utc::now(),
    )
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
        let selector = format!("[data-test={phone}]");
        let disabled = ui.click_twice(&selector);
        assert_eq!(ui.held_matches(), 1, "a second DELETE was sent");
        assert!(disabled);
        // Sorting by Last seen draws the table again, with a new button for
        // that session: it is disabled too, and clicking it sends nothing.
        ui.mark(&selector);
        ui.click("#user-session-table th:nth-child(4)");
        assert!(!ui.is_marked(&selector), "the sort did not redraw the row");
        let redrawn = ui.click_twice(&selector);
        assert_eq!(
            ui.held_matches(),
            1,
            "the redrawn button sent a second DELETE"
        );
        assert!(redrawn);
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
    users::set_admin(
        &server.db.pool,
        "alice",
        AdminChange {
            on: true,
            allow_last: false,
        },
        &AuditActor::cli(),
        Utc::now(),
    )
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_write_that_settles_after_its_user_was_reopened_updates_the_new_drawer() {
    let server = login_server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    server.user_with_password("alice", EMAIL, PASSWORD).await;
    let bob = NewUser {
        name: "bob",
        display_name: "",
    };
    users::create(&server.db.pool, &bob, &AuditActor::cli(), Utc::now())
        .await
        .expect("create bob");
    server.app.poll().await.expect("poll");
    let ran = drive(console_base(&server), move |ui| {
        ui.login(&admin);
        ui.open("/users");
        ui.click("[data-test=user-alice]");
        ui.wait_visible("#user-admin-toggle:not(:checked):not([disabled])");
        // Make alice an admin, but hold the write while her drawer is
        // closed and opened again: the new drawer reads her from before
        // the write, with the switch disabled while it runs.
        ui.hold_next("PUT", "/users/alice/admin");
        ui.click("#user-admin-toggle");
        ui.click("#drawer-close");
        ui.click("[data-test=user-alice]");
        ui.wait_visible("#user-admin-toggle:not(:checked)[disabled]");
        // Once the write settles, the new drawer shows the server's state.
        ui.release_held();
        ui.wait_visible("#user-admin-toggle:checked:not([disabled])");
        // A refusal reaches the reopened drawer too: clearing alice, the
        // last admin user, is refused.
        ui.hold_next("PUT", "/users/alice/admin");
        ui.click("#user-admin-toggle");
        ui.click("#drawer-close");
        ui.click("[data-test=user-alice]");
        ui.wait_visible("#user-admin-toggle[disabled]");
        ui.release_held();
        ui.wait_text("#user-refusal", "last admin user");
        ui.wait_visible("#user-admin-toggle:checked:not([disabled])");
        // Another user's drawer is left alone when alice's write settles.
        ui.hold_next("PUT", "/users/alice/email");
        ui.fill("#user-email-form input[name=email]", "alice@example.org");
        ui.click("#user-email-form button[type=submit]");
        ui.click("#drawer-close");
        ui.click("[data-test=user-bob]");
        ui.wait_text("#drawer-title", "bob");
        ui.release_held();
        assert_eq!(ui.text("#drawer-title"), "bob");
        ui.wait_text("[data-test=user-bob-email]", "none");
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
async fn the_users_filter_matches_names_and_emails() {
    let server = server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    let pool = server.app.pool();
    for (name, display_name) in [("alice", ""), ("bob", "Robert Example")] {
        users::create(
            pool,
            &NewUser { name, display_name },
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
    }
    users::set_email(
        pool,
        "bob",
        Some("robert@example.com"),
        &AuditActor::cli(),
        Utc::now(),
    )
    .await
    .unwrap();
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/users");
        ui.wait_visible("[data-test=user-alice]");
        // An email, in another letter case.
        ui.fill("#users-filter", "ROBERT@");
        ui.wait_hidden("[data-test=user-alice]");
        ui.wait_visible("[data-test=user-bob]");
        // A display name.
        ui.fill("#users-filter", "robert ex");
        ui.wait_visible("[data-test=user-bob]");
        ui.fill("#users-filter", "nobody");
        ui.wait_hidden("[data-test=user-bob]");
        ui.wait_text("#users-table", "No user matches the filter.");
        ui.fill("#users-filter", "");
        ui.wait_visible("[data-test=user-alice]");
        ui.wait_visible("[data-test=user-bob]");

        // A list that answers after the filter changed is drawn through the
        // newest filter: create carol under the filter "bob", hold the list
        // read that follows, filter by "car", then let the read answer.
        ui.fill("#users-filter", "bob");
        ui.wait_hidden("[data-test=user-alice]");
        ui.click("#user-new");
        ui.fill("#user-form input[name=name]", "carol");
        ui.hold_answer("GET", "/api/v1/users", || {
            ui.click("#user-form button[type=submit]")
        });
        ui.fill("#users-filter", "car");
        ui.release_held();
        ui.wait_visible("[data-test=user-carol]");
        ui.wait_hidden("[data-test=user-bob]");
        ui.wait_hidden("[data-test=user-alice]");
    })
    .await;
}

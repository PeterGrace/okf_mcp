//! The API keys page: create a key and see its secret once, sign in with
//! it, revoke the signed-in key, and what a read key sees; mint, list and
//! revoke a sign-in code, and how an OAuth grant shows (M10 Task 7); change
//! a key's or code's user, and assign one that has none (M11 Task 8).

use chrono::{TimeDelta, Utc};
use okf_mcp::config::Config;
use okf_mcp::store::keys::{self as key_rows, Access, Grants};
use okf_mcp::store::oauth::clients;
use okf_mcp::store::oauth::codes::{self, NewSignInCode};
use okf_mcp::store::users::{self, NewUser};

use crate::harness::drive;
use crate::support::server::{files, TestServer, OWNER};

/// Creates the user `alice`, a second one to reassign credentials to.
async fn alice(server: &TestServer) {
    let alice = NewUser {
        name: "alice",
        display_name: "Alice Example",
    };
    users::create(&server.db.pool, &alice, Utc::now())
        .await
        .expect("create alice");
}

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
        ui.select("#key-form select[name=user]", OWNER);
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

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_mints_a_sign_in_code_sees_it_once_and_revokes_it() {
    let server = server().await;
    let admin = server
        .key("console-admin", Access::Admin, &["main"], Some("main"))
        .await;
    let ran = drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/keys");
        ui.wait_visible("#code-new");
        ui.click("#code-new");
        ui.fill("#code-form input[name=name]", "console-code");
        ui.fill("#code-form input[name=actor]", "example-actor");
        ui.select("#code-form select[name=user]", OWNER);
        // No catalog: the server refuses (409), which the page toasts. No
        // code was minted, so no dialog opens, and the form stays filled.
        ui.click("#code-form button[type=submit]");
        ui.wait_text("#toasts", "must be granted a catalog");
        assert!(!ui.page.is_visible("#code-secret-dialog").expect("visible"));
        let kept = ui.eval(
            "() => ['input[name=name]', 'input[name=actor]', 'select[name=user]'].map(field => \
                 document.querySelector(`#code-form ${field}`).value)",
            serde_json::json!(null),
        );
        assert_eq!(
            kept,
            serde_json::json!(["console-code", "example-actor", OWNER])
        );
        // The retry, with a catalog, mints the code.
        ui.click("#code-form input[name=catalog][value=main]");
        ui.click("#code-form button[type=submit]");
        ui.wait_visible("#code-secret");
        let secret = ui.text("#code-secret");
        assert!(
            secret.starts_with("okfsc_"),
            "a sign-in code, not {secret:?}"
        );
        // Escape does not close the dialog: only "I have stored it" does.
        ui.page.press_key(None, "Escape").expect("press Escape");
        let open = ui.eval(
            "() => document.getElementById('code-secret-dialog').open",
            serde_json::json!(null),
        );
        assert_eq!(open, serde_json::json!(true), "Escape closed the dialog");
        assert!(ui.page.is_visible("#code-secret").expect("visible"));
        ui.click("#code-secret-done");
        ui.wait_hidden("#code-secret");
        ui.wait_text("[data-test=code-state-console-code]", "live");
        // Once dismissed, the code is nowhere on the page, nor in storage.
        let page = ui.eval(
            "() => document.body.textContent + JSON.stringify(localStorage) \
             + JSON.stringify(sessionStorage)",
            serde_json::json!(null),
        );
        assert!(!page.as_str().unwrap_or_default().contains(&secret));
        ui.click("[data-test=code-console-code]");
        ui.wait_visible("#code-revoke");
        ui.click("#code-revoke");
        ui.click("#confirm-ok");
        // A revoked code leaves the default (live-only) list...
        ui.wait_hidden("[data-test=code-console-code]");
        // ...and shows again, revoked, with every state shown.
        ui.click("#codes-show-all");
        ui.wait_text("[data-test=code-state-console-code]", "revoked");
    })
    .await;
    if !ran {
        return;
    }
    let minted = codes::list(&server.db.pool).await.expect("list codes");
    assert_eq!(minted.len(), 1);
    assert_eq!(minted[0].name, "console-code");
    assert_eq!(minted[0].actor, "example-actor");
    assert!(minted[0].revoked_at.is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_oauth_grant_is_marked_and_cannot_be_given_reporting() {
    let server = server().await;
    let admin = server
        .key("console-admin", Access::Admin, &["main"], Some("main"))
        .await;
    // A client and a grant made the way the consent page makes them (Task 3
    // registers clients; a raw row keeps this journey independent of it). A
    // CIMD row needs `fetched_at` and `cache_expires_at`.
    let client_id = "https://client.example.com/client.json";
    sqlx::query(
        "INSERT INTO oauth_clients \
         (client_id, kind, client_name, redirect_uris, metadata, fetched_at, cache_expires_at, \
          created_at) \
         VALUES ($1, 'cimd', 'Example Client', ARRAY['https://client.example.com/callback'], \
                 '{}'::jsonb, now(), now() + interval '1 hour', now())",
    )
    .bind(client_id)
    .execute(&server.db.pool)
    .await
    .expect("insert a client");
    let client = clients::find(&server.db.pool, client_id)
        .await
        .expect("read the client")
        .expect("the client exists");
    let main = ["main".to_string()];
    let code = codes::create(
        &server.db.pool,
        &NewSignInCode {
            name: "laptop",
            access: Access::Write,
            grants: Grants::Catalogs(&main),
            default_catalog: Some("main"),
            actor: "example-actor",
            expires_at: Utc::now() + TimeDelta::hours(1),
            user: OWNER,
        },
        Utc::now(),
    )
    .await
    .expect("mint a code");
    let grant_id = codes::redeem(&server.db.pool, &code.id, &client, Utc::now())
        .await
        .expect("redeem the code");
    server.app.poll().await.expect("poll the grant in");
    let name = key_rows::list(&server.db.pool)
        .await
        .expect("list keys")
        .into_iter()
        .find(|key| key.id == grant_id)
        .expect("the grant")
        .name;
    // Quoted: a grant's name has dots (Task 4, Decision 1).
    let row = format!("[data-test=\"key-{name}\"]");
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/keys");
        let text = ui.wait_text(&row, "OAuth");
        assert!(
            text.contains("Example Client"),
            "the client is named: {text:?}"
        );
        ui.click(&row);
        ui.wait_text("#drawer", "OAuth grant");
        ui.wait_visible("#key-revoke");
        assert!(!ui.page.is_visible("#key-report-toggle").expect("visible"));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_changes_a_keys_user_and_assigns_a_legacy_key() {
    let server = server().await;
    // Every catalog: only such a key manages the legacy key, which has them.
    let admin = server
        .key("console-admin", Access::Admin, &[], Some("main"))
        .await;
    server.key("laptop", Access::Read, &["main"], None).await;
    server.legacy_key("legacy", Access::Read).await;
    alice(&server).await;
    let ran = drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        // The shell names the signed-in key's user.
        ui.wait_text("#whoami", &format!("{OWNER} / console-admin"));
        ui.open("/keys");
        ui.wait_text("[data-test=key-user-laptop]", OWNER);
        ui.click("[data-test=key-laptop]");
        ui.wait_text("#key-assign", "Change user");
        ui.select("#key-user-select", "alice");
        ui.click("#key-assign");
        ui.wait_text("[data-test=key-user-laptop]", "alice");
        ui.click("#drawer-close");
        // A key from before users existed is marked, and its drawer offers
        // Assign instead.
        ui.wait_text("[data-test=key-user-legacy]", "unassigned");
        ui.click("[data-test=key-legacy]");
        ui.wait_text("#drawer-title", "legacy");
        ui.wait_text("#key-assign", "Assign");
        ui.select("#key-user-select", OWNER);
        ui.click("#key-assign");
        ui.wait_text("[data-test=key-user-legacy]", OWNER);
    })
    .await;
    if !ran {
        return;
    }
    let rows = key_rows::list(&server.db.pool).await.expect("list keys");
    let user_of = |name: &str| {
        rows.iter()
            .find(|key| key.name == name)
            .and_then(|key| key.user.clone())
    };
    assert_eq!(user_of("laptop").as_deref(), Some("alice"));
    assert_eq!(user_of("legacy").as_deref(), Some(OWNER));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_changes_a_sign_in_codes_user() {
    let server = server().await;
    let admin = server
        .key("console-admin", Access::Admin, &["main"], Some("main"))
        .await;
    alice(&server).await;
    let main = ["main".to_string()];
    codes::create(
        &server.db.pool,
        &NewSignInCode {
            name: "tablet",
            access: Access::Read,
            grants: Grants::Catalogs(&main),
            default_catalog: None,
            actor: "",
            expires_at: Utc::now() + TimeDelta::hours(1),
            user: OWNER,
        },
        Utc::now(),
    )
    .await
    .expect("mint a code");
    let ran = drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.open("/keys");
        ui.wait_text("[data-test=code-user-tablet]", OWNER);
        ui.click("[data-test=code-tablet]");
        ui.wait_text("#code-assign", "Change user");
        ui.select("#code-user-select", "alice");
        ui.click("#code-assign");
        ui.wait_text("[data-test=code-user-tablet]", "alice");
    })
    .await;
    if !ran {
        return;
    }
    let minted = codes::list(&server.db.pool).await.expect("list codes");
    assert_eq!(minted[0].user.as_deref(), Some("alice"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn reassigning_the_signed_in_key_updates_the_shell_identity() {
    let server = server().await;
    let admin = server
        .key("console-admin", Access::Admin, &["main"], Some("main"))
        .await;
    alice(&server).await;
    drive(server.base.clone(), move |ui| {
        ui.login(&admin);
        ui.wait_text("#whoami", &format!("{OWNER} / console-admin"));
        ui.open("/keys");
        ui.click("[data-test=key-console-admin]");
        ui.wait_text("#key-assign", "Change user");
        ui.select("#key-user-select", "alice");
        ui.click("#key-assign");
        ui.wait_text("[data-test=key-user-console-admin]", "alice");
        // The shell follows without a reload: the identity line, and the
        // Users page's "you" marker.
        ui.wait_text("#whoami", "alice / console-admin");
        ui.click("#drawer-close");
        ui.open("/users");
        ui.wait_text("[data-test=user-alice]", "you");
        let tester = ui.text(&format!("[data-test=user-{OWNER}]"));
        assert!(!tester.contains("you"), "still marked: {tester:?}");
    })
    .await;
}

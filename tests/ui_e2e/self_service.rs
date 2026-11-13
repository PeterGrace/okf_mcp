//! The console signed in with an account (M12 Task 14a): password sign-in
//! from the login card's link, the "Your account" group and no Administer
//! group for a member (whose admin routes the API refuses too), the full
//! console for an admin user, sign-out ending the server's session, and My
//! keys: a key created within the ceiling (its secret shown once), a key
//! the ceiling no longer allows refused with the reason, the key limit,
//! rename and revoke. Task 14b adds My grants.
//!
//! The fixtures here (`login_server`, `console_base`, `member`, `PASSWORD`,
//! `EMAIL`) are shared with `account.rs` (Task 14b) and with `users.rs`,
//! `catalogs.rs` and `settings.rs` (Task 15).

use std::sync::{Arc, Mutex};

use chrono::Utc;
use okf_mcp::config::Config;
use okf_mcp::store::ceiling;
use okf_mcp::store::keys::{self as key_rows, Access, CatalogGrant, Grants, NewKey};
use okf_mcp::store::settings::{self, SettingKey};
use okf_mcp::store::users;
use serde_json::json;

use crate::catalogs::switcher;
use crate::harness::drive;
use crate::support::server::{files, TestServer};

/// alice's password in every journey: long enough, and not a common one.
pub(crate) const PASSWORD: &str = "correct horse battery staple";

/// alice's email in every journey.
pub(crate) const EMAIL: &str = "alice@example.com";

/// The statuses the API gives the page's own cookie session for admin
/// routes: listing keys, users and teams, and creating a user (with the
/// session's CSRF token, so only the access check can refuse it).
const ADMIN_ROUTE_STATUSES: &str = "async () => { \
    const { csrf } = await (await fetch('/api/v1/session')).json(); \
    const status = async (path, init) => (await fetch(path, init)).status; \
    return [await status('/api/v1/keys'), await status('/api/v1/users'), \
        await status('/api/v1/teams'), \
        await status('/api/v1/users', { method: 'POST', \
            headers: { 'X-OKF-CSRF': csrf, 'Content-Type': 'application/json' }, \
            body: JSON.stringify({ name: 'mallory' }) })]; }";

/// The login card's "Sign in with your account" link.
const SIGN_IN_HREF: &str = "() => document.querySelector('#login-signin').getAttribute('href')";

/// The status of `GET /api/v1/session` with the page's cookie.
const SESSION_STATUS: &str = "async () => (await fetch('/api/v1/session')).status";

/// A server with login on, and two catalogs, `main` and `other`, both
/// owned by the fixture user's personal team (so alice reaches neither
/// through a team).
///
/// `public_url` names `localhost`, not `127.0.0.1`: a WebAuthn relying
/// party id must be a host name, and the session cookie belongs to the
/// host the browser used, so every journey that signs in opens the console
/// through [`console_base`].
///
/// # Returns
/// The running server.
pub(crate) async fn login_server() -> TestServer {
    TestServer::start_with(
        vec![
            ("main", files(&[("index.md", "# Main\n")])),
            ("other", files(&[("index.md", "# Other\n")])),
        ],
        |base| Config {
            public_url: Some(base.replace("127.0.0.1", "localhost")),
            ..Config::default()
        },
    )
    .await
}

/// The console's origin for journeys on a [`login_server`]: the server's
/// base with `localhost` for `127.0.0.1`. Chromium resolves `localhost`
/// to the loopback address itself.
///
/// # Returns
/// `http://localhost:<port>`.
pub(crate) fn console_base(server: &TestServer) -> String {
    server.base.replace("127.0.0.1", "localhost")
}

/// Makes alice, with [`EMAIL`] and [`PASSWORD`], entitled to `main` at
/// `access` (her whole ceiling), and reloads the key cache.
///
/// # Arguments
/// * `server` - a [`login_server`].
/// * `access` - her level on `main`: `Read` or `Write`.
///
/// # Panics
/// If a store call or the poll fails.
pub(crate) async fn member(server: &TestServer, access: Access) {
    server.user_with_password("alice", EMAIL, PASSWORD).await;
    ceiling::entitle(&server.db.pool, "alice", "main", access, Utc::now())
        .await
        .expect("entitle alice to main");
    server.app.poll().await.expect("poll");
}

/// Creates a self-issued key of alice's called `name`, with write on
/// `main`, as My keys would, and reloads the key cache.
async fn alices_key(server: &TestServer, name: &str) {
    let catalogs = ["main".to_string()];
    let levels = [CatalogGrant {
        catalog: "main".to_string(),
        access: Some(Access::Write),
    }];
    let new = NewKey {
        name,
        access: Access::Write,
        maintenance_report: false,
        grants: Grants::Catalogs(&catalogs),
        default_catalog: Some("main"),
        actor: name,
        user: "alice",
        self_issued: true,
        expires_at: None,
        levels: &levels,
    };
    key_rows::create(&server.db.pool, &new, Utc::now())
        .await
        .expect("create alice's key");
    server.app.poll().await.expect("poll");
}

/// The key called `name`, if the store has one.
async fn key_named(server: &TestServer, name: &str) -> Option<key_rows::KeyRecord> {
    key_rows::list(&server.db.pool)
        .await
        .expect("list keys")
        .into_iter()
        .find(|record| record.name == name)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_signs_in_from_the_card_and_sees_only_their_pages() {
    let server = login_server().await;
    member(&server, Access::Write).await;
    drive(console_base(&server), |ui| {
        // The card's button comes back to the very page asked for.
        ui.open("/my-keys");
        ui.wait_visible("#login");
        assert_eq!(
            ui.eval(SIGN_IN_HREF, json!(null)),
            json!("/signin?return_to=%2Fui%2F%23%2Fmy-keys")
        );
        ui.click("#login-signin");
        ui.fill("#signin-email", EMAIL);
        ui.fill("#signin-password", PASSWORD);
        ui.click("#signin-submit");
        ui.wait_text("#my-keys-note", "0 of 20");
        ui.wait_text("#whoami", "alice - write");
        ui.wait_visible("a[data-nav=my-keys]");
        // (My grants and Account join the group in Task 14b: until their
        // pages exist, the manifest hides their links.)
        assert!(!ui.page.is_visible("[data-need=admin]").expect("visible"));
        // Hidden is not the only guard: the API refuses her admin routes.
        assert_eq!(
            ui.eval(ADMIN_ROUTE_STATUSES, json!(null)),
            json!([403, 403, 403, 403])
        );
        // The switcher offers the ceiling's catalogs only.
        assert_eq!(switcher(ui), vec!["main".to_string()]);
        ui.click("#signout");
        ui.wait_visible("#login");
        assert_eq!(ui.eval(SESSION_STATUS, json!(null)), json!(401));
        // Sign-out lands on the console's root, and the button follows.
        assert_eq!(
            ui.eval(SIGN_IN_HREF, json!(null)),
            json!("/signin?return_to=%2Fui%2F")
        );
        // The server ended the session: a reload stays on the card.
        ui.open("");
        ui.wait_visible("#login");
        assert!(!ui.page.is_visible("#app").expect("visible"));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_members_level_follows_the_selected_catalog() {
    let server = login_server().await;
    member(&server, Access::Write).await;
    ceiling::entitle(&server.db.pool, "alice", "other", Access::Read, Utc::now())
        .await
        .expect("entitle alice to other");
    server.app.poll().await.expect("poll");
    drive(console_base(&server), |ui| {
        ui.sign_in_password(EMAIL, PASSWORD);
        // The console's own store module (the same instance the pages use).
        let can = "() => import('/ui/js/core/store.js') \
            .then(({ store }) => [store.can('read'), store.can('write'), store.can('admin')])";
        ui.select("#catalog-select", "main");
        assert_eq!(ui.eval(can, json!(null)), json!([true, true, false]));
        ui.select("#catalog-select", "other");
        assert_eq!(ui.eval(can, json!(null)), json!([true, false, false]));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_user_signs_in_and_sees_the_full_console() {
    let server = login_server().await;
    member(&server, Access::Read).await;
    users::set_admin(&server.db.pool, "alice", true, false)
        .await
        .expect("make alice an admin");
    server.app.poll().await.expect("poll");
    drive(console_base(&server), |ui| {
        ui.sign_in_password(EMAIL, PASSWORD);
        ui.wait_text("#whoami", "admin");
        ui.wait_visible("[data-need=admin]");
        ui.wait_visible("a[data-nav=my-keys]");
        // An admin session reaches every catalog.
        assert_eq!(switcher(ui), vec!["main".to_string(), "other".to_string()]);
        ui.open("/users");
        ui.wait_text("[data-test=user-alice]", "you");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_creates_a_key_within_the_ceiling_and_sees_its_secret_once() {
    let server = login_server().await;
    member(&server, Access::Write).await;
    // The secret the dialog showed, handed out of the journey's thread.
    let shown = Arc::new(Mutex::new(String::new()));
    let seen = Arc::clone(&shown);
    let ran = drive(console_base(&server), move |ui| {
        ui.sign_in_password(EMAIL, PASSWORD);
        ui.open("/my-keys");
        ui.wait_text("#my-keys-note", "0 of 20");
        ui.click("#my-key-new");
        ui.wait_visible("#my-key-form");
        // Only the ceiling's catalogs are offered.
        assert_eq!(
            ui.eval(
                "() => [...document.querySelectorAll('#my-key-form input[name=catalog]')] \
                 .map(input => input.value)",
                json!(null)
            ),
            json!(["main"])
        );
        ui.fill("#my-key-form input[name=name]", "laptop");
        ui.click("#my-key-form input[name=catalog][value=main]");
        ui.select("#my-key-form select[name=level-main]", "write");
        ui.select("#my-key-form select[name=expires_in]", "30d");
        ui.click("#my-key-form button[type=submit]");
        ui.wait_visible("#my-key-secret");
        let secret = ui.text("#my-key-secret");
        assert!(secret.starts_with("okf_"), "{secret:?}");
        ui.click("#my-key-secret-done");
        ui.wait_hidden("#my-key-secret-dialog");
        // Once closed, the secret is nowhere in the page or its storage.
        assert_eq!(
            ui.eval(
                "(secret) => [document.documentElement.outerHTML, \
                 ...Object.values(localStorage), ...Object.values(sessionStorage)] \
                 .some(text => text.includes(secret))",
                json!(secret)
            ),
            json!(false)
        );
        ui.wait_text("[data-test=my-key-laptop]", "laptop");
        ui.wait_text("#my-keys-note", "1 of 20");
        *seen.lock().expect("the secret's lock") = secret;
    })
    .await;
    if !ran {
        return;
    }
    let key = key_named(&server, "laptop").await.expect("the key exists");
    assert!(key.self_issued);
    assert_eq!(key.user.as_deref(), Some("alice"));
    assert!(key.expires_at.is_some());
    assert_eq!(key.catalog_access.get("main").copied(), Some(Access::Write));
    // The secret the page showed is the key's own.
    let secret = shown.lock().expect("the secret's lock").clone();
    let principal = server
        .app
        .keys()
        .find(&secret)
        .expect("the secret signs in");
    assert_eq!(principal.key_id, key.id);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_lifetime_limit_shapes_the_expiry_choices_and_the_longest_is_the_default() {
    let server = login_server().await;
    member(&server, Access::Write).await;
    settings::set(
        &server.db.pool,
        SettingKey::MaxKeyLifetime,
        "30d",
        "tester",
        Utc::now(),
    )
    .await
    .expect("limit key lifetimes to 30 days");
    server.app.poll().await.expect("poll");
    // The expiry the API reports for the new key, handed out of the
    // journey's thread.
    let reported = Arc::new(Mutex::new(String::new()));
    let seen = Arc::clone(&reported);
    let ran = drive(console_base(&server), move |ui| {
        ui.sign_in_password(EMAIL, PASSWORD);
        ui.open("/my-keys");
        ui.click("#my-key-new");
        ui.wait_visible("#my-key-form");
        // 7 days fits under the limit; 30 days is the limit itself (offered
        // once, as "the longest allowed", and chosen); 90 days, a year and
        // Never are past it.
        assert_eq!(
            ui.eval(
                "() => { const select = document.querySelector('#my-key-form select[name=expires_in]'); \
                 return { values: [...select.options].map(option => option.value), \
                 chosen: select.value, label: select.selectedOptions[0].textContent }; }",
                json!(null)
            ),
            json!({
                "values": ["7d", "longest"],
                "chosen": "longest",
                "label": "30 days (the longest allowed)",
            })
        );
        ui.fill("#my-key-form input[name=name]", "bounded");
        ui.click("#my-key-form input[name=catalog][value=main]");
        ui.click("#my-key-form button[type=submit]");
        ui.wait_visible("#my-key-secret");
        ui.click("#my-key-secret-done");
        ui.wait_text("[data-test=my-key-bounded]", "bounded");
        let expires = ui.eval(
            "async () => (await (await fetch('/api/v1/me/keys')).json()) \
             .keys.find(key => key.name === 'bounded').expires_at",
            json!(null),
        );
        *seen.lock().expect("the expiry's lock") =
            expires.as_str().expect("an expiry").to_string();
    })
    .await;
    if !ran {
        return;
    }
    // The page sent no expiry, so the server set its limit: about now + 30d.
    let expires = chrono::DateTime::parse_from_rfc3339(&reported.lock().expect("lock"))
        .expect("an RFC 3339 expiry")
        .with_timezone(&Utc);
    let expected = Utc::now() + chrono::TimeDelta::days(30);
    assert!(
        (expected - expires).num_seconds().abs() < 300,
        "expires {expires}, expected about {expected}"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_issued_key_for_every_catalog_shows_its_level() {
    let server = login_server().await;
    member(&server, Access::Write).await;
    let new = NewKey {
        name: "ops",
        access: Access::Admin,
        maintenance_report: false,
        grants: Grants::All,
        default_catalog: None,
        actor: "ops",
        user: "alice",
        self_issued: false,
        expires_at: None,
        levels: &[],
    };
    key_rows::create(&server.db.pool, &new, Utc::now())
        .await
        .expect("an admin issues alice a key for every catalog");
    server.app.poll().await.expect("poll");
    drive(console_base(&server), |ui| {
        ui.sign_in_password(EMAIL, PASSWORD);
        ui.open("/my-keys");
        ui.wait_text(
            "#my-keys-table tr:has([data-test=my-key-ops]) td:nth-child(2)",
            "All catalogs admin",
        );
        ui.click("[data-test=my-key-ops]");
        ui.wait_text("#drawer dl.facts", "All catalogs admin");
        ui.wait_text("#drawer dl.facts", "an admin (not capped by your ceiling)");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_key_the_ceiling_no_longer_allows_is_refused_and_names_the_limit() {
    let server = login_server().await;
    member(&server, Access::Write).await;
    let pool = server.db.pool.clone();
    let app = Arc::clone(&server.app);
    // The journey runs on a blocking thread; this handle lets it run the
    // admin's change on the test's runtime while the form is open.
    let runtime = tokio::runtime::Handle::current();
    let ran = drive(console_base(&server), move |ui| {
        ui.sign_in_password(EMAIL, PASSWORD);
        ui.open("/my-keys");
        ui.click("#my-key-new");
        ui.fill("#my-key-form input[name=name]", "too-much");
        ui.click("#my-key-form input[name=catalog][value=main]");
        ui.select("#my-key-form select[name=level-main]", "write");
        runtime.block_on(async {
            ceiling::unentitle(&pool, "alice", "main")
                .await
                .expect("take main away");
            app.poll().await.expect("poll");
        });
        ui.click("#my-key-form button[type=submit]");
        // The 422 (`limit: "ceiling"`) says why.
        ui.wait_text("#toasts", "not among the catalogs you may use");
        // The form stays filled and no secret was shown.
        assert!(ui.page.is_visible("#my-key-form").expect("visible"));
        assert!(!ui.page.is_visible("#my-key-secret").expect("visible"));
    })
    .await;
    if !ran {
        return;
    }
    assert!(key_named(&server, "too-much").await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn at_the_key_limit_new_key_is_disabled_and_says_why() {
    let server = login_server().await;
    member(&server, Access::Write).await;
    settings::set(
        &server.db.pool,
        SettingKey::MaxKeysPerUser,
        "1",
        "tester",
        Utc::now(),
    )
    .await
    .expect("limit alice to one key");
    alices_key(&server, "laptop").await;
    drive(console_base(&server), |ui| {
        ui.sign_in_password(EMAIL, PASSWORD);
        ui.open("/my-keys");
        ui.wait_text("[data-test=my-key-laptop]", "laptop");
        ui.wait_visible("#my-key-new[disabled]");
        ui.wait_text("#my-keys-note", "the most allowed");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_renames_a_key_then_revokes_it() {
    let server = login_server().await;
    member(&server, Access::Write).await;
    alices_key(&server, "laptop").await;
    let ran = drive(console_base(&server), |ui| {
        ui.sign_in_password(EMAIL, PASSWORD);
        ui.open("/my-keys");
        ui.click("[data-test=my-key-laptop]");
        ui.wait_visible("#my-key-rename-form");
        ui.fill("#my-key-rename-form input[name=name]", "desk");
        ui.click("#my-key-rename-form button[type=submit]");
        ui.wait_text("[data-test=my-key-desk]", "desk");
        ui.wait_text("#drawer-title", "desk");
        ui.click("#my-key-revoke");
        ui.click("#confirm-ok");
        // Revoked keys are hidden until asked for.
        ui.wait_text_gone("#my-keys-table", "desk");
        ui.click("#my-keys-show-old");
        ui.wait_text("[data-test=my-key-state-desk]", "revoked");
    })
    .await;
    if !ran {
        return;
    }
    let key = key_named(&server, "desk").await.expect("the renamed key");
    assert!(key.revoked_at.is_some());
    assert!(key_named(&server, "laptop").await.is_none());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_my_keys_page_says_so_to_a_key() {
    let server = login_server().await;
    let key = server
        .key("console-reader", Access::Read, &["main"], Some("main"))
        .await;
    drive(console_base(&server), move |ui| {
        ui.login(&key);
        // A key has no account: the group is hidden and the pages explain.
        assert!(!ui.page.is_visible("a[data-nav=my-keys]").expect("visible"));
        ui.open("/my-keys");
        ui.wait_text("#my-keys-denied", "signed in with their account");
        assert!(!ui.page.is_visible("#my-key-new").expect("visible"));
        // The API refuses a key its self-service routes too.
        assert_eq!(
            ui.eval(
                "async (key) => (await fetch('/api/v1/me/keys', \
                 { headers: { Authorization: `Bearer ${key}` } })).status",
                json!(key)
            ),
            json!(403)
        );
    })
    .await;
}

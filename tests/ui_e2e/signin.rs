//! Signing in as a person: an emailed setup link, a password, the console
//! showing the user, a reload that resumes the session, sign-out, and
//! signing in again with the password.

use std::time::Duration;

use chrono::Utc;
use okf_mcp::config::Config;
use okf_mcp::mail::{link, SETUP_PATH};
use okf_mcp::mail::{queue as mail_queue, Template};
use okf_mcp::store::email_tokens::{self, Purpose};
use okf_mcp::store::keys::Access;
use okf_mcp::store::sign_in_events::{self, Method, Outcome};
use okf_mcp::store::{ceiling, passkeys, users};
use rustwright::GotoOptions;

use crate::harness::drive;
use crate::support::server::{files, TestServer};

const PASSWORD: &str = "violet-lantern-orbit-42";

/// Makes the page's next `DELETE /api/v1/session` answer 503 without
/// reaching the server; later requests go through.
const FAIL_NEXT_SIGN_OUT: &str = "() => { const real = window.fetch; let failed = false; \
    window.fetch = (input, init) => { \
      if (!failed && init?.method === 'DELETE' && String(input).endsWith('/api/v1/session')) { \
        failed = true; \
        return Promise.resolve(new Response(JSON.stringify({ message: 'unavailable' }), \
          { status: 503, headers: { 'content-type': 'application/json' } })); } \
      return real(input, init); }; }";

/// The status of `GET /api/v1/session` with the page's cookie.
const SESSION_STATUS: &str = "async () => (await fetch('/api/v1/session')).status";

/// A server whose `public_url` is its own address, so the login routes exist.
async fn server() -> TestServer {
    TestServer::start_with(vec![("main", files(&[("index.md", "# Main\n")]))], |base| {
        Config {
            public_url: Some(base.to_string()),
            ..Config::default()
        }
    })
    .await
}

/// The link in a queued mail's payload, as the mailer would send it.
fn link_in(job: &str, path: &str) -> String {
    let at = job.find(path).expect("the mail carries the link");
    let start = job[..at].rfind("http").expect("an absolute link");
    let end = job[at..]
        .find(|c: char| !(c.is_ascii_alphanumeric() || "/?=_-.:".contains(c)))
        .map_or(job.len(), |length| at + length);
    job[start..end].to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_setup_link_sets_a_password_and_the_console_shows_the_user() {
    let server = server().await;
    let pool = &server.db.pool;
    users::create(
        pool,
        &users::NewUser {
            name: "alice",
            display_name: "Alice",
        },
        Utc::now(),
    )
    .await
    .expect("create alice");
    users::set_email(pool, "alice", Some("alice@example.com"))
        .await
        .expect("set the email");
    ceiling::entitle(pool, "alice", "main", Access::Read, Utc::now())
        .await
        .expect("entitle");
    server.app.poll().await.expect("poll");
    let alice = users::find_by_email(pool, "alice@example.com")
        .await
        .expect("find alice")
        .expect("alice exists");
    let token = email_tokens::issue(
        pool,
        &alice.id,
        Purpose::Setup,
        Duration::from_secs(3600),
        Utc::now(),
    )
    .await
    .expect("issue a setup link");
    let template = Template::Setup {
        link: link(&server.base, SETUP_PATH, &token),
        user: "alice".to_string(),
    };
    mail_queue::enqueue(pool, "alice@example.com", &template, Utc::now())
        .await
        .expect("queue the mail");
    let job: String = sqlx::query_scalar("SELECT convert_from(job, 'UTF8') FROM apalis.jobs")
        .fetch_one(pool)
        .await
        .expect("the queued mail");
    let link = link_in(&job, "/account/setup?token=");

    let ran = drive(server.base.clone(), move |ui| {
        ui.page
            .goto(&link, GotoOptions::default())
            .expect("open the setup link");
        ui.wait_text("#setup-form", "Confirm the password");
        ui.wait_text("main strong", "alice");
        ui.fill("#setup-password", PASSWORD);
        ui.fill("#setup-confirm", PASSWORD);
        ui.click("#setup-submit");
        ui.wait_visible("#app");
        ui.wait_text("#whoami", "alice - read");
        assert!(!ui.page.is_visible("[data-need=admin]").expect("visible"));
        // The first sign-out request fails (a 503 from a fetch wrapper):
        // the console says so and stays signed in, on the server too.
        ui.eval(FAIL_NEXT_SIGN_OUT, serde_json::json!(null));
        ui.click("#signout");
        ui.wait_text("#toasts", "Sign-out failed");
        assert!(ui.page.is_visible("#app").expect("visible"));
        assert!(!ui.page.is_visible("#login").expect("visible"));
        assert_eq!(ui.eval(SESSION_STATUS, serde_json::json!(null)), 200);
        // The retry goes through.
        ui.click("#signout");
        ui.wait_visible("#login");
        assert_eq!(ui.eval(SESSION_STATUS, serde_json::json!(null)), 401);
        // Signed out on the server: a reload finds no session.
        ui.open("");
        ui.wait_visible("#login");
        assert!(!ui.page.is_visible("#app").expect("visible"));
        ui.sign_in_password("alice@example.com", PASSWORD);
        ui.wait_text("#whoami", "alice - read");
    })
    .await;
    if !ran {
        return;
    }
    let events = sign_in_events::recent(pool, "alice", 10)
        .await
        .expect("events");
    assert_eq!(events.len(), 2);
    assert!(matches!(
        (&events[0].method, &events[0].outcome),
        (Method::Password, Outcome::Ok)
    ));
    assert!(matches!(
        (&events[1].method, &events[1].outcome),
        (Method::Setup, Outcome::Ok)
    ));
    // `http::serve` gives requests their peer address (plan Decision 7).
    assert_eq!(events[0].ip, "127.0.0.1".parse().ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_password_says_so_and_a_session_survives_a_reload() {
    let server = server().await;
    server
        .user_with_password("bob", "bob@example.com", PASSWORD)
        .await;
    ceiling::entitle(&server.db.pool, "bob", "main", Access::Write, Utc::now())
        .await
        .expect("entitle");
    server.app.poll().await.expect("poll");

    drive(server.base.clone(), move |ui| {
        // The login card links to the account sign-in.
        ui.open("");
        ui.wait_visible("#login");
        ui.click("#login-signin");
        ui.wait_visible("#signin-form");
        ui.fill("#signin-email", "bob@example.com");
        ui.fill("#signin-password", "not-the-password-at-all");
        ui.click("#signin-submit");
        ui.wait_text("#signin-error", "incorrect");
        ui.sign_in_password("bob@example.com", PASSWORD);
        ui.wait_text("#whoami", "bob - write");
        // The cookie session resumes after a reload, with no key stored.
        ui.open("");
        ui.wait_visible("#app");
        ui.wait_text("#whoami", "bob - write");
        assert!(!ui.page.is_visible("#login").expect("visible"));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_account_sign_in_replaces_a_stored_key_and_sign_out_ends_both() {
    let server = server().await;
    server
        .user_with_password("carol", "carol@example.com", PASSWORD)
        .await;
    ceiling::entitle(&server.db.pool, "carol", "main", Access::Read, Utc::now())
        .await
        .expect("entitle");
    let key = server
        .key("console-carol", Access::Read, &["main"], Some("main"))
        .await;

    drive(server.base.clone(), move |ui| {
        // A key pasted earlier stays stored in this browser.
        ui.login(&key);
        ui.wait_text("#whoami", "console-carol");
        // Then an account sign-in: its session wins at boot, and the key
        // is dropped.
        ui.sign_in_password("carol@example.com", PASSWORD);
        ui.wait_text("#whoami", "carol - read");
        assert_eq!(
            ui.eval(
                "() => localStorage.getItem('okf.key')",
                serde_json::json!(null)
            ),
            serde_json::Value::Null
        );
        ui.click("#signout");
        ui.wait_visible("#login");
        assert_eq!(ui.eval(SESSION_STATUS, serde_json::json!(null)), 401);
        // Nothing resumes after a reload.
        ui.open("");
        ui.wait_visible("#login");
        assert!(!ui.page.is_visible("#app").expect("visible"));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn sign_out_ends_a_cookie_session_hidden_behind_a_stored_key() {
    let server = server().await;
    server
        .user_with_password("dave", "dave@example.com", PASSWORD)
        .await;
    let key = server
        .key("console-dave", Access::Read, &["main"], Some("main"))
        .await;

    drive(server.base.clone(), move |ui| {
        ui.sign_in_password("dave@example.com", PASSWORD);
        ui.wait_text("#whoami", "dave - ");
        // A key stored on top of the live session (the console's own store
        // module, so the page uses it at once), as a console that kept a
        // key from before the account sign-in would hold it.
        ui.eval(
            "async (key) => { (await import('/ui/js/core/store.js')).store.key = key; }",
            serde_json::json!(key),
        );
        ui.click("#signout");
        ui.wait_visible("#login");
        // The cookie session ended on the server, and the key is gone.
        assert_eq!(ui.eval(SESSION_STATUS, serde_json::json!(null)), 401);
        assert_eq!(
            ui.eval(
                "() => localStorage.getItem('okf.key')",
                serde_json::json!(null)
            ),
            serde_json::Value::Null
        );
        ui.open("");
        ui.wait_visible("#login");
        assert!(!ui.page.is_visible("#app").expect("visible"));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_session_writes_through_the_console_with_its_csrf_token() {
    let server = server().await;
    server
        .user_with_password("erin", "erin@example.com", PASSWORD)
        .await;
    users::set_admin(&server.db.pool, "erin", true, false)
        .await
        .expect("make erin an admin");
    server.app.poll().await.expect("poll");

    let ran = drive(server.base.clone(), move |ui| {
        ui.sign_in_password("erin@example.com", PASSWORD);
        ui.wait_text("#whoami", "erin - admin");
        // After a reload the console runs on the cookie session alone.
        ui.open("");
        ui.wait_text("#whoami", "erin - admin");
        assert_eq!(
            ui.eval(
                "() => localStorage.getItem('okf.key')",
                serde_json::json!(null)
            ),
            serde_json::Value::Null
        );
        // A write through the UI: without its X-OKF-CSRF header the server
        // refuses it (403) and the user never appears.
        ui.open("/users");
        ui.click("#user-new");
        ui.fill("#user-form input[name=name]", "frank");
        ui.click("#user-form button[type=submit]");
        ui.wait_visible("[data-test=user-frank]");
        // Read back through the API, with the cookie.
        let names = ui.eval(
            "async () => (await (await fetch('/api/v1/users')).json()).users.map(u => u.name)",
            serde_json::json!(null),
        );
        assert!(
            names
                .as_array()
                .is_some_and(|names| names.iter().any(|name| name == "frank")),
            "{names}"
        );
    })
    .await;
    if !ran {
        return;
    }
    users::find(&server.db.pool, "frank")
        .await
        .expect("frank was stored");
}

/// A server whose `public_url` is its own address under the name
/// `localhost`, since WebAuthn needs a domain; the browser goes there too.
async fn passkey_server() -> (TestServer, String) {
    let server =
        TestServer::start_with(vec![("main", files(&[("index.md", "# Main\n")]))], |base| {
            Config {
                public_url: Some(base.replace("127.0.0.1", "localhost")),
                ..Config::default()
            }
        })
        .await;
    let base = server.base.replace("127.0.0.1", "localhost");
    (server, base)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_sign_in_page_offers_a_passkey_under_the_console_csp() {
    let (_server, base) = passkey_server().await;
    let page = format!("{base}/signin");
    drive(base, move |ui| {
        ui.page
            .goto(&page, GotoOptions::default())
            .expect("open the sign-in page");
        // `data-ready` is set by passkey.js, so its module (and webauthn.js)
        // loaded under the CSP; `drive` fails on any console error.
        ui.wait_visible("#signin-passkey[data-ready=true]");
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_setup_link_registers_a_passkey_that_then_signs_in_from_the_sign_in_page() {
    let (server, base) = passkey_server().await;
    let pool = &server.db.pool;
    users::create(
        pool,
        &users::NewUser {
            name: "alice",
            display_name: "Alice",
        },
        Utc::now(),
    )
    .await
    .expect("create alice");
    users::set_email(pool, "alice", Some("alice@example.com"))
        .await
        .expect("set the email");
    ceiling::entitle(pool, "alice", "main", Access::Read, Utc::now())
        .await
        .expect("entitle");
    server.app.poll().await.expect("poll");
    let alice = users::find_by_email(pool, "alice@example.com")
        .await
        .expect("find alice")
        .expect("alice exists");
    let token = email_tokens::issue(
        pool,
        &alice.id,
        Purpose::Setup,
        Duration::from_secs(3600),
        Utc::now(),
    )
    .await
    .expect("issue a setup link");
    let setup = link(&base, SETUP_PATH, &token);

    let ran = drive(base.clone(), move |ui| {
        ui.page
            .goto(&setup, GotoOptions::default())
            .expect("open the setup link");
        let _authenticator = ui.virtual_authenticator();
        ui.wait_visible("#setup-passkey[data-ready=true]");
        ui.fill("#setup-passkey-name", "Laptop");
        ui.click("#setup-passkey");
        ui.wait_text("#whoami", "alice - read");
        ui.click("#signout");
        ui.wait_visible("#login");
        assert_eq!(ui.eval(SESSION_STATUS, serde_json::json!(null)), 401);
        // No email typed: the authenticator offers alice's passkey.
        ui.page
            .goto(&format!("{base}/signin"), GotoOptions::default())
            .expect("open the sign-in page");
        ui.wait_visible("#signin-passkey[data-ready=true]");
        ui.click("#signin-passkey");
        ui.wait_text("#whoami", "alice - read");
        assert_eq!(ui.eval(SESSION_STATUS, serde_json::json!(null)), 200);
    })
    .await;
    if !ran {
        return;
    }
    let stored = passkeys::list(pool, &alice.id).await.expect("list");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].name, "Laptop");
    assert!(stored[0].last_used_at.is_some(), "it signed in");
    let used = email_tokens::peek(pool, &token, Purpose::Setup, Utc::now()).await;
    assert!(used.is_err(), "the setup link is used");
    let events = sign_in_events::recent(pool, "alice", 10)
        .await
        .expect("events");
    assert_eq!(events.len(), 2);
    assert!(matches!(
        (&events[0].method, &events[0].outcome),
        (Method::Passkey, Outcome::Ok)
    ));
    assert!(matches!(
        (&events[1].method, &events[1].outcome),
        (Method::Setup, Outcome::Ok)
    ));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_sign_in_page_offers_each_configured_provider() {
    // Edition 2021: `set_var` is a safe call; no other test reads this name.
    std::env::set_var(
        "OKF_UI_E2E_OIDC_SECRET",
        "test-secret-at-least-32-bytes-long!!",
    );
    // No provider is contacted: the page lists providers from the config.
    let server =
        TestServer::start_with(vec![("main", files(&[("index.md", "# Main\n")]))], |base| {
            Config {
                public_url: Some(base.to_string()),
                auth: okf_mcp::config::AuthConfig {
                    oidc: vec![okf_mcp::config::OidcProviderConfig {
                        id: "example".to_string(),
                        label: "Example ID".to_string(),
                        issuer: "https://id.example.com".to_string(),
                        client_id: "okf-console".to_string(),
                        client_secret_env: "OKF_UI_E2E_OIDC_SECRET".to_string(),
                        scopes: vec!["openid".to_string(), "email".to_string()],
                    }],
                    ..okf_mcp::config::AuthConfig::default()
                },
                ..Config::default()
            }
        })
        .await;
    let page = format!("{}/signin?return_to=%2Fui%2F%23%2Fkeys", server.base);
    drive(server.base.clone(), move |ui| {
        ui.page
            .goto(&page, GotoOptions::default())
            .expect("open the sign-in page");
        ui.wait_text("#signin-oidc-example", "Sign in with Example ID");
        let href = ui.eval(
            "() => document.querySelector('#signin-oidc-example').getAttribute('href')",
            serde_json::json!(null),
        );
        assert_eq!(
            href,
            "/auth/oidc/example/start?return_to=%2Fui%2F%23%2Fkeys"
        );
    })
    .await;
}

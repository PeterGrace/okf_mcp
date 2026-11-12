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
use okf_mcp::store::{ceiling, users};
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

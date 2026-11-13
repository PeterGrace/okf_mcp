//! The Account page (M12 Task 14b): changing the password (and signing in
//! with the new one), a wrong current password refused without signing
//! out, passkeys (an add through the browser's authenticator, rename,
//! remove, an add the browser prompt cancels, and the last sign-in method
//! kept), signing sessions out one at a time (another browser's, and this
//! one's), unlinking an identity, the ceiling as `/me` gives it now and
//! recent sign-ins, linking an identity through a mock OIDC provider (and a
//! link the server refuses), a credential that fails verification at
//! finish, and what a key sees.

use std::time::Duration;

use chrono::Utc;
use okf_mcp::config::AuthConfig;
use okf_mcp::store::keys::Access;
use okf_mcp::store::passwords::{self, SignInOutcome};
use okf_mcp::store::{ceiling, identities, passkeys, user_sessions, users};
use serde_json::json;
use webauthn_authenticator_rs::prelude::WebauthnAuthenticator as _;
use webauthn_authenticator_rs::softpasskey::SoftPasskey;
use webauthn_rs::prelude::{Passkey, Url, Uuid, WebauthnBuilder};

use crate::harness::{drive, Ui};
use crate::oidc_mock::{self as idp, MockIdp};
use crate::self_service::{console_base, login_server, login_server_with, member, EMAIL, PASSWORD};
use crate::support::server::TestServer;

/// The password the first journey changes to.
const NEW_PASSWORD: &str = "a different long passphrase";

/// The status of `GET /api/v1/session` with the page's cookie.
const SESSION_STATUS: &str = "async () => (await fetch('/api/v1/session')).status";

/// alice's user id, for the store calls that take one.
async fn alices_id(pool: &sqlx::PgPool) -> String {
    users::find_by_email(pool, EMAIL)
        .await
        .expect("find alice")
        .expect("alice exists")
        .id
}

/// A passkey for `user_id`, registered with `webauthn-rs`'s software
/// authenticator as the setup page would. Its relying party is
/// `localhost`; the Account page never checks a stored passkey's origin,
/// it only lists, renames and removes it.
fn soft_passkey(user_id: &str) -> Passkey {
    let origin = Url::parse("http://localhost").expect("origin");
    let webauthn = WebauthnBuilder::new("localhost", &origin)
        .expect("relying party")
        .rp_name("okf")
        .build()
        .expect("webauthn");
    let handle = Uuid::parse_str(user_id).expect("user ids are UUIDs");
    let (challenge, state) = webauthn
        .start_passkey_registration(handle, EMAIL, "alice", None)
        .expect("start registration");
    // `true`: report user verification, which passkeys require.
    let mut authenticator = SoftPasskey::new(true);
    let credential = authenticator
        .do_registration(origin, challenge)
        .expect("the authenticator registers");
    webauthn
        .finish_passkey_registration(&credential, &state)
        .expect("finish registration")
}

/// Stores a [`soft_passkey`] of alice's called `name`.
async fn alices_passkey(pool: &sqlx::PgPool, id: &str, name: &str) {
    passkeys::add(pool, id, &soft_passkey(id), name, Utc::now())
        .await
        .expect("store the passkey");
}

/// Starts another console session of alice's, in a browser called
/// `agent`, as signing in elsewhere would.
async fn another_session(pool: &sqlx::PgPool, id: &str, agent: &str) {
    user_sessions::create(
        pool,
        id,
        None,
        agent,
        Duration::from_secs(7 * 86_400),
        Utc::now(),
    )
    .await
    .expect("another session");
}

/// The environment variable that holds the mock provider's client secret.
/// Every journey sets it to the same value, so they can share it.
const OIDC_SECRET_ENV: &str = "OKF_UI_E2E_ACCOUNT_OIDC_SECRET";

/// A [`login_server`] whose one OIDC provider, `example`, is `idp`.
///
/// # Arguments
/// * `idp` - a running mock provider.
///
/// # Returns
/// The running server.
async fn oidc_server(idp_server: &MockIdp) -> TestServer {
    // Edition 2021: `set_var` is a safe call. Every writer sets one value.
    std::env::set_var(OIDC_SECRET_ENV, idp::SECRET);
    login_server_with(AuthConfig {
        oidc: vec![idp::provider(idp_server, OIDC_SECRET_ENV)],
        ..AuthConfig::default()
    })
    .await
}

/// The `data-test` of the Sign out button on the session row whose text
/// contains `agent`, in `#session-table`.
fn revoke_button_of(ui: &Ui, agent: &str) -> String {
    let test = ui.eval(
        "(agent) => [...document.querySelectorAll('#session-table tbody tr')] \
         .find(row => row.textContent.includes(agent)) \
         ?.querySelector('button')?.dataset.test ?? null",
        json!(agent),
    );
    test.as_str()
        .unwrap_or_else(|| panic!("no session row for {agent:?}"))
        .to_string()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_changes_their_password_and_signs_in_with_the_new_one() {
    let server = login_server().await;
    member(&server, Access::Write).await;
    let ran = drive(console_base(&server), |ui| {
        ui.sign_in_password(EMAIL, PASSWORD);
        ui.open("/account");
        ui.wait_visible("#password-form");
        ui.fill("#password-form input[name=current]", PASSWORD);
        ui.fill("#password-form input[name=password]", NEW_PASSWORD);
        ui.fill("#password-form input[name=confirm]", NEW_PASSWORD);
        ui.click("#password-form button[type=submit]");
        ui.wait_text("#toasts", "Password changed");
        ui.click("#signout");
        ui.wait_visible("#login");
        ui.sign_in_password(EMAIL, NEW_PASSWORD);
        ui.wait_text("#whoami", "alice");
    })
    .await;
    if !ran {
        return;
    }
    let pool = &server.db.pool;
    let old = passwords::verify(pool, EMAIL, PASSWORD, Utc::now())
        .await
        .expect("verify the old password");
    assert!(matches!(old, SignInOutcome::Failed));
    let new = passwords::verify(pool, EMAIL, NEW_PASSWORD, Utc::now())
        .await
        .expect("verify the new password");
    assert!(matches!(new, SignInOutcome::Ok(_)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_wrong_current_password_is_refused_and_keeps_the_session() {
    let server = login_server().await;
    member(&server, Access::Write).await;
    let ran = drive(console_base(&server), |ui| {
        ui.sign_in_password(EMAIL, PASSWORD);
        ui.open("/account");
        ui.fill(
            "#password-form input[name=current]",
            "not the password at all",
        );
        ui.fill("#password-form input[name=password]", NEW_PASSWORD);
        ui.fill("#password-form input[name=confirm]", NEW_PASSWORD);
        ui.click("#password-form button[type=submit]");
        // The 403's own reason, not the generic "cannot do this".
        ui.wait_text("#toasts", "the current password is incorrect");
        assert!(!ui.text("#toasts").contains("cannot do this"));
        // Still signed in, still on the page: a 403 is not a 401.
        assert!(ui.page.is_visible("#password-form").expect("visible"));
        assert!(!ui.page.is_visible("#login").expect("visible"));
        assert_eq!(ui.eval(SESSION_STATUS, json!(null)), json!(200));
        ui.wait_text("#whoami", "alice");
    })
    .await;
    if !ran {
        return;
    }
    let kept = passwords::verify(&server.db.pool, EMAIL, PASSWORD, Utc::now())
        .await
        .expect("verify");
    assert!(
        matches!(kept, SignInOutcome::Ok(_)),
        "the password is unchanged"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn mismatched_new_passwords_are_caught_before_sending() {
    let server = login_server().await;
    member(&server, Access::Write).await;
    let ran = drive(console_base(&server), |ui| {
        ui.sign_in_password(EMAIL, PASSWORD);
        ui.open("/account");
        ui.fill("#password-form input[name=current]", PASSWORD);
        ui.fill("#password-form input[name=password]", NEW_PASSWORD);
        ui.fill(
            "#password-form input[name=confirm]",
            "something else entirely",
        );
        ui.click("#password-form button[type=submit]");
        ui.wait_text("#toasts", "differ");
        // Still signed in, still on the page.
        ui.wait_visible("#password-form");
    })
    .await;
    if !ran {
        return;
    }
    let kept = passwords::verify(&server.db.pool, EMAIL, PASSWORD, Utc::now())
        .await
        .expect("verify");
    assert!(matches!(kept, SignInOutcome::Ok(_)), "nothing was sent");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_adds_a_passkey_with_the_browser_and_other_sessions_end() {
    let server = login_server().await;
    member(&server, Access::Write).await;
    let pool = &server.db.pool;
    let id = alices_id(pool).await;
    another_session(pool, &id, "phone browser").await;
    let ran = drive(console_base(&server), |ui| {
        ui.sign_in_password(EMAIL, PASSWORD);
        let _authenticator = ui.virtual_authenticator();
        ui.open("/account");
        ui.wait_text("#session-table", "phone browser");
        ui.wait_text("#passkey-list", "No passkeys yet");
        ui.fill("#passkey-name", "laptop");
        // "Keep me signed in elsewhere" stays unticked.
        ui.click("#passkey-add");
        ui.wait_text("#toasts", "Added passkey laptop");
        ui.wait_visible("[data-test=passkey-laptop]");
        ui.wait_text_gone("#session-table", "phone browser");
        ui.wait_text("#session-table", "this browser");
    })
    .await;
    if !ran {
        return;
    }
    let stored = passkeys::list(pool, &id).await.expect("list passkeys");
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].name, "laptop");
    let left = user_sessions::list(pool, &id, None)
        .await
        .expect("list sessions");
    assert_eq!(left.len(), 1, "the phone's session ended; this one is kept");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_renames_and_removes_a_passkey_and_a_cancelled_add_adds_none() {
    let server = login_server().await;
    member(&server, Access::Write).await;
    let id = alices_id(&server.db.pool).await;
    alices_passkey(&server.db.pool, &id, "laptop").await;
    let ran = drive(console_base(&server), |ui| {
        ui.sign_in_password(EMAIL, PASSWORD);
        ui.open("/account");
        ui.wait_visible("[data-test=passkey-laptop]");
        // Rename: a write signed in by session, so it also proves the
        // console sends the CSRF header.
        ui.fill("[data-test=passkey-laptop] input", "work-laptop");
        ui.click("[data-test=passkey-rename-laptop]");
        ui.wait_visible("[data-test=passkey-work-laptop]");
        // A cancelled browser prompt adds nothing, is no error toast and
        // no script error.
        ui.eval(
            "() => { navigator.credentials.create = () => Promise.reject( \
                 new DOMException('The prompt was cancelled.', 'NotAllowedError')); \
             return true; }",
            json!(null),
        );
        ui.fill("#passkey-name", "phone");
        ui.click("#passkey-add");
        ui.wait_text("#toasts .toast-info", "No passkey was added");
        assert!(!ui.page.is_visible("#toasts .toast-error").expect("visible"));
        assert!(!ui
            .page
            .is_visible("[data-test=passkey-phone]")
            .expect("visible"));
        // Remove: alice still has a password, so this is not her last method.
        ui.click("[data-test=passkey-remove-work-laptop]");
        ui.click("#confirm-ok");
        ui.wait_text("#passkey-list", "No passkeys yet");
    })
    .await;
    if !ran {
        return;
    }
    let left = passkeys::list(&server.db.pool, &id).await.expect("list");
    assert!(left.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_last_sign_in_method_stays_and_the_page_says_why() {
    let server = login_server().await;
    member(&server, Access::Write).await;
    let pool = server.db.pool.clone();
    let id = alices_id(&pool).await;
    alices_passkey(&pool, &id, "laptop").await;
    // The journey runs on a blocking thread; this handle lets it change the
    // store on the test's runtime mid-session.
    let runtime = tokio::runtime::Handle::current();
    let (journey_pool, journey_id) = (pool.clone(), id.clone());
    let ran = drive(console_base(&server), move |ui| {
        ui.sign_in_password(EMAIL, PASSWORD);
        // Signed in, alice is left with the passkey alone (as if she had
        // only ever set up a passkey).
        runtime.block_on(async {
            sqlx::query("UPDATE users SET password_hash = NULL WHERE id = $1")
                .bind(&journey_id)
                .execute(&journey_pool)
                .await
                .expect("drop alice's password");
        });
        ui.open("/account");
        ui.click("[data-test=passkey-remove-laptop]");
        ui.click("#confirm-ok");
        // The 409's reason.
        ui.wait_text("#toasts", "last way to sign in");
        ui.wait_visible("[data-test=passkey-laptop]");
    })
    .await;
    if !ran {
        return;
    }
    let left = passkeys::list(&pool, &id).await.expect("list");
    assert_eq!(left.len(), 1, "the last sign-in method is kept");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_signs_out_other_sessions_one_at_a_time() {
    let server = login_server().await;
    member(&server, Access::Write).await;
    let pool = &server.db.pool;
    let id = alices_id(pool).await;
    for agent in ["phone browser", "tablet browser"] {
        another_session(pool, &id, agent).await;
    }
    let ran = drive(console_base(&server), |ui| {
        ui.sign_in_password(EMAIL, PASSWORD);
        ui.open("/account");
        ui.wait_text("#session-table", "phone browser");
        ui.wait_text("#session-table", "this browser");
        let phone = revoke_button_of(ui, "phone browser");
        ui.click(&format!("[data-test={phone}]"));
        ui.wait_text_gone("#session-table", "phone browser");
        // Each other session has its own Sign out (Task 10b's
        // DELETE /api/v1/me/sessions/{id_prefix}).
        let tablet = revoke_button_of(ui, "tablet browser");
        ui.click(&format!("[data-test={tablet}]"));
        ui.wait_text_gone("#session-table", "tablet browser");
        // This browser is still signed in, and its session still works.
        ui.wait_text("#session-table", "this browser");
        assert_eq!(ui.eval(SESSION_STATUS, json!(null)), json!(200));
        assert!(!ui.page.is_visible("#login").expect("visible"));
    })
    .await;
    if !ran {
        return;
    }
    let left = user_sessions::list(pool, &id, None).await.expect("list");
    assert_eq!(left.len(), 1, "only the journey's own session is left");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn signing_this_browser_out_from_the_account_page_ends_its_session() {
    let server = login_server().await;
    member(&server, Access::Write).await;
    let pool = &server.db.pool;
    let id = alices_id(pool).await;
    another_session(pool, &id, "phone browser").await;
    let ran = drive(console_base(&server), |ui| {
        ui.sign_in_password(EMAIL, PASSWORD);
        ui.open("/account");
        ui.wait_text("#session-table", "phone browser");
        let here = revoke_button_of(ui, "this browser");
        ui.click(&format!("[data-test={here}]"));
        ui.click("#confirm-ok");
        // Signed out cleanly: the card at the console's root, no session.
        ui.wait_visible("#login");
        assert!(!ui.page.is_visible("#app").expect("visible"));
        assert_eq!(ui.eval(SESSION_STATUS, json!(null)), json!(401));
        assert_eq!(
            ui.eval("() => location.pathname + location.hash", json!(null)),
            json!("/ui/")
        );
        // A reload stays on the card.
        ui.open("");
        ui.wait_visible("#login");
    })
    .await;
    if !ran {
        return;
    }
    let left = user_sessions::list(pool, &id, None).await.expect("list");
    assert_eq!(left.len(), 1, "the phone's session is untouched");
    assert_eq!(left[0].user_agent, "phone browser");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_unlinks_an_identity_and_sees_their_ceiling_and_sign_ins() {
    let server = login_server().await;
    member(&server, Access::Write).await;
    let pool = server.db.pool.clone();
    let id = alices_id(&pool).await;
    identities::link(&pool, &id, "example", "subject-1", EMAIL, Utc::now())
        .await
        .expect("link an identity");
    let app = std::sync::Arc::clone(&server.app);
    let runtime = tokio::runtime::Handle::current();
    let journey_pool = pool.clone();
    let ran = drive(console_base(&server), move |ui| {
        ui.sign_in_password(EMAIL, PASSWORD);
        ui.open("/account");
        ui.wait_text("[data-test=ceiling-main]", "main");
        ui.wait_text("#ceiling-table", "write");
        assert!(!ui.text("#ceiling-table").contains("other"));
        // The journey's own sign-in is the latest.
        ui.wait_text("#sign-in-table", "password");
        ui.wait_text("[data-test=identity-example]", EMAIL);
        ui.click("[data-test=identity-unlink-example]");
        ui.click("#confirm-ok");
        ui.wait_text("#identity-list", "No linked accounts");
        // An admin entitles alice to `other` mid-session: the page shows
        // the ceiling /me gives now, not the one she signed in with.
        runtime.block_on(async {
            ceiling::entitle(&journey_pool, "alice", "other", Access::Read, Utc::now())
                .await
                .expect("entitle alice to other");
            app.poll().await.expect("poll");
        });
        ui.click("a[data-nav=my-keys]");
        ui.wait_visible("#my-keys-table");
        ui.click("a[data-nav=account]");
        ui.wait_text("[data-test=ceiling-other]", "other");
    })
    .await;
    if !ran {
        return;
    }
    let left = identities::list(&pool, &id).await.expect("list");
    assert!(left.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_bad_credential_at_finish_shows_the_reason_and_keeps_the_session() {
    let server = login_server().await;
    member(&server, Access::Write).await;
    let id = alices_id(&server.db.pool).await;
    let ran = drive(console_base(&server), |ui| {
        ui.sign_in_password(EMAIL, PASSWORD);
        let _authenticator = ui.virtual_authenticator();
        ui.open("/account");
        ui.wait_text("#passkey-list", "No passkeys yet");
        // The page's own fetch, wrapped: the finish request's client data
        // claims a sign-in (`webauthn.get`), so the server cannot verify it.
        ui.eval(
            "() => { const original = window.fetch; \
                 const b64 = text => btoa(text).replace(/\\+/g, '-').replace(/\\//g, '_').replace(/=+$/, ''); \
                 const unb64 = text => atob(text.replace(/-/g, '+').replace(/_/g, '/')); \
                 window.fetch = (url, init) => { \
                     if (String(url).endsWith('/register/finish')) { \
                         const body = JSON.parse(init.body); \
                         const data = unb64(body.response.clientDataJSON).replace('webauthn.create', 'webauthn.get'); \
                         body.response.clientDataJSON = b64(data); \
                         init = { ...init, body: JSON.stringify(body) }; \
                     } \
                     return original(url, init); }; \
                 return true; }",
            json!(null),
        );
        ui.fill("#passkey-name", "laptop");
        ui.click("#passkey-add");
        ui.wait_text("#toasts", "could not be registered");
        // Still signed in: a bad credential is not a 401.
        assert!(!ui.page.is_visible("#login").expect("visible"));
        assert_eq!(ui.eval(SESSION_STATUS, json!(null)), json!(200));
        ui.wait_text("#whoami", "alice");
        ui.wait_text("#passkey-list", "No passkeys yet");
    })
    .await;
    if !ran {
        return;
    }
    let stored = passkeys::list(&server.db.pool, &id).await.expect("list");
    assert!(stored.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_links_an_identity_and_keeps_their_other_sessions() {
    let provider = idp::start().await;
    provider.will_issue("subject-1", Some("alice@id.example.com"), Some(true));
    let server = oidc_server(&provider).await;
    member(&server, Access::Write).await;
    let pool = &server.db.pool;
    let id = alices_id(pool).await;
    another_session(pool, &id, "phone browser").await;
    let ran = drive(console_base(&server), |ui| {
        ui.sign_in_password(EMAIL, PASSWORD);
        ui.open("/account");
        ui.wait_text("[data-test=identity-link-example]", "Link Example ID");
        ui.click("#identity-keep-sessions");
        ui.click("[data-test=identity-link-example]");
        // Off to the provider, which signs in at once and sends the browser
        // to the callback, which links and comes back to the Account page.
        ui.wait_text("[data-test=identity-example]", "alice@id.example.com");
        assert_eq!(
            ui.eval("() => location.pathname + location.hash", json!(null)),
            json!("/ui/#/account")
        );
        assert!(!ui
            .page
            .is_visible("[data-test=identity-link-example]")
            .expect("visible"));
        // "Keep me signed in elsewhere" was ticked.
        ui.wait_text("#session-table", "phone browser");
    })
    .await;
    if !ran {
        return;
    }
    let linked = identities::list(pool, &id).await.expect("list identities");
    assert_eq!(linked.len(), 1);
    assert_eq!(linked[0].provider, "example");
    assert_eq!(linked[0].email_at_link, "alice@id.example.com");
    let sessions = user_sessions::list(pool, &id, None)
        .await
        .expect("list sessions");
    assert_eq!(sessions.len(), 2, "the phone's session is kept");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_refused_link_shows_the_reason_and_stays_on_the_page() {
    let provider = idp::start().await;
    // Discovery fails, so the server cannot start the link (503).
    provider
        .discovery_down
        .store(true, std::sync::atomic::Ordering::SeqCst);
    let server = oidc_server(&provider).await;
    member(&server, Access::Write).await;
    let id = alices_id(&server.db.pool).await;
    let ran = drive(console_base(&server), |ui| {
        ui.sign_in_password(EMAIL, PASSWORD);
        ui.open("/account");
        ui.click("[data-test=identity-link-example]");
        ui.wait_text("#toasts", "discovery for example failed");
        assert_eq!(
            ui.eval("() => location.pathname + location.hash", json!(null)),
            json!("/ui/#/account")
        );
        assert!(ui
            .page
            .is_visible("[data-test=identity-link-example]")
            .expect("visible"));
        assert_eq!(ui.eval(SESSION_STATUS, json!(null)), json!(200));
    })
    .await;
    if !ran {
        return;
    }
    let linked = identities::list(&server.db.pool, &id).await.expect("list");
    assert!(linked.is_empty());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn my_grants_and_account_say_so_to_a_key() {
    let server = login_server().await;
    let key = server
        .key("console-reader", Access::Read, &["main"], Some("main"))
        .await;
    drive(console_base(&server), move |ui| {
        ui.login(&key);
        assert!(!ui
            .page
            .is_visible("a[data-nav=my-grants]")
            .expect("visible"));
        assert!(!ui.page.is_visible("a[data-nav=account]").expect("visible"));
        ui.open("/my-grants");
        ui.wait_text("#my-grants-denied", "signed in with their account");
        ui.open("/account");
        ui.wait_text("#account-denied", "signed in with their account");
        assert!(!ui.page.is_visible("#password-form").expect("visible"));
    })
    .await;
}

//! The Settings page (M12 Task 15): a policy override and its reset, a
//! refused value and its reason, the read-only mail provider, Send test
//! email, and the mail queue's Retry and Delete; what a scoped admin and a
//! member see.

use chrono::Utc;
use okf_mcp::config::AuthConfig;
use okf_mcp::mail::queue;
use okf_mcp::mail::Template;
use okf_mcp::store::keys::Access;
use okf_mcp::store::settings::{self, SettingKey};
use serde_json::json;

use crate::harness::{drive, Ui};
use crate::self_service::{console_base, login_server, member, EMAIL, PASSWORD};
use crate::support::server::TestServer;

/// `max_keys_per_user` as `GET /api/v1/settings` answers it now, read with
/// the key the console stored: the server's state, not the page's.
const MAX_KEYS_SETTING: &str = "async () => { \
    const headers = { Authorization: `Bearer ${localStorage.getItem('okf.key')}` }; \
    const { settings } = await (await fetch('/api/v1/settings', { headers })).json(); \
    return settings.find(row => row.key === 'max_keys_per_user'); }";

/// Queues a test mail to `to`, then marks it failed for good (apalis's
/// `Killed`, which no worker fetches again) with an SMTP error, as a
/// permanent provider refusal would.
///
/// # Arguments
/// * `server` - a [`login_server`].
/// * `to` - the recipient, unique to this mail.
///
/// # Returns
/// The job's id, as the queue lists it.
///
/// # Panics
/// If a database call fails or the mail is not listed.
async fn failed_mail(server: &TestServer, to: &str) -> String {
    let pool = &server.db.pool;
    queue::enqueue(pool, to, &Template::Test, Utc::now())
        .await
        .expect("queue a mail");
    sqlx::query(
        "UPDATE apalis.jobs \
         SET status = 'Killed', attempts = 1, done_at = now(), \
             last_result = '{\"Err\": \"550 mailbox unavailable\"}'::jsonb \
         WHERE convert_from(job, 'UTF8')::jsonb -> 'mail' ->> 'to' = $1",
    )
    .bind(to)
    .execute(pool)
    .await
    .expect("fail the mail");
    queue::list(pool)
        .await
        .expect("list the queue")
        .into_iter()
        .find(|mail| mail.to == to)
        .expect("the failed mail is listed")
        .id
}

/// Waits until the queue row's status cell no longer reads "failed" (the
/// retried job is pending, or already sent and gone).
///
/// # Panics
/// When it still reads "failed" after 10 seconds.
fn wait_not_failed(ui: &Ui, id: &str) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        let status = ui.eval(
            "(id) => document.querySelector(`[data-test=mail-status-${id}]`)?.textContent ?? 'gone'",
            json!(id),
        );
        if status != json!("failed") {
            return;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the mail stayed failed"
        );
        std::thread::sleep(std::time::Duration::from_millis(50));
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_overrides_a_setting_then_resets_it_to_the_default() {
    let server = login_server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    let ran = drive(console_base(&server), move |ui| {
        ui.login(&admin);
        ui.click("a[data-nav=settings]");
        ui.wait_text("[data-test=setting-source-max_keys_per_user]", "default");
        // A value the server refuses changes nothing, and its reason stays
        // beside the setting.
        ui.fill("[data-test=setting-input-max_keys_per_user]", "many");
        ui.click("[data-test=setting-save-max_keys_per_user]");
        ui.wait_text("#toasts", "max_keys_per_user");
        ui.wait_text(
            "[data-test=setting-error-max_keys_per_user]",
            "whole number",
        );
        ui.wait_text("[data-test=setting-source-max_keys_per_user]", "default");
        assert_eq!(
            ui.eval(MAX_KEYS_SETTING, json!(null))["overridden"],
            json!(false)
        );
        // A valid one becomes an override, and the refusal goes.
        ui.fill("[data-test=setting-input-max_keys_per_user]", "5");
        ui.click("[data-test=setting-save-max_keys_per_user]");
        ui.wait_text("[data-test=setting-source-max_keys_per_user]", "override");
        ui.wait_text(
            "[data-test=setting-source-max_keys_per_user]",
            "console-admin",
        );
        ui.wait_hidden("[data-test=setting-error-max_keys_per_user]");
        let saved = ui.eval(MAX_KEYS_SETTING, json!(null));
        assert_eq!(saved["value"], json!("5"));
        assert_eq!(saved["overridden"], json!(true));
        ui.click("[data-test=setting-reset-max_keys_per_user]");
        ui.wait_text("[data-test=setting-source-max_keys_per_user]", "default");
        ui.wait_visible("[data-test=setting-reset-max_keys_per_user][disabled]");
    })
    .await;
    if !ran {
        return;
    }
    let rows = settings::list(&server.db.pool, &AuthConfig::default())
        .await
        .expect("list settings");
    let row = rows
        .iter()
        .find(|row| row.key == SettingKey::MaxKeysPerUser)
        .expect("the row");
    assert!(!row.overridden);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_admin_sends_a_test_email_and_retries_and_deletes_failed_mail() {
    let server = login_server().await;
    let admin = server.key("console-admin", Access::Admin, &[], None).await;
    let retried = failed_mail(&server, "bob@example.com").await;
    let deleted = failed_mail(&server, "dave@example.com").await;
    let (retried_id, deleted_id) = (retried.clone(), deleted.clone());
    let ran = drive(console_base(&server), move |ui| {
        ui.login(&admin);
        ui.open("/settings");
        // The provider is read-only: the test config's `log`.
        ui.wait_text("#settings-mail", "log");
        assert_eq!(
            ui.eval(
                "() => document.querySelectorAll('#settings-mail input, #settings-mail select').length",
                json!(null)
            ),
            json!(0)
        );
        ui.fill("#mail-test-form input[name=to]", "carol@example.com");
        ui.click("#mail-test-form button[type=submit]");
        ui.wait_text("#toasts", "Queued a test email to carol@example.com");
        // The queue is read again: the new mail is listed.
        ui.wait_text("#mail-queue-table", "carol@example.com");
        ui.wait_text(&format!("[data-test=mail-status-{retried}]"), "failed");
        ui.wait_text(&format!("[data-test=mail-error-{retried}]"), "550");
        ui.click(&format!("[data-test=mail-retry-{retried}]"));
        ui.wait_text("#toasts", "bob@example.com");
        wait_not_failed(ui, &retried);
        ui.click(&format!("[data-test=mail-delete-{deleted}]"));
        ui.click("#confirm-ok");
        ui.wait_hidden(&format!("[data-test=mail-status-{deleted}]"));
    })
    .await;
    if !ran {
        return;
    }
    let left = queue::list(&server.db.pool).await.expect("list the queue");
    assert!(left.iter().all(|mail| mail.id != deleted_id));
    let again = left
        .iter()
        .find(|mail| mail.id == retried_id)
        .expect("the retried mail is still queued (no worker runs here)");
    assert_eq!(again.status, "pending");
    assert!(left.iter().any(|mail| mail.to == "carol@example.com"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_scoped_admin_sees_settings_read_only() {
    let server = login_server().await;
    let scoped = server
        .key("console-scoped", Access::Admin, &["main"], Some("main"))
        .await;
    let failed = failed_mail(&server, "bob@example.com").await;
    drive(console_base(&server), move |ui| {
        ui.login(&scoped);
        ui.open("/settings");
        ui.wait_text("#settings-readonly", "every catalog");
        ui.wait_text("[data-test=setting-source-max_keys_per_user]", "default");
        ui.wait_visible("[data-test=setting-input-max_keys_per_user][disabled]");
        ui.wait_visible("[data-test=setting-save-max_keys_per_user][disabled]");
        ui.wait_visible("#mail-test-form button[type=submit][disabled]");
        ui.wait_visible(&format!("[data-test=mail-retry-{failed}][disabled]"));
        ui.wait_visible(&format!("[data-test=mail-delete-{failed}][disabled]"));
    })
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_has_no_settings_page() {
    let server = login_server().await;
    member(&server, Access::Write).await;
    drive(console_base(&server), |ui| {
        ui.sign_in_password(EMAIL, PASSWORD);
        ui.wait_text("#whoami", "alice");
        assert!(!ui.page.is_visible("a[data-nav=settings]").expect("visible"));
        ui.open("/settings");
        ui.wait_text("#settings-denied", "admin");
    })
    .await;
}

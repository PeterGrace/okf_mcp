//! Setup emails (spec "Emailed links": Setup): an admin sends a user a
//! single-use link to choose a password or register a passkey. The API's
//! `POST /api/v1/users/{name}/invite` and the CLI's `user invite` (Task 13)
//! share this.

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::PgPool;

use crate::config::Config;
use crate::error::OkfError;
use crate::mail::{link, queue, Template, SETUP_PATH};
use crate::store::audit::RowLock;
use crate::store::audit::{self, Action, AuditActor};
use crate::store::email_tokens::{self, Purpose};
use crate::store::settings;
use crate::store::users;

/// Issues a setup link for `user`, queues the setup email and records
/// `user.invited`, in one transaction under the user's row lock: the link
/// goes to the address the user has at the commit, and a link exists only
/// if its mail is queued and the invite is in the audit log. Any older
/// setup link of the user stops working. The link is never returned or
/// logged: only the queued job holds it.
///
/// The user's row is locked first (`FOR NO KEY UPDATE`, which conflicts
/// with the `FOR UPDATE` of `users::set_admin`; with the actor's rows, as
/// `audit::lock_people` orders them), and a provisioner is
/// refused an admin user before anything else is read, so whether an admin
/// has an email is never told to it.
///
/// # Arguments
/// * `pool` - the database (the job goes into apalis on it).
/// * `config` - for `public_url` (the link's origin) and the `[auth]`
///   defaults under the stored settings (`setup_link_ttl`).
/// * `user` - the user's name.
/// * `actor` - who, for the audit log; a provisioner is refused an admin.
/// * `now` - the server clock.
///
/// # Returns
/// The address the email goes to.
///
/// # Errors
/// [`OkfError::Refused`] without `public_url` or without an email;
/// [`OkfError::Forbidden`] for a provisioner and an admin user;
/// [`OkfError::UnknownName`] for an unknown user; [`OkfError::Database`].
pub async fn send_setup(
    pool: &PgPool,
    config: &Config,
    user: &str,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<String, OkfError> {
    let Some(public_url) = config.public_url.as_deref() else {
        return Err(OkfError::Refused {
            reason: "setup links need public_url in the config".to_string(),
        });
    };
    // Code without an `App` reads the settings itself (plan Decision 24).
    let policy = settings::load(pool, &config.auth).await?;
    let mut tx = pool.begin().await?;
    // The lock `issue_for_user_in` takes next, taken here first (with the
    // actor's rows, in `audit::lock_people`' order) so the guard reads the
    // flag under it. A weaker lock first and this one after would let two
    // invites deadlock upgrading theirs.
    let (_, is_admin) = users::lock_target(&mut tx, user, RowLock::NoKeyUpdate, actor).await?;
    audit::refuse_admin_target(actor, is_admin, user)?;
    let issued =
        email_tokens::issue_for_user_in(&mut tx, user, Purpose::Setup, policy.setup_link_ttl, now)
            .await?;
    let Some(issued) = issued else {
        // Nothing was written; dropping `tx` rolls back and releases the lock.
        return Err(OkfError::Refused {
            reason: format!("user {user} has no email; set one first"),
        });
    };
    let template = Template::Setup {
        // The one link builder, so the path matches the setup page.
        link: link(public_url, SETUP_PATH, &issued.token),
        user: issued.user.name.clone(),
    };
    queue::enqueue_in(&mut tx, &issued.to, &template, now).await?;
    let entry = users::user_entry(Action::UserInvited, &issued.user.id, json!({}));
    audit::record(&mut tx, actor, &entry, now).await?;
    tx.commit().await?;
    tracing::info!(%user, "setup email queued");
    Ok(issued.to)
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use chrono::Utc;

    use super::send_setup;
    use crate::config::Config;
    use crate::db::{test_db, TEST_USER};
    use crate::error::OkfError;
    use crate::http::api::testing::{fail_on, wait_for_lock_waiters};
    use crate::mail::queue;
    use crate::store::audit::testing::{entries, RecordedEntry};
    use crate::store::audit::AuditActor;
    use crate::store::users::{self, AdminChange};

    /// Sets `user`'s email as the CLI.
    async fn set_email(pool: &sqlx::PgPool, user: &str, email: &str) {
        users::set_email(pool, user, Some(email), &AuditActor::cli(), Utc::now())
            .await
            .expect("email");
    }

    fn with_public_url() -> Config {
        Config {
            public_url: Some("https://okf.example.com".to_string()),
            ..Config::default()
        }
    }

    /// How many setup tokens exist.
    async fn setup_tokens(pool: &sqlx::PgPool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM email_tokens WHERE purpose = 'setup'")
            .fetch_one(pool)
            .await
            .expect("count")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_setup_link_is_queued_for_a_user_with_an_email() {
        let db = test_db().await;
        users::set_email(
            &db.pool,
            TEST_USER,
            Some("tester@example.com"),
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .expect("email");

        let first = send_setup(
            &db.pool,
            &with_public_url(),
            TEST_USER,
            &AuditActor::cli(),
            Utc::now(),
        )
        .await;
        let second = send_setup(
            &db.pool,
            &with_public_url(),
            TEST_USER,
            &AuditActor::cli(),
            Utc::now(),
        )
        .await;

        assert_eq!(first.expect("queued"), "tester@example.com");
        assert_eq!(second.expect("queued again"), "tester@example.com");
        let queued = queue::list(&db.pool).await.expect("the queue");
        assert_eq!(queued.len(), 2);
        assert_eq!(queued[0].to, "tester@example.com");
        assert_eq!(queued[0].template, "setup");
        assert_eq!(
            setup_tokens(&db.pool).await,
            1,
            "a new link replaces the old"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn no_email_or_no_public_url_is_refused_and_queues_nothing() {
        let db = test_db().await;

        let no_email = send_setup(
            &db.pool,
            &with_public_url(),
            TEST_USER,
            &AuditActor::cli(),
            Utc::now(),
        )
        .await;
        users::set_email(
            &db.pool,
            TEST_USER,
            Some("tester@example.com"),
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .expect("email");
        let no_url = send_setup(
            &db.pool,
            &Config::default(),
            TEST_USER,
            &AuditActor::cli(),
            Utc::now(),
        )
        .await;
        let unknown = send_setup(
            &db.pool,
            &with_public_url(),
            "nobody",
            &AuditActor::cli(),
            Utc::now(),
        )
        .await;
        let malformed = send_setup(
            &db.pool,
            &with_public_url(),
            "a\0b",
            &AuditActor::cli(),
            Utc::now(),
        )
        .await;

        assert!(
            matches!(no_email, Err(OkfError::Refused { .. })),
            "{no_email:?}"
        );
        assert!(
            matches!(no_url, Err(OkfError::Refused { .. })),
            "{no_url:?}"
        );
        assert!(
            matches!(unknown, Err(OkfError::UnknownName { .. })),
            "{unknown:?}"
        );
        assert!(
            matches!(malformed, Err(OkfError::UnknownName { .. })),
            "a NUL is an unknown user, not a database error: {malformed:?}"
        );
        assert!(queue::list(&db.pool).await.expect("the queue").is_empty());
        assert_eq!(setup_tokens(&db.pool).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_setup_link_goes_to_the_address_the_user_has_when_it_commits() {
        // An email change that holds the user's row while the invite starts
        // commits first; the invite must then read the new address, never
        // mail a working link to the old one.
        let db = test_db().await;
        users::set_email(
            &db.pool,
            TEST_USER,
            Some("old@example.com"),
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .expect("email");
        let mut holder = db.pool.begin().await.expect("begin");
        sqlx::query("SELECT id FROM users WHERE name = $1 FOR NO KEY UPDATE")
            .bind(TEST_USER)
            .execute(&mut *holder)
            .await
            .expect("hold the user");
        let pool = db.pool.clone();
        // `tokio::spawn` runs the invite on another task, so it can wait on
        // the row while this task commits the change.
        let invite = tokio::spawn(async move {
            send_setup(
                &pool,
                &with_public_url(),
                TEST_USER,
                &AuditActor::cli(),
                Utc::now(),
            )
            .await
        });
        tokio::time::timeout(Duration::from_secs(30), wait_for_lock_waiters(&db.pool, 1))
            .await
            .expect("the invite never waited on the user's row");
        sqlx::query("UPDATE users SET email = 'new@example.com' WHERE name = $1")
            .bind(TEST_USER)
            .execute(&mut *holder)
            .await
            .expect("change the email");
        holder.commit().await.expect("commit the change");

        let sent = tokio::time::timeout(Duration::from_secs(30), invite)
            .await
            .expect("the invite never finished")
            .expect("the invite task");

        assert_eq!(sent.expect("queued"), "new@example.com");
        let queued = queue::list(&db.pool).await.expect("the queue");
        assert_eq!(queued.len(), 1);
        assert_eq!(queued[0].to, "new@example.com");
    }

    /// The `user.invited` entries, oldest first.
    async fn invited(pool: &sqlx::PgPool) -> Vec<RecordedEntry> {
        entries(pool, "user.invited").await
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_invite_records_one_entry_and_a_refused_one_none() {
        let db = test_db().await;
        set_email(&db.pool, TEST_USER, "tester@example.com").await;
        let cli = AuditActor::cli();

        send_setup(&db.pool, &with_public_url(), TEST_USER, &cli, Utc::now())
            .await
            .expect("queued");
        users::set_email(&db.pool, TEST_USER, None, &cli, Utc::now())
            .await
            .expect("clear the email");
        let no_email = send_setup(&db.pool, &with_public_url(), TEST_USER, &cli, Utc::now()).await;

        assert!(
            matches!(no_email, Err(OkfError::Refused { .. })),
            "{no_email:?}"
        );
        let recorded = invited(&db.pool).await;
        assert_eq!(recorded.len(), 1, "the refused invite recorded nothing");
        assert_eq!(recorded[0].person.as_deref(), Some(TEST_USER));
        assert_eq!(recorded[0].target_kind, "user");
        assert_eq!(recorded[0].details, serde_json::json!({}));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_provisioner_cannot_invite_an_admin_with_or_without_an_email() {
        let db = test_db().await;
        let admin = AdminChange {
            on: true,
            allow_last: true,
        };
        users::set_admin(&db.pool, TEST_USER, admin, &AuditActor::cli(), Utc::now())
            .await
            .expect("make the fixture an admin");
        let provisioner = AuditActor {
            provisioner: true,
            ..AuditActor::cli()
        };

        // Forbidden, not "no email": the refusal tells nothing about the admin.
        let without = send_setup(
            &db.pool,
            &with_public_url(),
            TEST_USER,
            &provisioner,
            Utc::now(),
        )
        .await;
        set_email(&db.pool, TEST_USER, "tester@example.com").await;
        let with = send_setup(
            &db.pool,
            &with_public_url(),
            TEST_USER,
            &provisioner,
            Utc::now(),
        )
        .await;

        assert!(
            matches!(without, Err(OkfError::Forbidden { .. })),
            "{without:?}"
        );
        assert!(matches!(with, Err(OkfError::Forbidden { .. })), "{with:?}");
        assert!(queue::list(&db.pool).await.expect("the queue").is_empty());
        assert_eq!(setup_tokens(&db.pool).await, 0);
        assert!(invited(&db.pool).await.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_audit_write_fails_the_invite() {
        let db = test_db().await;
        set_email(&db.pool, TEST_USER, "tester@example.com").await;
        fail_on(&db.pool, "BEFORE INSERT ON audit_entries FOR EACH ROW").await;

        let refused = send_setup(
            &db.pool,
            &with_public_url(),
            TEST_USER,
            &AuditActor::cli(),
            Utc::now(),
        )
        .await;

        assert!(refused.is_err());
        assert!(queue::list(&db.pool).await.expect("the queue").is_empty());
        assert_eq!(setup_tokens(&db.pool).await, 0, "no link without its entry");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn admins_inviting_each_other_at_once_both_finish() {
        use crate::store::users::tests::{both_finish_while_a_person_is_held, two_admins};
        let db = test_db().await;
        let [(low_name, low), (high_name, high)] = two_admins(&db.pool).await;
        set_email(&db.pool, &low_name, "low@example.com").await;
        set_email(&db.pool, &high_name, "high@example.com").await;
        let held = low.user_id.clone().expect("a session actor has a user");
        let pool = db.pool.clone();
        let first = async move {
            send_setup(&pool, &with_public_url(), &high_name, &low, Utc::now())
                .await
                .map(drop)
        };
        let pool = db.pool.clone();
        let second = async move {
            send_setup(&pool, &with_public_url(), &low_name, &high, Utc::now())
                .await
                .map(drop)
        };

        let (one, other) = both_finish_while_a_person_is_held(&db.pool, &held, first, second).await;

        one.expect("the first invite");
        other.expect("the second invite");
        assert_eq!(invited(&db.pool).await.len(), 2);
    }
}

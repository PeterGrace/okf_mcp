//! Single-use emailed links (spec "Emailed links"): setup and reset
//! tokens. A token is 256 random bits in base64url; only its SHA-256 is
//! stored. Issuing one deletes the user's older tokens of the same purpose,
//! and redeeming one marks it used in the statement that checks it.

use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::error::OkfError;
use crate::keys;
use crate::store::users::{user_auth_columns, UserAuth};

/// What a link is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Purpose {
    /// The first password or passkey (`/account/setup`).
    Setup,
    /// A forgotten password (`/account/reset`).
    Reset,
}

impl Purpose {
    /// The `email_tokens.purpose` spelling.
    ///
    /// # Returns
    /// `"setup"` or `"reset"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Purpose::Setup => "setup",
            Purpose::Reset => "reset",
        }
    }
}

/// The answer for every link that does not work, whatever the reason, so
/// the page never says whether a token ever existed.
fn gone() -> OkfError {
    OkfError::Gone {
        reason: "This link has expired. Ask for a new one.".to_string(),
    }
}

/// Issues a link token for a user, deleting its older tokens of the same
/// purpose.
///
/// # Arguments
/// * `pool` - the database.
/// * `user_id` - a `users.id`.
/// * `purpose` - setup or reset.
/// * `ttl` - how long it works (`setup_link_ttl` or `reset_link_ttl`).
/// * `now` - when it is issued (server clock).
///
/// # Returns
/// The raw token, to put in the link. Never stored or logged.
///
/// # Errors
/// [`OkfError::UnknownName`] for an unknown user; [`OkfError::LimitExceeded`]
/// for an absurd `ttl`; [`OkfError::Io`] if the OS random source fails;
/// [`OkfError::Database`].
pub async fn issue(
    pool: &PgPool,
    user_id: &str,
    purpose: Purpose,
    ttl: Duration,
    now: DateTime<Utc>,
) -> Result<String, OkfError> {
    let expires_at = crate::clock::after(now, ttl)?;
    let token = keys::generate_with("")?;
    let mut tx = pool.begin().await?;
    // `FOR NO KEY UPDATE` serializes the issuers of one user's tokens: two
    // concurrent issues cannot both delete the old token and then both
    // insert, which would leave two live links. It also makes a concurrent
    // user delete (`FOR UPDATE`) or email change wait for this insert, so
    // neither fails it with a foreign-key error nor misses the new token.
    let exists: Option<String> =
        sqlx::query_scalar("SELECT id FROM users WHERE id = $1 FOR NO KEY UPDATE")
            .bind(user_id)
            .fetch_optional(&mut *tx)
            .await?;
    if exists.is_none() {
        return Err(OkfError::UnknownName {
            noun: "user",
            name: user_id.to_string(),
        });
    }
    sqlx::query("DELETE FROM email_tokens WHERE user_id = $1 AND purpose = $2")
        .bind(user_id)
        .bind(purpose.as_str())
        .execute(&mut *tx)
        .await?;
    sqlx::query(
        "INSERT INTO email_tokens (hash, user_id, purpose, created_at, expires_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(keys::hash(&token).as_slice())
    .bind(user_id)
    .bind(purpose.as_str())
    .bind(now)
    .bind(expires_at)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(token)
}

/// Uses a link token: marks it used and returns its user, in one statement,
/// so two concurrent uses cannot both succeed (the second waits for the
/// first's row lock, then re-reads `used_at` and matches nothing).
///
/// # Arguments
/// * `pool` - the database.
/// * `token` - the raw token from the link.
/// * `purpose` - what the page expects.
/// * `now` - when (server clock).
///
/// # Returns
/// The token's user.
///
/// # Errors
/// [`OkfError::Gone`] for an unknown, used, expired or wrong-purpose token;
/// [`OkfError::Database`].
pub async fn redeem(
    pool: &PgPool,
    token: &str,
    purpose: Purpose,
    now: DateTime<Utc>,
) -> Result<UserAuth, OkfError> {
    sqlx::query_as::<_, UserAuth>(concat!(
        "WITH used AS (UPDATE email_tokens SET used_at = $3 \
                       WHERE hash = $1 AND purpose = $2 AND used_at IS NULL \
                         AND expires_at > $3 \
                       RETURNING user_id) \
         SELECT ",
        user_auth_columns!(),
        " FROM used JOIN users u ON u.id = used.user_id"
    ))
    .bind(keys::hash(token).as_slice())
    .bind(purpose.as_str())
    .bind(now)
    .fetch_optional(pool)
    .await?
    .ok_or_else(gone)
}

/// Checks a link token without using it (the page shown before the form is
/// submitted).
///
/// # Arguments
/// * `pool` - the database.
/// * `token` - the raw token from the link.
/// * `purpose` - what the page expects.
/// * `now` - when (server clock).
///
/// # Returns
/// The token's user.
///
/// # Errors
/// As [`redeem`].
pub async fn peek(
    pool: &PgPool,
    token: &str,
    purpose: Purpose,
    now: DateTime<Utc>,
) -> Result<UserAuth, OkfError> {
    sqlx::query_as::<_, UserAuth>(concat!(
        "SELECT ",
        user_auth_columns!(),
        " FROM email_tokens t JOIN users u ON u.id = t.user_id \
          WHERE t.hash = $1 AND t.purpose = $2 AND t.used_at IS NULL AND t.expires_at > $3"
    ))
    .bind(keys::hash(token).as_slice())
    .bind(purpose.as_str())
    .bind(now)
    .fetch_optional(pool)
    .await?
    .ok_or_else(gone)
}

/// Deletes tokens that expired before `before`, used or not (`App::poll`'s
/// hourly prune keeps a week for diagnosis).
///
/// # Arguments
/// * `pool` - the database.
/// * `before` - the cutoff.
///
/// # Returns
/// How many were deleted.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn prune(pool: &PgPool, before: DateTime<Utc>) -> Result<u64, OkfError> {
    Ok(
        sqlx::query("DELETE FROM email_tokens WHERE expires_at < $1")
            .bind(before)
            .execute(pool)
            .await?
            .rows_affected(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;
    use crate::store::users::tests::seed_user;
    use chrono::TimeDelta;

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-12T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    const HOUR: Duration = Duration::from_secs(3600);

    #[tokio::test(flavor = "multi_thread")]
    async fn a_link_works_once_and_only_its_hash_is_stored() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", Some("alice@example.com")).await;

        let token = issue(&db.pool, &alice.id, Purpose::Setup, HOUR, now())
            .await
            .unwrap();
        let first = redeem(&db.pool, &token, Purpose::Setup, now()).await;
        let second = redeem(&db.pool, &token, Purpose::Setup, now()).await;

        assert_eq!(token.len(), 43);
        assert_eq!(first.unwrap(), alice);
        assert!(matches!(second, Err(OkfError::Gone { .. })), "{second:?}");
        let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM email_tokens WHERE hash = $1")
            .bind(keys::hash(&token).as_slice())
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(stored, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn expired_unknown_and_wrong_purpose_links_are_gone() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        let token = issue(&db.pool, &alice.id, Purpose::Reset, HOUR, now())
            .await
            .unwrap();

        let expired = redeem(
            &db.pool,
            &token,
            Purpose::Reset,
            now() + TimeDelta::hours(1),
        )
        .await;
        let wrong_purpose = redeem(&db.pool, &token, Purpose::Setup, now()).await;
        let unknown = redeem(&db.pool, "not-a-token", Purpose::Reset, now()).await;

        for result in [expired, wrong_purpose, unknown] {
            assert!(matches!(result, Err(OkfError::Gone { .. })), "{result:?}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_new_link_replaces_the_older_one_of_its_purpose_only() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        let old_setup = issue(&db.pool, &alice.id, Purpose::Setup, HOUR, now())
            .await
            .unwrap();
        let reset = issue(&db.pool, &alice.id, Purpose::Reset, HOUR, now())
            .await
            .unwrap();

        let new_setup = issue(&db.pool, &alice.id, Purpose::Setup, HOUR, now())
            .await
            .unwrap();

        assert!(matches!(
            redeem(&db.pool, &old_setup, Purpose::Setup, now()).await,
            Err(OkfError::Gone { .. })
        ));
        assert_eq!(
            redeem(&db.pool, &new_setup, Purpose::Setup, now())
                .await
                .unwrap(),
            alice
        );
        assert_eq!(
            redeem(&db.pool, &reset, Purpose::Reset, now())
                .await
                .unwrap(),
            alice
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn peek_validates_without_using() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        let token = issue(&db.pool, &alice.id, Purpose::Setup, HOUR, now())
            .await
            .unwrap();

        let peeked = peek(&db.pool, &token, Purpose::Setup, now()).await.unwrap();
        let redeemed = redeem(&db.pool, &token, Purpose::Setup, now())
            .await
            .unwrap();
        let after = peek(&db.pool, &token, Purpose::Setup, now()).await;

        assert_eq!(peeked, alice);
        assert_eq!(redeemed, alice);
        assert!(matches!(after, Err(OkfError::Gone { .. })));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_uses_give_one_user_and_the_rest_gone() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        let token = issue(&db.pool, &alice.id, Purpose::Reset, HOUR, now())
            .await
            .unwrap();

        // Eight redeems on their own tasks, on the multi-threaded runtime.
        let tasks: Vec<_> = (0..8)
            .map(|_| {
                let (pool, token) = (db.pool.clone(), token.clone());
                tokio::spawn(async move { redeem(&pool, &token, Purpose::Reset, now()).await })
            })
            .collect();
        let mut outcomes = Vec::new();
        for task in tasks {
            outcomes.push(task.await.unwrap());
        }

        let won: Vec<_> = outcomes.iter().filter_map(|o| o.as_ref().ok()).collect();
        assert_eq!(won, [&alice], "{outcomes:?}");
        assert!(outcomes
            .iter()
            .filter_map(|o| o.as_ref().err())
            .all(|e| matches!(e, OkfError::Gone { .. })));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_use_waiting_on_another_use_is_gone() {
        // Deterministic form of the race: a transaction has used the token
        // and holds its commit while a second use waits on the row.
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        let token = issue(&db.pool, &alice.id, Purpose::Setup, HOUR, now())
            .await
            .unwrap();
        let mut using = db.pool.begin().await.unwrap();
        sqlx::query("UPDATE email_tokens SET used_at = $2 WHERE hash = $1")
            .bind(keys::hash(&token).as_slice())
            .bind(now())
            .execute(&mut *using)
            .await
            .unwrap();
        let (pool, raw) = (db.pool.clone(), token.clone());
        let waiting = tokio::spawn(async move { redeem(&pool, &raw, Purpose::Setup, now()).await });
        crate::store::catalogs::tests::until_a_lock_is_awaited(&db.pool).await;

        using.commit().await.unwrap();
        let outcome = waiting.await.unwrap();

        assert!(matches!(outcome, Err(OkfError::Gone { .. })), "{outcome:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_issues_leave_exactly_one_live_link() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;

        let tasks: Vec<_> = (0..6)
            .map(|_| {
                let (pool, id) = (db.pool.clone(), alice.id.clone());
                tokio::spawn(async move { issue(&pool, &id, Purpose::Reset, HOUR, now()).await })
            })
            .collect();
        let mut tokens = Vec::new();
        for task in tasks {
            tokens.push(task.await.unwrap().unwrap());
        }

        let mut live = 0;
        for token in &tokens {
            if peek(&db.pool, token, Purpose::Reset, now()).await.is_ok() {
                live += 1;
            }
        }
        assert_eq!(live, 1);
        let stored: i64 = sqlx::query_scalar("SELECT count(*) FROM email_tokens")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(stored, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn changing_the_users_email_drops_its_links_but_setting_the_same_one_does_not() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", Some("alice@example.com")).await;
        let token = issue(&db.pool, &alice.id, Purpose::Setup, HOUR, now())
            .await
            .unwrap();

        crate::store::users::set_email(&db.pool, "alice", Some("ALICE@example.com"))
            .await
            .unwrap();
        let same = peek(&db.pool, &token, Purpose::Setup, now()).await;
        crate::store::users::set_email(&db.pool, "alice", Some("alice@example.org"))
            .await
            .unwrap();
        let changed = peek(&db.pool, &token, Purpose::Setup, now()).await;

        assert!(same.is_ok(), "{same:?}");
        assert!(matches!(changed, Err(OkfError::Gone { .. })), "{changed:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn issuing_for_an_unknown_user_is_unknown_name_and_prune_drops_old_tokens() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        issue(&db.pool, &alice.id, Purpose::Setup, HOUR, now())
            .await
            .unwrap();

        let unknown = issue(&db.pool, "no-such-id", Purpose::Setup, HOUR, now()).await;
        let kept = prune(&db.pool, now()).await.unwrap();
        let pruned = prune(&db.pool, now() + TimeDelta::hours(2)).await.unwrap();

        assert!(matches!(
            unknown,
            Err(OkfError::UnknownName { noun: "user", .. })
        ));
        assert_eq!((kept, pruned), (0, 1));
    }
}

//! The sign-in audit (spec "Logging and audit"): one row per attempt,
//! shown to the user and to admins, pruned after 90 days. Recording also
//! writes the `info` log line, so the two never disagree. Secrets are never
//! part of an event.

use std::net::IpAddr;

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::error::OkfError;

/// How someone tried to sign in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Method {
    /// Email and password.
    Password,
    /// A passkey (WebAuthn).
    Passkey,
    /// A linked OIDC identity.
    Oidc,
    /// A setup link (the first sign-in).
    Setup,
}

/// How the attempt ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Signed in.
    Ok,
    /// Refused.
    Failed,
    /// Refused because the account is locked.
    Locked,
}

impl Method {
    /// The `sign_in_events.method` spelling.
    ///
    /// # Returns
    /// `"password"`, `"passkey"`, `"oidc"` or `"setup"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Method::Password => "password",
            Method::Passkey => "passkey",
            Method::Oidc => "oidc",
            Method::Setup => "setup",
        }
    }

    /// Reads a stored spelling.
    ///
    /// # Arguments
    /// * `text` - a `sign_in_events.method` value.
    ///
    /// # Returns
    /// The method, or `None` for an unknown spelling.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "password" => Some(Method::Password),
            "passkey" => Some(Method::Passkey),
            "oidc" => Some(Method::Oidc),
            "setup" => Some(Method::Setup),
            _ => None,
        }
    }
}

impl Outcome {
    /// The `sign_in_events.outcome` spelling.
    ///
    /// # Returns
    /// `"ok"`, `"failed"` or `"locked"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Outcome::Ok => "ok",
            Outcome::Failed => "failed",
            Outcome::Locked => "locked",
        }
    }

    /// Reads a stored spelling.
    ///
    /// # Arguments
    /// * `text` - a `sign_in_events.outcome` value.
    ///
    /// # Returns
    /// The outcome, or `None` for an unknown spelling.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "ok" => Some(Outcome::Ok),
            "failed" => Some(Outcome::Failed),
            "locked" => Some(Outcome::Locked),
            _ => None,
        }
    }
}

/// One recorded attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInEvent {
    /// How.
    pub method: Method,
    /// With what result.
    pub outcome: Outcome,
    /// From where, when known.
    pub ip: Option<IpAddr>,
    /// When (server clock).
    pub at: DateTime<Utc>,
}

/// Records an attempt and logs it at `info`.
///
/// # Arguments
/// * `pool` - the database.
/// * `user_id` - the user, when the attempt named a known one (`None` for
///   an unknown email: the row is kept for counting, shown to nobody).
/// * `method` - how.
/// * `outcome` - with what result.
/// * `ip` - the client address, when known.
/// * `now` - when (server clock).
///
/// # Errors
/// [`OkfError::Database`].
pub async fn record(
    pool: &PgPool,
    user_id: Option<&str>,
    method: Method,
    outcome: Outcome,
    ip: Option<IpAddr>,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    tracing::info!(
        user_id = user_id.unwrap_or("-"),
        method = method.as_str(),
        outcome = outcome.as_str(),
        ip = ip
            .map(|address| address.to_string())
            .as_deref()
            .unwrap_or("-"),
        "sign-in attempt"
    );
    // The address goes in as text and is cast to `inet` by Postgres, so no
    // sqlx network-type feature is needed.
    sqlx::query(
        "INSERT INTO sign_in_events (user_id, method, outcome, ip, at) \
         VALUES ($1, $2, $3, $4::inet, $5)",
    )
    .bind(user_id)
    .bind(method.as_str())
    .bind(outcome.as_str())
    .bind(ip.map(|address| address.to_string()))
    .bind(now)
    .execute(pool)
    .await?;
    Ok(())
}

/// A user's most recent attempts, newest first.
///
/// # Arguments
/// * `pool` - the database.
/// * `user` - the user's name.
/// * `limit` - how many at most.
///
/// # Returns
/// The events.
///
/// # Errors
/// [`OkfError::UnknownName`]; [`OkfError::Database`].
pub async fn recent(pool: &PgPool, user: &str, limit: i64) -> Result<Vec<SignInEvent>, OkfError> {
    let user_id: String = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(user)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| OkfError::UnknownName {
            noun: "user",
            name: user.to_string(),
        })?;
    // `host(ip)` is the address as text without a prefix length.
    let rows: Vec<(String, String, Option<String>, DateTime<Utc>)> = sqlx::query_as(
        "SELECT method, outcome, host(ip), at FROM sign_in_events \
         WHERE user_id = $1 ORDER BY at DESC, id DESC LIMIT $2",
    )
    .bind(&user_id)
    .bind(limit)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(method, outcome, ip, at)| SignInEvent {
            // The table's CHECK constraints admit only these spellings.
            method: Method::parse(&method).expect("sign_in_events.method is checked"),
            outcome: Outcome::parse(&outcome).expect("sign_in_events.outcome is checked"),
            ip: ip.and_then(|text| text.parse().ok()),
            at,
        })
        .collect())
}

/// Deletes events from before `before` (the hourly prune passes 90 days ago).
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
    Ok(sqlx::query("DELETE FROM sign_in_events WHERE at < $1")
        .bind(before)
        .execute(pool)
        .await?
        .rows_affected())
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

    #[test]
    fn spellings_round_trip() {
        for method in [
            Method::Password,
            Method::Passkey,
            Method::Oidc,
            Method::Setup,
        ] {
            assert_eq!(Method::parse(method.as_str()), Some(method));
        }
        for outcome in [Outcome::Ok, Outcome::Failed, Outcome::Locked] {
            assert_eq!(Outcome::parse(outcome.as_str()), Some(outcome));
        }
        assert_eq!(Method::parse("magic"), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn recent_is_newest_first_limited_and_keeps_the_address() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        let ip: IpAddr = "192.0.2.7".parse().unwrap();
        record(
            &db.pool,
            Some(&alice.id),
            Method::Password,
            Outcome::Failed,
            Some(ip),
            now(),
        )
        .await
        .unwrap();
        let later = now() + TimeDelta::minutes(1);
        record(
            &db.pool,
            Some(&alice.id),
            Method::Passkey,
            Outcome::Ok,
            None,
            later,
        )
        .await
        .unwrap();
        record(
            &db.pool,
            None,
            Method::Password,
            Outcome::Failed,
            Some(ip),
            later,
        )
        .await
        .unwrap();

        let all = recent(&db.pool, "alice", 10).await.unwrap();
        let one = recent(&db.pool, "alice", 1).await.unwrap();

        assert_eq!(
            all,
            [
                SignInEvent {
                    method: Method::Passkey,
                    outcome: Outcome::Ok,
                    ip: None,
                    at: later
                },
                SignInEvent {
                    method: Method::Password,
                    outcome: Outcome::Failed,
                    ip: Some(ip),
                    at: now()
                },
            ]
        );
        assert_eq!(one.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_user_is_unknown_name_and_prune_drops_old_events() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        record(
            &db.pool,
            Some(&alice.id),
            Method::Setup,
            Outcome::Ok,
            None,
            now(),
        )
        .await
        .unwrap();

        let unknown = recent(&db.pool, "nobody", 10).await;
        let kept = prune(&db.pool, now()).await.unwrap();
        let pruned = prune(&db.pool, now() + TimeDelta::seconds(1))
            .await
            .unwrap();

        assert!(matches!(
            unknown,
            Err(OkfError::UnknownName { noun: "user", .. })
        ));
        assert_eq!((kept, pruned), (0, 1));
    }
}

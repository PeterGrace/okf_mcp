//! Console sessions (spec "Sessions"; named apart from MCP sessions,
//! Decision 9). The `okf_session` cookie carries a 256-bit random id; only
//! its SHA-256 (`crate::keys::hash`, as for API keys) is stored, as
//! `user_sessions.id_hash`.
//!
//! The CSRF token is derived from the id (a SHA-256 of the id under a
//! label of its own), so the database holds no secret at all: a copy of
//! `user_sessions` can neither sign anyone in nor forge a write.
//! `csrf_token` keeps only the hash of the token.
//!
//! A session ends when its absolute limit passes, when it sits idle too
//! long, or when it is revoked. [`touch`] never brings an ended session
//! back: its writes re-check both limits under the row lock.

use std::net::IpAddr;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{DateTime, TimeDelta, Utc};
use sqlx::{PgExecutor, PgPool};

use crate::error::OkfError;
use crate::keys;

/// How stale `last_seen_at` may be before a request rewrites it, so an
/// active console writes at most once a minute per session.
const LAST_SEEN_RESOLUTION: TimeDelta = TimeDelta::minutes(1);

/// The length of [`SessionInfo::id_prefix`], in hex characters (6 bytes).
pub const ID_PREFIX_LENGTH: usize = 12;

/// The most of a `User-Agent` header a session keeps, in characters: a
/// browser's fits, and a client cannot fill the table with a huge one.
const USER_AGENT_LIMIT: usize = 256;

/// Separates the CSRF token's derivation from every other hash of the id.
const CSRF_LABEL: &str = "okf-mcp console session csrf\0";

/// A new session's secrets: the cookie value and the CSRF token. Its
/// `Debug` shows neither.
#[derive(Clone, PartialEq, Eq)]
pub struct NewSession {
    /// The raw session id (the cookie's value).
    pub id: String,
    /// The raw CSRF token (sent back in `X-OKF-CSRF`).
    pub csrf: String,
}

// A hand-written `Debug` (Python's `__repr__`) instead of `#[derive]`, so
// `{:?}` in a log line can never print a secret.
impl std::fmt::Debug for NewSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NewSession")
            .field("id", &"<redacted>")
            .field("csrf", &"<redacted>")
            .finish()
    }
}

/// A live session, as a request sees it. Its `Debug` hides the CSRF token.
#[derive(Clone, PartialEq, Eq)]
pub struct SessionRow {
    /// Whose session.
    pub user_id: String,
    /// The CSRF token a cookie-authenticated write must carry.
    pub csrf: String,
    /// When it started.
    pub created_at: DateTime<Utc>,
    /// When it ends at the latest.
    pub expires_at: DateTime<Utc>,
}

impl std::fmt::Debug for SessionRow {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionRow")
            .field("user_id", &self.user_id)
            .field("csrf", &"<redacted>")
            .field("created_at", &self.created_at)
            .field("expires_at", &self.expires_at)
            .finish()
    }
}

/// One session as the account page and `user sessions` list it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionInfo {
    /// The first [`ID_PREFIX_LENGTH`] hex characters of the stored hash:
    /// tells sessions apart, signs nobody in.
    pub id_prefix: String,
    /// When it started.
    pub created_at: DateTime<Utc>,
    /// When it was last used (to the minute).
    pub last_seen_at: DateTime<Utc>,
    /// The address it started from, when known.
    pub ip: Option<IpAddr>,
    /// The browser it started in (`User-Agent`, possibly empty).
    pub user_agent: String,
    /// Whether this is the session asking.
    pub current: bool,
}

/// The CSRF token of the session whose raw id is `id`: 256 bits, 43
/// base64url characters, computable only from the id.
fn csrf_for(id: &str) -> String {
    URL_SAFE_NO_PAD.encode(keys::hash(&format!("{CSRF_LABEL}{id}")))
}

/// The instant before which a session's `last_seen_at` means it has been
/// idle too long, or `None` when `idle` reaches past every stored time (a
/// configured idle beyond 1970 never times out; no session predates it).
fn idle_cutoff(idle: Duration, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    TimeDelta::from_std(idle)
        .ok()
        .and_then(|idle| now.checked_sub_signed(idle))
        .filter(|cutoff| *cutoff > DateTime::UNIX_EPOCH)
}

/// Starts a session.
///
/// # Arguments
/// * `pool` - the database.
/// * `user_id` - whose session.
/// * `ip` - the client address, when known.
/// * `user_agent` - the `User-Agent` header (empty when absent); only its
///   first 256 characters are kept.
/// * `absolute` - `console_session_absolute`.
/// * `now` - when (server clock).
///
/// # Returns
/// The raw id and CSRF token. Neither is stored.
///
/// # Errors
/// [`OkfError::LimitExceeded`] for an absurd `absolute`; [`OkfError::Io`]
/// if the OS random source fails; [`OkfError::Database`] (a foreign-key
/// failure for an unknown user).
pub async fn create(
    pool: &PgPool,
    user_id: &str,
    ip: Option<IpAddr>,
    user_agent: &str,
    absolute: Duration,
    now: DateTime<Utc>,
) -> Result<NewSession, OkfError> {
    let expires_at = crate::clock::after(now, absolute)?;
    let id = keys::generate_with("")?;
    let session = NewSession {
        csrf: csrf_for(&id),
        id,
    };
    // `char_indices().nth(n)` finds the byte offset of the n-th character,
    // so the cut never splits a multi-byte character (slicing a `&str`
    // mid-character panics).
    let user_agent = user_agent
        .char_indices()
        .nth(USER_AGENT_LIMIT)
        .map_or(user_agent, |(end, _)| &user_agent[..end]);
    sqlx::query(
        "INSERT INTO user_sessions \
             (id_hash, user_id, csrf_token, created_at, last_seen_at, expires_at, ip, user_agent) \
         VALUES ($1, $2, encode($3, 'hex'), $4, $4, $5, $6::inet, $7)",
    )
    .bind(keys::hash(&session.id).as_slice())
    .bind(user_id)
    .bind(keys::hash(&session.csrf).as_slice())
    .bind(now)
    .bind(expires_at)
    .bind(ip.map(|address| address.to_string()))
    .bind(user_agent)
    .execute(pool)
    .await?;
    Ok(session)
}

/// A stored session, as [`touch`] reads it.
#[derive(sqlx::FromRow)]
struct Stored {
    user_id: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
}

/// Looks a session up for a request: ends it if it is past its absolute
/// limit or idle for `idle`, and otherwise records the activity (at most
/// once a minute).
///
/// Safe under concurrency: the activity write re-checks both limits under
/// the row lock (and never moves `last_seen_at` backwards), so a request
/// racing the one that ends a session, or a revocation, cannot extend it;
/// it gets `None` instead.
///
/// # Arguments
/// * `pool` - the database.
/// * `id` - the raw id from the cookie.
/// * `idle` - `console_session_idle`.
/// * `now` - the request's time (server clock).
///
/// # Returns
/// The session, or `None` when there is none or it just ended.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn touch(
    pool: &PgPool,
    id: &str,
    idle: Duration,
    now: DateTime<Utc>,
) -> Result<Option<SessionRow>, OkfError> {
    let hash = keys::hash(id);
    let cutoff = idle_cutoff(idle, now);
    let Some(stored) = sqlx::query_as::<_, Stored>(
        "SELECT user_id, created_at, expires_at, last_seen_at \
         FROM user_sessions WHERE id_hash = $1",
    )
    .bind(hash.as_slice())
    .fetch_optional(pool)
    .await?
    else {
        return Ok(None);
    };
    let ended =
        now >= stored.expires_at || cutoff.is_some_and(|cutoff| stored.last_seen_at <= cutoff);
    if ended {
        // The same conditions again, so a session another request has
        // just kept alive (by this request's clock) is not deleted.
        sqlx::query(
            "DELETE FROM user_sessions WHERE id_hash = $1 \
             AND (expires_at <= $2 OR last_seen_at <= $3::timestamptz)",
        )
        .bind(hash.as_slice())
        .bind(now)
        .bind(cutoff)
        .execute(pool)
        .await?;
        return Ok(None);
    }
    if now - stored.last_seen_at >= LAST_SEEN_RESOLUTION {
        // Postgres re-evaluates the WHERE on the latest row version once it
        // holds the row lock, so a session deleted or ended meanwhile
        // matches nothing. (`x > NULL` is NULL, so `$3 IS NULL` is spelled
        // out for "no idle limit".)
        let still_live = sqlx::query(
            "UPDATE user_sessions SET last_seen_at = GREATEST(last_seen_at, $2) \
             WHERE id_hash = $1 AND expires_at > $2 \
               AND ($3::timestamptz IS NULL OR last_seen_at > $3)",
        )
        .bind(hash.as_slice())
        .bind(now)
        .bind(cutoff)
        .execute(pool)
        .await?
        .rows_affected()
            > 0;
        if !still_live {
            return Ok(None);
        }
    }
    Ok(Some(SessionRow {
        user_id: stored.user_id,
        csrf: csrf_for(id),
        created_at: stored.created_at,
        expires_at: stored.expires_at,
    }))
}

/// Ends one session (sign-out).
///
/// # Arguments
/// * `pool` - the database.
/// * `id` - the raw id from the cookie.
///
/// # Returns
/// `Ok(())`, whether or not the session existed.
///
/// # Errors
/// [`OkfError::Database`]. An unknown id is not an error.
pub async fn revoke(pool: &PgPool, id: &str) -> Result<(), OkfError> {
    sqlx::query("DELETE FROM user_sessions WHERE id_hash = $1")
        .bind(keys::hash(id).as_slice())
        .execute(pool)
        .await?;
    Ok(())
}

/// Ends every session of a user, except perhaps the one asking. The one
/// place that deletes a user's sessions in bulk; `users::set_admin` calls
/// it inside its own transaction.
///
/// # Arguments
/// * `executor` - the database: a `&PgPool`, or a `&mut PgConnection`
///   (such as `&mut *tx`) to run inside a caller's transaction.
/// * `user_id` - whose sessions.
/// * `except` - the raw id of a session to keep (the caller's own).
///
/// # Returns
/// How many ended.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn revoke_all(
    executor: impl PgExecutor<'_>,
    user_id: &str,
    except: Option<&str>,
) -> Result<u64, OkfError> {
    // `[u8; 32]` lives in `kept`, so the slice bound below borrows it.
    let kept = except.map(keys::hash);
    Ok(sqlx::query(
        "DELETE FROM user_sessions WHERE user_id = $1 AND ($2::bytea IS NULL OR id_hash <> $2)",
    )
    .bind(user_id)
    .bind(kept.as_ref().map(|hash| hash.as_slice()))
    .execute(executor)
    .await?
    .rows_affected())
}

/// One row of [`list`] as read.
#[derive(sqlx::FromRow)]
struct Listed {
    id_prefix: String,
    created_at: DateTime<Utc>,
    last_seen_at: DateTime<Utc>,
    ip: Option<String>,
    user_agent: String,
    current: bool,
}

/// A user's sessions, most recently used first. Sessions past their limits
/// are included until [`touch`] or [`prune`] removes them.
///
/// # Arguments
/// * `pool` - the database.
/// * `user_id` - whose sessions.
/// * `current` - the raw id of the session asking, to mark it.
///
/// # Returns
/// The sessions.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn list(
    pool: &PgPool,
    user_id: &str,
    current: Option<&str>,
) -> Result<Vec<SessionInfo>, OkfError> {
    let current = current.map(keys::hash);
    let rows = sqlx::query_as::<_, Listed>(
        "SELECT encode(substring(id_hash FROM 1 FOR 6), 'hex') AS id_prefix, \
                created_at, last_seen_at, host(ip) AS ip, user_agent, \
                COALESCE(id_hash = $2::bytea, false) AS current \
         FROM user_sessions WHERE user_id = $1 \
         ORDER BY last_seen_at DESC, created_at DESC",
    )
    .bind(user_id)
    .bind(current.as_ref().map(|hash| hash.as_slice()))
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| SessionInfo {
            id_prefix: row.id_prefix,
            created_at: row.created_at,
            last_seen_at: row.last_seen_at,
            ip: row.ip.and_then(|text| text.parse().ok()),
            user_agent: row.user_agent,
            current: row.current,
        })
        .collect())
}

/// Ends one of a user's sessions by its [`SessionInfo::id_prefix`].
///
/// # Arguments
/// * `pool` - the database.
/// * `user_id` - whose session (another user's prefix matches nothing).
/// * `id_prefix` - 12 lowercase hex characters.
///
/// # Returns
/// `Ok(())` once the session has ended.
///
/// # Errors
/// [`OkfError::UnknownName`] with `noun: "session"` for a malformed prefix
/// or one that matches none of the user's sessions; [`OkfError::Database`].
pub async fn revoke_by_prefix(
    pool: &PgPool,
    user_id: &str,
    id_prefix: &str,
) -> Result<(), OkfError> {
    let unknown = || OkfError::UnknownName {
        noun: "session",
        name: id_prefix.to_string(),
    };
    let well_formed = id_prefix.len() == ID_PREFIX_LENGTH
        && id_prefix
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte));
    if !well_formed {
        return Err(unknown());
    }
    let deleted = sqlx::query(
        "DELETE FROM user_sessions \
         WHERE user_id = $1 AND encode(substring(id_hash FROM 1 FOR 6), 'hex') = $2",
    )
    .bind(user_id)
    .bind(id_prefix)
    .execute(pool)
    .await?
    .rows_affected();
    if deleted == 0 {
        return Err(unknown());
    }
    Ok(())
}

/// Deletes sessions past their absolute limit (idle ones go at their next
/// [`touch`], or here once their absolute limit passes).
///
/// # Arguments
/// * `pool` - the database.
/// * `now` - the server clock.
///
/// # Returns
/// How many were deleted.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn prune(pool: &PgPool, now: DateTime<Utc>) -> Result<u64, OkfError> {
    Ok(
        sqlx::query("DELETE FROM user_sessions WHERE expires_at <= $1")
            .bind(now)
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

    fn now() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-11-12T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    const IDLE: Duration = Duration::from_secs(12 * 60 * 60);
    const ABSOLUTE: Duration = Duration::from_secs(7 * 24 * 60 * 60);

    async fn last_seen(pool: &PgPool, id: &str) -> DateTime<Utc> {
        sqlx::query_scalar("SELECT last_seen_at FROM user_sessions WHERE id_hash = $1")
            .bind(keys::hash(id).as_slice())
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_new_session_is_found_by_its_id_and_only_its_hash_is_stored() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        let ip: IpAddr = "192.0.2.7".parse().unwrap();

        let session = create(&db.pool, &alice.id, Some(ip), "test-agent", ABSOLUTE, now())
            .await
            .unwrap();
        let row = touch(&db.pool, &session.id, IDLE, now())
            .await
            .unwrap()
            .unwrap();

        assert_eq!((session.id.len(), session.csrf.len()), (43, 43));
        assert_ne!(session.id, session.csrf);
        assert_eq!(row.user_id, alice.id);
        assert_eq!(row.csrf, session.csrf);
        assert_eq!(row.expires_at, now() + TimeDelta::days(7));
        let raw: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM user_sessions \
             WHERE id_hash = $1 OR csrf_token = $2 OR csrf_token = $3 \
                OR position($2 IN csrf_token) > 0 OR position($3 IN csrf_token) > 0",
        )
        .bind(session.id.as_bytes())
        .bind(&session.id)
        .bind(&session.csrf)
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert_eq!(raw, 0, "neither raw secret is stored");
        let debug = format!("{session:?} {row:?}");
        assert!(
            !debug.contains(&session.csrf) && !debug.contains(&session.id),
            "{debug}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_long_user_agent_is_cut_at_a_character_boundary() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        // A multibyte character, so a byte-count cut would split one.
        let agent = "é".repeat(300);

        create(&db.pool, &alice.id, None, &agent, ABSOLUTE, now())
            .await
            .unwrap();

        let listed = list(&db.pool, &alice.id, None).await.unwrap();
        assert_eq!(listed[0].user_agent, "é".repeat(256));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn last_seen_is_written_at_most_once_a_minute() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        let session = create(&db.pool, &alice.id, None, "", ABSOLUTE, now())
            .await
            .unwrap();

        touch(&db.pool, &session.id, IDLE, now() + TimeDelta::seconds(30))
            .await
            .unwrap();
        let after_30s = last_seen(&db.pool, &session.id).await;
        touch(&db.pool, &session.id, IDLE, now() + TimeDelta::seconds(61))
            .await
            .unwrap();
        let after_61s = last_seen(&db.pool, &session.id).await;

        assert_eq!(after_30s, now());
        assert_eq!(after_61s, now() + TimeDelta::seconds(61));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_idle_session_ends_and_is_deleted() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        let session = create(&db.pool, &alice.id, None, "", ABSOLUTE, now())
            .await
            .unwrap();

        let idle = touch(&db.pool, &session.id, IDLE, now() + TimeDelta::hours(12))
            .await
            .unwrap();
        let again = touch(&db.pool, &session.id, IDLE, now()).await.unwrap();

        assert!(idle.is_none());
        assert!(again.is_none(), "the row went with it");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_active_session_still_ends_at_its_absolute_limit() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        let session = create(&db.pool, &alice.id, None, "", ABSOLUTE, now())
            .await
            .unwrap();

        // Used every 11 hours, so never idle for 12.
        let mut at = now();
        let mut alive = 0;
        while touch(&db.pool, &session.id, IDLE, at)
            .await
            .unwrap()
            .is_some()
        {
            alive += 1;
            at += TimeDelta::hours(11);
        }

        assert_eq!(
            alive, 16,
            "7 days = 15 full 11-hour steps after the first touch"
        );
        assert!(at >= now() + TimeDelta::days(7));
        assert!(list(&db.pool, &alice.id, None).await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_late_touch_never_revives_an_ended_session() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        let session = create(&db.pool, &alice.id, None, "", ABSOLUTE, now())
            .await
            .unwrap();
        // Another replica, its clock 13 hours on, ends the session first...
        let ended = touch(&db.pool, &session.id, IDLE, now() + TimeDelta::hours(13))
            .await
            .unwrap();
        // ...then a request stamped earlier (still inside the idle window
        // by its own clock) arrives.
        let late = touch(&db.pool, &session.id, IDLE, now() + TimeDelta::hours(11))
            .await
            .unwrap();

        assert!(ended.is_none());
        assert!(late.is_none());
        assert!(list(&db.pool, &alice.id, None).await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_revoked_session_is_not_revived_by_a_touch_that_read_it_first() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        let session = create(&db.pool, &alice.id, None, "", ABSOLUTE, now())
            .await
            .unwrap();
        // Hold the row lock, revoke inside it, and let a touch that needs
        // to write `last_seen_at` queue behind the lock.
        let mut tx = db.pool.begin().await.unwrap();
        sqlx::query("SELECT 1 FROM user_sessions WHERE id_hash = $1 FOR UPDATE")
            .bind(keys::hash(&session.id).as_slice())
            .execute(&mut *tx)
            .await
            .unwrap();
        let pool = db.pool.clone();
        let id = session.id.clone();
        let racing = tokio::spawn(async move {
            touch(&pool, &id, IDLE, now() + TimeDelta::hours(1))
                .await
                .unwrap()
        });
        crate::store::catalogs::tests::until_a_lock_is_awaited(&db.pool).await;
        sqlx::query("DELETE FROM user_sessions WHERE id_hash = $1")
            .bind(keys::hash(&session.id).as_slice())
            .execute(&mut *tx)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        assert!(racing.await.unwrap().is_none());
        assert!(list(&db.pool, &alice.id, None).await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoke_and_revoke_all_with_an_exception() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        let bob = seed_user(&db.pool, "bob", None).await;
        let first = create(&db.pool, &alice.id, None, "", ABSOLUTE, now())
            .await
            .unwrap();
        let second = create(&db.pool, &alice.id, None, "", ABSOLUTE, now())
            .await
            .unwrap();
        let third = create(&db.pool, &alice.id, None, "", ABSOLUTE, now())
            .await
            .unwrap();
        let bobs = create(&db.pool, &bob.id, None, "", ABSOLUTE, now())
            .await
            .unwrap();

        revoke(&db.pool, &first.id).await.unwrap();
        let others = revoke_all(&db.pool, &alice.id, Some(&second.id))
            .await
            .unwrap();

        assert_eq!(others, 1);
        assert!(touch(&db.pool, &first.id, IDLE, now())
            .await
            .unwrap()
            .is_none());
        assert!(touch(&db.pool, &second.id, IDLE, now())
            .await
            .unwrap()
            .is_some());
        assert!(touch(&db.pool, &third.id, IDLE, now())
            .await
            .unwrap()
            .is_none());
        assert!(touch(&db.pool, &bobs.id, IDLE, now())
            .await
            .unwrap()
            .is_some());
        assert_eq!(revoke_all(&db.pool, &alice.id, None).await.unwrap(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_shows_prefixes_marks_the_current_one_and_revokes_by_prefix() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        let ip: IpAddr = "2001:db8::1".parse().unwrap();
        let older = create(&db.pool, &alice.id, Some(ip), "phone", ABSOLUTE, now())
            .await
            .unwrap();
        let later = now() + TimeDelta::minutes(5);
        let current = create(&db.pool, &alice.id, None, "laptop", ABSOLUTE, later)
            .await
            .unwrap();

        let sessions = list(&db.pool, &alice.id, Some(&current.id)).await.unwrap();

        assert_eq!(sessions.len(), 2);
        assert_eq!(
            (
                sessions[0].user_agent.as_str(),
                sessions[0].current,
                sessions[0].ip
            ),
            ("laptop", true, None)
        );
        assert_eq!(
            (
                sessions[1].user_agent.as_str(),
                sessions[1].current,
                sessions[1].ip
            ),
            ("phone", false, Some(ip))
        );
        let expected_prefix: String = keys::hash(&older.id)[..6]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(sessions[1].id_prefix, expected_prefix);

        revoke_by_prefix(&db.pool, &alice.id, &sessions[1].id_prefix)
            .await
            .unwrap();
        let unknown = revoke_by_prefix(&db.pool, &alice.id, &sessions[1].id_prefix).await;
        let malformed = revoke_by_prefix(&db.pool, &alice.id, "xyz").await;

        assert!(touch(&db.pool, &older.id, IDLE, later)
            .await
            .unwrap()
            .is_none());
        assert!(matches!(
            unknown,
            Err(OkfError::UnknownName {
                noun: "session",
                ..
            })
        ));
        assert!(matches!(
            malformed,
            Err(OkfError::UnknownName {
                noun: "session",
                ..
            })
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn another_users_prefix_is_unknown() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        let bob = seed_user(&db.pool, "bob", None).await;
        create(&db.pool, &bob.id, None, "", ABSOLUTE, now())
            .await
            .unwrap();
        let bobs_prefix = list(&db.pool, &bob.id, None).await.unwrap()[0]
            .id_prefix
            .clone();

        let refused = revoke_by_prefix(&db.pool, &alice.id, &bobs_prefix).await;

        assert!(matches!(refused, Err(OkfError::UnknownName { .. })));
        assert_eq!(list(&db.pool, &bob.id, None).await.unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prune_deletes_sessions_past_their_absolute_limit() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        create(
            &db.pool,
            &alice.id,
            None,
            "",
            Duration::from_secs(60),
            now(),
        )
        .await
        .unwrap();
        create(&db.pool, &alice.id, None, "", ABSOLUTE, now())
            .await
            .unwrap();

        let pruned = prune(&db.pool, now() + TimeDelta::minutes(2))
            .await
            .unwrap();

        assert_eq!(pruned, 1);
        assert_eq!(list(&db.pool, &alice.id, None).await.unwrap().len(), 1);
    }
}

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
//!
//! Activity is recorded at most once per write resolution: a quarter of
//! the idle limit, and never more than a minute. A use inside the window is
//! not written, so the idle limit is enforced with one resolution of
//! grace: a session ends once `last_seen_at + idle + resolution` has
//! passed. It is never ended sooner than `idle` after its last use, and
//! always by `idle + resolution` after it.

use std::net::IpAddr;
use std::time::Duration;

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{DateTime, TimeDelta, Utc};
use sqlx::{PgExecutor, PgPool};

use crate::error::OkfError;
use crate::keys;
use crate::store::passwords::PasswordStamp;

/// The most stale `last_seen_at` may be before a request rewrites it, so
/// an active console writes at most once a minute per session.
const MAX_LAST_SEEN_RESOLUTION: TimeDelta = TimeDelta::minutes(1);

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

/// The instant at or before which a session's `last_seen_at` means it has
/// been idle too long: `now - (idle + resolution)`. The grace of one
/// [`write_resolution`] covers a use that came too soon after the last
/// write to be recorded (it was at most one resolution after
/// `last_seen_at`), so no session ends sooner than `idle` after its real
/// last use. `None` when the cutoff reaches past every stored time (a
/// configured idle beyond 1970 never times out; no session predates it).
fn idle_cutoff(idle: Duration, now: DateTime<Utc>) -> Option<DateTime<Utc>> {
    TimeDelta::from_std(idle)
        .ok()
        .and_then(|idle| idle.checked_add(&write_resolution_of(idle)))
        .and_then(|limit| now.checked_sub_signed(limit))
        .filter(|cutoff| *cutoff > DateTime::UNIX_EPOCH)
}

/// How stale `last_seen_at` may be before a request rewrites it: a
/// quarter of `idle`, at most [`MAX_LAST_SEEN_RESOLUTION`]. A session used
/// more often than every `idle` therefore always has a recent enough
/// `last_seen_at` to stay alive, however short `idle` is.
fn write_resolution(idle: Duration) -> TimeDelta {
    // `idle / 4` past chrono's range is far above the cap, so the cap wins.
    TimeDelta::from_std(idle).map_or(MAX_LAST_SEEN_RESOLUTION, write_resolution_of)
}

/// [`write_resolution`] for an idle limit already in chrono's range.
fn write_resolution_of(idle: TimeDelta) -> TimeDelta {
    (idle / 4).min(MAX_LAST_SEEN_RESOLUTION)
}

/// What a new session needs.
pub struct SessionStart<'a> {
    /// Whose session.
    pub user_id: &'a str,
    /// The client address, when known.
    pub ip: Option<IpAddr>,
    /// The `User-Agent` header (empty when absent); only its first 256
    /// characters are kept.
    pub user_agent: &'a str,
    /// `console_session_absolute`.
    pub absolute: Duration,
    /// When (server clock).
    pub now: DateTime<Utc>,
    /// For a password sign-in, the hash the password matched: the session
    /// is refused if the user's hash has changed since. `None` for the other
    /// sign-in methods.
    pub password: Option<&'a PasswordStamp>,
}

/// Starts a session with no password condition ([`start`] with
/// `password: None`).
///
/// # Arguments
/// * `pool` - the database.
/// * `user_id` - whose session.
/// * `ip` - the client address, when known.
/// * `user_agent` - the `User-Agent` header (empty when absent).
/// * `absolute` - `console_session_absolute`.
/// * `now` - when (server clock).
///
/// # Returns
/// The raw id and CSRF token. Neither is stored.
///
/// # Errors
/// As [`start`].
pub async fn create(
    pool: &PgPool,
    user_id: &str,
    ip: Option<IpAddr>,
    user_agent: &str,
    absolute: Duration,
    now: DateTime<Utc>,
) -> Result<NewSession, OkfError> {
    let request = SessionStart {
        user_id,
        ip,
        user_agent,
        absolute,
        now,
        password: None,
    };
    start(pool, &request).await
}

/// Starts a session.
///
/// The insert reads the user's row `FOR SHARE` in the same statement, so it
/// serializes with a password change (`passwords::reset` updates that row
/// before it deletes the sessions): either this insert commits first and
/// the reset deletes the session, or the reset commits first and Postgres
/// re-checks the hash on the new row version, which no longer matches.
///
/// Inside a caller's transaction that already holds the user's row (a
/// passkey sign-in, a setup link), the `FOR SHARE` is a no-op and the
/// session commits or rolls back with the rest.
///
/// # Arguments
/// * `executor` - the database: a `&PgPool`, or a `&mut PgConnection`
///   (such as `&mut *tx`) to run inside a caller's transaction.
/// * `request` - the user, client details and lifetime.
///
/// # Returns
/// The raw id and CSRF token. Neither is stored.
///
/// # Errors
/// [`OkfError::Unauthenticated`] when the user is gone or (for a password
/// sign-in) their password changed since it was checked;
/// [`OkfError::LimitExceeded`] for an absurd `absolute`; [`OkfError::Io`]
/// if the OS random source fails; [`OkfError::Database`].
pub async fn start(
    executor: impl PgExecutor<'_>,
    request: &SessionStart<'_>,
) -> Result<NewSession, OkfError> {
    let expires_at = crate::clock::after(request.now, request.absolute)?;
    let id = keys::generate_with("")?;
    let session = NewSession {
        csrf: csrf_for(&id),
        id,
    };
    // `char_indices().nth(n)` finds the byte offset of the n-th character,
    // so the cut never splits a multi-byte character (slicing a `&str`
    // mid-character panics).
    let user_agent = request
        .user_agent
        .char_indices()
        .nth(USER_AGENT_LIMIT)
        .map_or(request.user_agent, |(end, _)| &request.user_agent[..end]);
    // `$8 IS NULL OR ...`: no condition without a stamp. `IS NOT DISTINCT
    // FROM` would also match a user whose hash was cleared; `=` does not.
    let inserted = sqlx::query(
        "INSERT INTO user_sessions \
             (id_hash, user_id, csrf_token, created_at, last_seen_at, expires_at, ip, user_agent) \
         SELECT $1, u.id, encode($3, 'hex'), $4, $4, $5, $6::inet, $7 FROM users u \
          WHERE u.id = $2 AND ($8::text IS NULL OR u.password_hash = $8) \
            FOR SHARE",
    )
    .bind(keys::hash(&session.id).as_slice())
    .bind(request.user_id)
    .bind(keys::hash(&session.csrf).as_slice())
    .bind(request.now)
    .bind(expires_at)
    .bind(request.ip.map(|address| address.to_string()))
    .bind(user_agent)
    .bind(request.password.map(PasswordStamp::as_str))
    .execute(executor)
    .await?
    .rows_affected();
    if inserted == 0 {
        return Err(OkfError::Unauthenticated {
            reason: "the user is gone or their password changed; sign in again".to_string(),
        });
    }
    Ok(session)
}

/// A stored session, as [`touch`] reads it.
#[derive(sqlx::FromRow)]
struct Stored {
    user_id: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    // Read so the activity write and the live check share one row type;
    // only the live check looks at it.
    last_seen_at: DateTime<Utc>,
}

impl Stored {
    /// The session as a request sees it, given the raw id it was found by.
    fn into_row(self, id: &str) -> SessionRow {
        SessionRow {
            user_id: self.user_id,
            csrf: csrf_for(id),
            created_at: self.created_at,
            expires_at: self.expires_at,
        }
    }
}

/// Looks a session up for a request: ends it if it is past its absolute
/// limit or idle too long, and otherwise records the activity, at most
/// once per write resolution (`idle / 4`, capped at a minute).
///
/// Idle means no use for `idle` plus one write resolution of grace (a use
/// inside a resolution of the last write is not recorded): a session is
/// never ended sooner than `idle` after its last use, and always by
/// `idle + resolution` after it.
///
/// Safe under concurrency:
/// - The activity write is one `UPDATE` whose `WHERE` requires the session
///   to be live and its `last_seen_at` stale. Postgres re-checks that
///   `WHERE` on the newest row version once it holds the row lock. So of
///   several concurrent touches, one writes and the rest find the row
///   fresh and write nothing. A session revoked or ended meanwhile matches
///   nothing either.
/// - A touch that wrote nothing reads the row again to tell "live, no write
///   needed" (the session) from "gone or ended" (`None`). A concurrent
///   writer never turns into a false sign-out.
///
/// # Arguments
/// * `pool` - the database.
/// * `id` - the raw id from the cookie.
/// * `idle` - `console_session_idle`.
/// * `now` - the request's time (server clock).
///
/// # Returns
/// The session, or `None` when there is none or it has ended.
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
    // `last_seen_at` at or before this is stale enough to rewrite. It is
    // always before `now`, so the write never moves `last_seen_at` back.
    let stale_before = now - write_resolution(idle);
    // `x > NULL` is NULL, so `$3 IS NULL` is spelled out for "no idle
    // limit".
    let written = sqlx::query_as::<_, Stored>(
        "UPDATE user_sessions SET last_seen_at = $2 \
         WHERE id_hash = $1 AND expires_at > $2 \
           AND ($3::timestamptz IS NULL OR last_seen_at > $3) \
           AND last_seen_at <= $4 \
         RETURNING user_id, created_at, expires_at, last_seen_at",
    )
    .bind(hash.as_slice())
    .bind(now)
    .bind(cutoff)
    .bind(stale_before)
    .fetch_optional(pool)
    .await?;
    if let Some(stored) = written {
        return Ok(Some(stored.into_row(id)));
    }
    // Nothing written: the session is live and fresh, or it has ended, or
    // it does not exist. A fresh read tells them apart.
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
    Ok(Some(stored.into_row(id)))
}

/// Ends one session (sign-out, or the browser's old session when it signs
/// in again).
///
/// # Arguments
/// * `executor` - the database: a `&PgPool`, or a `&mut PgConnection`
///   (such as `&mut *tx`) to run inside a caller's transaction.
/// * `id` - the raw id from the cookie.
///
/// # Returns
/// `Ok(())`, whether or not the session existed.
///
/// # Errors
/// [`OkfError::Database`]. An unknown id is not an error.
pub async fn revoke(executor: impl PgExecutor<'_>, id: &str) -> Result<(), OkfError> {
    sqlx::query("DELETE FROM user_sessions WHERE id_hash = $1")
        .bind(keys::hash(id).as_slice())
        .execute(executor)
        .await?;
    Ok(())
}

/// Whether a session is still in force (not revoked, not past its
/// absolute limit), holding its row `FOR SHARE` until the caller's
/// transaction ends, so it cannot be revoked meanwhile. A change made on
/// behalf of a session (adding a passkey) checks this after locking the
/// user's row (lock order: the user first), so a reset that ended the
/// session first refuses the change.
///
/// # Arguments
/// * `executor` - the caller's transaction (`&mut *tx`).
/// * `id` - the raw id from the cookie.
/// * `user_id` - whose session it must be.
/// * `now` - when (server clock).
///
/// # Returns
/// `true` while it is in force.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn hold_live(
    executor: impl PgExecutor<'_>,
    id: &str,
    user_id: &str,
    now: DateTime<Utc>,
) -> Result<bool, OkfError> {
    let held: Option<i32> = sqlx::query_scalar(
        "SELECT 1 FROM user_sessions \
         WHERE id_hash = $1 AND user_id = $2 AND expires_at > $3 FOR SHARE",
    )
    .bind(keys::hash(id).as_slice())
    .bind(user_id)
    .bind(now)
    .fetch_optional(executor)
    .await?;
    Ok(held.is_some())
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

        // Twelve hours idle plus the one-minute grace.
        let idle_end = now() + TimeDelta::hours(12) + TimeDelta::minutes(1);
        let idle = touch(&db.pool, &session.id, IDLE, idle_end).await.unwrap();
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
    async fn a_revoked_session_is_not_revived_by_a_touch_waiting_on_its_lock() {
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
    async fn a_short_idle_limit_keeps_an_active_session_alive() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        // (idle limit, time between touches), both in seconds: at and below
        // the one-minute cap on the write resolution.
        for (idle_secs, every_secs) in [(30, 10), (60, 20)] {
            let idle = Duration::from_secs(idle_secs);
            let session = create(&db.pool, &alice.id, None, "", ABSOLUTE, now())
                .await
                .unwrap();

            // Two minutes of use, never idle for `idle`.
            let mut at = now();
            while at < now() + TimeDelta::minutes(2) {
                at += TimeDelta::seconds(every_secs);
                let alive = touch(&db.pool, &session.id, idle, at).await.unwrap();
                assert!(alive.is_some(), "idle {idle_secs}s, touched at {at}");
            }
            // Then left alone for the idle limit plus its grace (a
            // quarter of it).
            let idle_end = at + TimeDelta::milliseconds(idle_secs as i64 * 1250);
            let ended = touch(&db.pool, &session.id, idle, idle_end).await.unwrap();

            assert!(ended.is_none(), "idle {idle_secs}s ends once idle");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_use_too_soon_to_be_written_still_counts_toward_the_idle_limit() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        let idle = Duration::from_secs(30);
        let session = create(&db.pool, &alice.id, None, "", ABSOLUTE, now())
            .await
            .unwrap();

        // +7s is inside the 7.5 s write resolution, so it is not written;
        // +32s is only 25 s after that use.
        let early = touch(&db.pool, &session.id, idle, now() + TimeDelta::seconds(7))
            .await
            .unwrap();
        let after_7s = last_seen(&db.pool, &session.id).await;
        let later = touch(&db.pool, &session.id, idle, now() + TimeDelta::seconds(32))
            .await
            .unwrap();

        assert!(early.is_some());
        assert_eq!(after_7s, now(), "the +7s use was not written");
        assert!(
            later.is_some(),
            "never ended sooner than idle after the last use"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unused_session_ends_once_idle_plus_the_resolution_has_passed() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        let idle = Duration::from_secs(30);
        // Two sessions started together, so each is touched only once.
        let just_inside = create(&db.pool, &alice.id, None, "", ABSOLUTE, now())
            .await
            .unwrap();
        let at_the_limit = create(&db.pool, &alice.id, None, "", ABSOLUTE, now())
            .await
            .unwrap();

        // 30 s idle plus the 7.5 s resolution is 37.5 s.
        let inside = touch(
            &db.pool,
            &just_inside.id,
            idle,
            now() + TimeDelta::milliseconds(37_499),
        )
        .await
        .unwrap();
        let ended = touch(
            &db.pool,
            &at_the_limit.id,
            idle,
            now() + TimeDelta::milliseconds(37_500),
        )
        .await
        .unwrap();

        assert!(inside.is_some());
        assert!(ended.is_none(), "always ended by idle + resolution");
        assert_eq!(list(&db.pool, &alice.id, None).await.unwrap().len(), 1);
    }

    /// Counts every write to `user_sessions` (in `session_writes`), so a
    /// test can see how many activity updates ran.
    async fn count_session_writes(pool: &PgPool) {
        sqlx::raw_sql(
            "CREATE TABLE session_writes (n integer NOT NULL); \
             INSERT INTO session_writes VALUES (0); \
             CREATE FUNCTION count_session_write() RETURNS trigger LANGUAGE plpgsql AS \
                 $$ BEGIN UPDATE session_writes SET n = n + 1; RETURN NULL; END $$; \
             CREATE TRIGGER count_session_writes AFTER UPDATE ON user_sessions \
                 FOR EACH ROW EXECUTE FUNCTION count_session_write();",
        )
        .execute(pool)
        .await
        .unwrap();
    }

    async fn session_writes(pool: &PgPool) -> i32 {
        sqlx::query_scalar("SELECT n FROM session_writes")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn touches_queued_on_the_row_lock_write_once_and_all_stay_signed_in() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        let session = create(&db.pool, &alice.id, None, "", ABSOLUTE, now())
            .await
            .unwrap();
        count_session_writes(&db.pool).await;
        // Hold the row lock so both touches, each needing a write, queue
        // behind it before either runs.
        let mut tx = db.pool.begin().await.unwrap();
        sqlx::query("SELECT 1 FROM user_sessions WHERE id_hash = $1 FOR UPDATE")
            .bind(keys::hash(&session.id).as_slice())
            .execute(&mut *tx)
            .await
            .unwrap();
        let at = now() + TimeDelta::hours(1);
        let racing: Vec<_> = (0..2)
            .map(|_| {
                let pool = db.pool.clone();
                let id = session.id.clone();
                tokio::spawn(async move { touch(&pool, &id, IDLE, at).await.unwrap() })
            })
            .collect();
        crate::store::catalogs::tests::until_locks_are_awaited(&db.pool, 2).await;
        tx.commit().await.unwrap();

        for handle in racing {
            assert!(handle.await.unwrap().is_some(), "no false sign-out");
        }
        assert_eq!(session_writes(&db.pool).await, 1);
        assert_eq!(last_seen(&db.pool, &session.id).await, at);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_burst_of_concurrent_touches_writes_once() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        let session = create(&db.pool, &alice.id, None, "", ABSOLUTE, now())
            .await
            .unwrap();
        count_session_writes(&db.pool).await;
        let at = now() + TimeDelta::hours(1);

        let burst: Vec<_> = (0..16)
            .map(|_| {
                let pool = db.pool.clone();
                let id = session.id.clone();
                tokio::spawn(async move { touch(&pool, &id, IDLE, at).await.unwrap() })
            })
            .collect();
        for handle in burst {
            assert!(handle.await.unwrap().is_some());
        }

        assert_eq!(session_writes(&db.pool).await, 1);
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

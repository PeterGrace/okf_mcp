//! Local passwords (spec "Passwords"): the policy, argon2id hashing at
//! OWASP's parameters, and sign-in verification with a durable lockout
//! (Decision 8: `users.failed_sign_ins` and `users.locked_until`).
//!
//! Every verification costs one argon2 check, whatever the outcome: an
//! unknown email, or a user without a password, is checked against a dummy
//! hash, so the response time does not tell an attacker which emails exist.
//! Hashing is CPU-bound (19 MiB, two passes), so it runs on tokio's
//! blocking pool, never on an async worker thread.
//!
//! The lockout holds under concurrency: the failure counter moves in one
//! atomic `UPDATE`, so simultaneous failures all count, and a success only
//! clears the counter if no concurrent failure locked the account (and the
//! password was not changed) after the row was read.
//!
//! No password, hash or `users` row holding a hash is ever logged or given a
//! `Debug` form.

use std::collections::HashSet;
use std::sync::LazyLock;

// The traits must be in scope for their methods (`hash_password`,
// `verify_password`) to be callable on `Argon2`, as with Python mixins.
use argon2::password_hash::{PasswordHasher, PasswordVerifier};
use argon2::{Argon2, PasswordHash};
use chrono::{DateTime, TimeDelta, Utc};
use sqlx::{PgConnection, PgPool};

use crate::error::OkfError;
use crate::store::users::{is_lookup_email, user_auth_columns, UserAuth};

/// The shortest password accepted, in characters (Unicode code points).
pub const MIN_LENGTH: usize = 12;

/// The longest password accepted, in characters: argon2 takes any length,
/// but a megabyte "password" is a cheap way to burn CPU.
pub const MAX_LENGTH: usize = 1024;

/// Consecutive failures that lock an account.
pub const LOCK_AFTER: i32 = 10;

/// How long a locked account stays locked.
pub const LOCK_MINUTES: i64 = 15;

/// The bundled common-password list, lowercase, one per line (`#` lines are
/// comments). Only entries of at least [`MIN_LENGTH`] characters matter,
/// since shorter ones fail the length rule first.
// `LazyLock` builds the set on first use and shares it after, like a
// module-level constant computed once in Python. `include_str!` embeds the
// file's text in the binary at compile time.
static COMMON: LazyLock<HashSet<&'static str>> = LazyLock::new(|| {
    include_str!("common_passwords.txt")
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .collect()
});

/// An argon2id hash, at the default parameters, of a random password that
/// was thrown away. It is checked when there is no real hash to check, so
/// that path costs the same time. A constant rather than a hash computed on
/// first use, so the first unknown email costs no more than any other.
const DUMMY_HASH: &str =
    "$argon2id$v=19$m=19456,t=2,p=1$NB4HZTkJ899VClQMrRFf1A$AD++Ro8hQJ2TjI7miH3beMHlHNF6h4497/Tz+IIY5FM";

/// Which password a sign-in proved: the stored hash it was checked
/// against. A console session started from it is inserted only while the
/// user still has that hash (`user_sessions::start`), so a sign-in that
/// verified a password a reset has since replaced never gets a session.
/// A new password always has a new hash (a fresh salt), even when the
/// text is the same. `Debug` is redacted: the hash is never logged.
#[derive(Clone, PartialEq, Eq)]
pub struct PasswordStamp(String);

impl PasswordStamp {
    /// The stored PHC string, for the session insert's comparison.
    pub(crate) fn as_str(&self) -> &str {
        &self.0
    }
}

// Hand-written (Python's `__repr__`), so `{:?}` never prints the hash.
impl std::fmt::Debug for PasswordStamp {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PasswordStamp(<redacted>)")
    }
}

/// A password that matched: the user and the [`PasswordStamp`] it matched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    /// The user.
    pub user: UserAuth,
    /// The hash the password matched, for a session start that must not
    /// outlive a password change.
    pub stamp: PasswordStamp,
}

/// The outcome of a password sign-in. Every non-`Ok` outcome answers the
/// same 401 to the client; the difference is for the audit log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignInOutcome {
    /// The password matched an unlocked account.
    Ok(Verified),
    /// Unknown email, no password set, or a wrong password.
    Failed,
    /// The account is locked (or this failure locked it).
    Locked,
}

/// Checks a proposed password against the policy: 12 to 1024 characters,
/// not in the bundled common list (compared in lowercase). No composition
/// rules. The refusal never repeats the password.
///
/// # Arguments
/// * `password` - the proposed password.
///
/// # Errors
/// [`OkfError::LimitExceeded`] with `limit: "password"`, naming the rule.
pub fn check_policy(password: &str) -> Result<(), OkfError> {
    let refuse = |reason: String| OkfError::LimitExceeded {
        limit: "password",
        reason,
    };
    let length = password.chars().count();
    if length < MIN_LENGTH {
        return Err(refuse(format!(
            "a password needs at least {MIN_LENGTH} characters"
        )));
    }
    if length > MAX_LENGTH {
        return Err(refuse(format!(
            "a password may have at most {MAX_LENGTH} characters"
        )));
    }
    if COMMON.contains(password.to_lowercase().as_str()) {
        return Err(refuse(
            "this password is on a list of common passwords; choose another".to_string(),
        ));
    }
    Ok(())
}

/// Hashes a password with argon2id (m=19456 KiB, t=2, p=1) and a fresh
/// 16-byte salt from the OS. CPU-bound: call it from `spawn_blocking`.
///
/// # Arguments
/// * `password` - the password (already through [`check_policy`]).
///
/// # Returns
/// The PHC string (`$argon2id$v=19$m=19456,t=2,p=1$<salt>$<hash>`).
///
/// # Errors
/// [`OkfError::Io`] if the OS random source fails.
pub fn hash(password: &str) -> Result<String, OkfError> {
    let hashed: PasswordHash = Argon2::default()
        .hash_password(password.as_bytes())
        .map_err(hasher_error)?;
    Ok(hashed.to_string())
}

/// Does `password` match the stored PHC string? The parameters come from
/// the string, and the comparison is constant-time. A malformed string
/// matches nothing.
fn matches(stored: &str, password: &str) -> bool {
    Argon2::default()
        .verify_password(password.as_bytes(), stored)
        .is_ok()
}

/// [`matches`] on tokio's blocking pool. Takes owned strings because the
/// closure outlives this call's borrows.
async fn matches_off_thread(stored: String, password: String) -> Result<bool, OkfError> {
    // `move` hands the two strings to the closure, which runs on another
    // thread; `.await` waits for it without blocking this one.
    tokio::task::spawn_blocking(move || matches(&stored, &password))
        .await
        .map_err(hasher_error)
}

/// The error for a hasher that could not run (no OS randomness, or its
/// blocking task died). The messages of both name no input.
fn hasher_error(error: impl std::fmt::Display) -> OkfError {
    OkfError::Io {
        path: "<password hasher>".to_string(),
        source: std::io::Error::other(error.to_string()),
    }
}

/// One `users` row as sign-in reads it. Deliberately no `Debug`: it holds
/// the hash, which must never reach a log line.
#[derive(sqlx::FromRow)]
struct Candidate {
    // `flatten` reads `UserAuth`'s columns from the same row.
    #[sqlx(flatten)]
    user: UserAuth,
    password_hash: Option<String>,
    locked_until: Option<DateTime<Utc>>,
}

/// The columns a [`Candidate`] reads, over `users u`.
macro_rules! candidate_columns {
    () => {
        concat!(
            "SELECT ",
            user_auth_columns!(),
            ", u.password_hash, u.locked_until FROM users u"
        )
    };
}

/// Checks an email and password, keeping the lockout counters.
///
/// # Arguments
/// * `pool` - the database.
/// * `email` - as typed (compared case-insensitively).
/// * `password` - as typed.
/// * `now` - the attempt's time (server clock), for the lock.
///
/// # Returns
/// [`SignInOutcome::Ok`] with the user; [`SignInOutcome::Failed`] for an
/// unknown email, a user without a password or a wrong password;
/// [`SignInOutcome::Locked`] for a locked account, or for the failure that
/// locks it.
///
/// # Errors
/// [`OkfError::Database`]; [`OkfError::Io`] if the hasher task fails.
pub async fn verify(
    pool: &PgPool,
    email: &str,
    password: &str,
    now: DateTime<Utc>,
) -> Result<SignInOutcome, OkfError> {
    // An address no user can have (a NUL, which Postgres would refuse, among
    // others) is an unknown email: no query, and the same dummy hash below.
    let candidate = if is_lookup_email(email) {
        sqlx::query_as::<_, Candidate>(concat!(candidate_columns!(), " WHERE u.email = $1::citext"))
            .bind(email)
            .fetch_optional(pool)
            .await?
    } else {
        None
    };
    check_candidate(pool, candidate, password, now).await
}

/// Checks the password of the user with this id, with the same lockout
/// rules as [`verify`] (Task 10b's password change checks the current
/// password of a signed-in user, who may have no email).
///
/// # Arguments
/// * `pool` - the database.
/// * `user_id` - the user's `users.id`.
/// * `password` - as typed.
/// * `now` - the attempt's time (server clock), for the lock.
///
/// # Returns
/// As [`verify`]; an unknown id is [`SignInOutcome::Failed`].
///
/// # Errors
/// [`OkfError::Database`]; [`OkfError::Io`] if the hasher task fails.
pub async fn verify_user(
    pool: &PgPool,
    user_id: &str,
    password: &str,
    now: DateTime<Utc>,
) -> Result<SignInOutcome, OkfError> {
    let candidate =
        sqlx::query_as::<_, Candidate>(concat!(candidate_columns!(), " WHERE u.id = $1"))
            .bind(user_id)
            .fetch_optional(pool)
            .await?;
    check_candidate(pool, candidate, password, now).await
}

/// The shared body of [`verify`] and [`verify_user`], once the user row
/// (if any) is read: the hash check, the lock, and the failure counter.
///
/// The row may be stale by the time the hash is checked (argon2 takes tens
/// of milliseconds), so both writes re-check the lock in SQL rather than
/// trusting `candidate.locked_until`.
async fn check_candidate(
    pool: &PgPool,
    candidate: Option<Candidate>,
    password: &str,
    now: DateTime<Utc>,
) -> Result<SignInOutcome, OkfError> {
    // No user, or no password: spend the same time on the dummy hash.
    // `and_then` keeps the candidate only when it has a hash, like
    // `c if c and c.password_hash else None`.
    let Some((candidate, stored)) =
        candidate.and_then(|c| c.password_hash.clone().map(|stored| (c, stored)))
    else {
        matches_off_thread(DUMMY_HASH.to_string(), password.to_string()).await?;
        return Ok(SignInOutcome::Failed);
    };
    let matched = matches_off_thread(stored.clone(), password.to_string()).await?;
    // Checked after the hash, so a locked account costs the same time too.
    if candidate.locked_until.is_some_and(|until| until > now) {
        return Ok(SignInOutcome::Locked);
    }
    if matched {
        return record_success(pool, candidate.user, &stored, now).await;
    }
    record_failure(pool, &candidate.user.id, now).await
}

/// Clears the lockout counters after a matching password, unless the
/// account was locked (by a concurrent failure) or its password changed
/// since the row was read.
///
/// # Returns
/// [`SignInOutcome::Ok`]; [`SignInOutcome::Locked`] if a lock appeared;
/// [`SignInOutcome::Failed`] if the password changed or the user is gone.
async fn record_success(
    pool: &PgPool,
    user: UserAuth,
    stored: &str,
    now: DateTime<Utc>,
) -> Result<SignInOutcome, OkfError> {
    // One statement: the CASEs read the row as it is now, under its lock, so
    // a lock set after the row was first read keeps the account locked.
    // `NULL > $2` is NULL, which CASE treats as false.
    let locked: Option<bool> = sqlx::query_scalar(
        "UPDATE users SET \
             failed_sign_ins = CASE WHEN locked_until > $2 THEN failed_sign_ins ELSE 0 END, \
             locked_until = CASE WHEN locked_until > $2 THEN locked_until ELSE NULL END \
         WHERE id = $1 AND password_hash = $3 \
         RETURNING COALESCE(locked_until > $2, false)",
    )
    .bind(&user.id)
    .bind(now)
    .bind(stored)
    .fetch_optional(pool)
    .await?;
    Ok(match locked {
        Some(false) => SignInOutcome::Ok(Verified {
            user,
            stamp: PasswordStamp(stored.to_string()),
        }),
        Some(true) => SignInOutcome::Locked,
        None => SignInOutcome::Failed,
    })
}

/// Counts a failed password, locking the account at the [`LOCK_AFTER`]th
/// consecutive failure.
///
/// # Returns
/// [`SignInOutcome::Locked`] when the account is locked after this failure
/// (by it, or concurrently); [`SignInOutcome::Failed`] otherwise.
async fn record_failure(
    pool: &PgPool,
    user_id: &str,
    now: DateTime<Utc>,
) -> Result<SignInOutcome, OkfError> {
    // One atomic statement, so concurrent failures all count: each waits for
    // the row lock and then reads the count the previous one wrote. The
    // CASEs read the old values. A failure while the account is already
    // locked (one that read the row before the lock was set) leaves both
    // alone; the failure that reaches LOCK_AFTER sets the lock and starts
    // the count again, so the next failure after the lock ends does not lock
    // at once.
    let locked: Option<bool> = sqlx::query_scalar(
        "UPDATE users SET \
             failed_sign_ins = CASE WHEN locked_until > $4 THEN failed_sign_ins \
                                    WHEN failed_sign_ins + 1 >= $2 THEN 0 \
                                    ELSE failed_sign_ins + 1 END, \
             locked_until = CASE WHEN locked_until > $4 THEN locked_until \
                                 WHEN failed_sign_ins + 1 >= $2 THEN $3 \
                                 ELSE locked_until END \
         WHERE id = $1 \
         RETURNING COALESCE(locked_until > $4, false)",
    )
    .bind(user_id)
    .bind(LOCK_AFTER)
    .bind(now + TimeDelta::minutes(LOCK_MINUTES))
    .bind(now)
    .fetch_optional(pool)
    .await?;
    // `None`: the user was deleted meanwhile; nothing to count.
    Ok(if locked == Some(true) {
        SignInOutcome::Locked
    } else {
        SignInOutcome::Failed
    })
}

/// Sets a user's password (after the policy), records when, and clears the
/// lockout counters. Revoking other sessions is the caller's choice (spec
/// "Revocation"); [`reset`] ends them all in the same transaction.
///
/// # Arguments
/// * `pool` - the database.
/// * `user_id` - a `users.id`.
/// * `password` - the new password.
/// * `now` - when (server clock).
///
/// # Errors
/// [`OkfError::LimitExceeded`] from [`check_policy`]; [`OkfError::UnknownName`]
/// for an unknown id; [`OkfError::Io`] if hashing fails; [`OkfError::Database`].
pub async fn set(
    pool: &PgPool,
    user_id: &str,
    password: &str,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    let hashed = NewPassword::hash(password).await?;
    let mut conn = pool.acquire().await?;
    set_in(&mut conn, user_id, &hashed, now).await
}

/// Sets a user's password and ends every console session they have, in one
/// transaction ([`reset_in`]).
///
/// # Arguments
/// * `pool` - the database.
/// * `user_id` - a `users.id`.
/// * `password` - the new password.
/// * `now` - when (server clock).
///
/// # Returns
/// How many sessions ended.
///
/// # Errors
/// As [`set`].
pub async fn reset(
    pool: &PgPool,
    user_id: &str,
    password: &str,
    now: DateTime<Utc>,
) -> Result<u64, OkfError> {
    let hashed = NewPassword::hash(password).await?;
    let mut tx = pool.begin().await?;
    let ended = reset_in(&mut tx, user_id, &hashed, now).await?;
    tx.commit().await?;
    Ok(ended)
}

/// A new password, already through the policy and hashed, ready to store
/// inside a transaction. Hashing takes tens of milliseconds, so it is done
/// before the transaction opens, never while it holds row locks. `Debug`
/// is redacted: the hash is never logged.
pub struct NewPassword(String);

impl NewPassword {
    /// Checks `password` against the policy and hashes it (argon2id) on
    /// tokio's blocking pool.
    ///
    /// # Arguments
    /// * `password` - the proposed password.
    ///
    /// # Returns
    /// The hash, to pass to [`set_in`] or [`reset_in`].
    ///
    /// # Errors
    /// [`OkfError::LimitExceeded`] from [`check_policy`]; [`OkfError::Io`]
    /// if hashing fails.
    pub async fn hash(password: &str) -> Result<Self, OkfError> {
        check_policy(password)?;
        let owned = password.to_string();
        // The first `?` is the task's (did it run?), the second `hash`'s own.
        let hashed = tokio::task::spawn_blocking(move || hash(&owned))
            .await
            .map_err(hasher_error)??;
        Ok(Self(hashed))
    }
}

impl NewPassword {
    /// The [`PasswordStamp`] this password has once stored: a session
    /// started from it (setup signs in after storing the password) is
    /// refused if the password changes again before the session is
    /// inserted.
    ///
    /// # Returns
    /// The stamp.
    pub fn stamp(&self) -> PasswordStamp {
        PasswordStamp(self.0.clone())
    }
}

// Hand-written (Python's `__repr__`), so `{:?}` never prints the hash.
impl std::fmt::Debug for NewPassword {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("NewPassword(<redacted>)")
    }
}

/// Stores a new password, records when, and clears the lockout counters,
/// inside the caller's transaction (so an emailed link's redemption and the
/// change commit together). The `UPDATE` holds the user's row lock until the
/// transaction ends.
///
/// # Arguments
/// * `conn` - the transaction (`&mut *tx`), or a pooled connection.
/// * `user_id` - a `users.id`.
/// * `password` - from [`NewPassword::hash`].
/// * `now` - when (server clock).
///
/// # Errors
/// [`OkfError::UnknownName`] for an unknown id; [`OkfError::Database`].
pub async fn set_in(
    conn: &mut PgConnection,
    user_id: &str,
    password: &NewPassword,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    let updated = sqlx::query(
        "UPDATE users SET password_hash = $2, password_changed_at = $3, \
                          failed_sign_ins = 0, locked_until = NULL \
         WHERE id = $1",
    )
    .bind(user_id)
    .bind(&password.0)
    .bind(now)
    .execute(conn)
    .await?
    .rows_affected();
    if updated == 0 {
        return Err(OkfError::UnknownName {
            noun: "user",
            name: user_id.to_string(),
        });
    }
    Ok(())
}

/// [`set_in`], then ends every console session of the user, in the caller's
/// transaction (a reset link, spec "Revocation").
///
/// The `UPDATE` takes the user's row lock before the sessions are deleted,
/// and a password sign-in inserts its session only while holding a share
/// lock on that row with the hash it verified (`user_sessions::start`). So
/// the two serialize: a session inserted first is deleted here, and one
/// inserted after finds the hash changed and is refused. A failure anywhere
/// in the transaction rolls the password and the revocation back together.
///
/// # Arguments
/// * `conn` - the transaction (`&mut *tx`).
/// * `user_id` - a `users.id`.
/// * `password` - from [`NewPassword::hash`].
/// * `now` - when (server clock).
///
/// # Returns
/// How many sessions ended.
///
/// # Errors
/// As [`set_in`].
pub async fn reset_in(
    conn: &mut PgConnection,
    user_id: &str,
    password: &NewPassword,
    now: DateTime<Utc>,
) -> Result<u64, OkfError> {
    set_in(&mut *conn, user_id, password, now).await?;
    crate::store::user_sessions::revoke_all(&mut *conn, user_id, None).await
}

/// A password change asked for by a signed-in console session (`PUT
/// /api/v1/me/password`). `Debug` is redacted where it holds a secret.
pub struct SessionChange<'a> {
    /// Whose password.
    pub user_id: &'a str,
    /// The raw id of the session asking (the cookie's value).
    pub session_id: &'a str,
    /// The password this change replaces, as the current password the
    /// caller typed proved it; `None` when the user had none.
    pub replaces: Option<&'a PasswordStamp>,
    /// The new password, from [`NewPassword::hash`].
    pub password: &'a NewPassword,
    /// Keep the user's other sessions (otherwise they end).
    pub keep_sessions: bool,
    /// When (server clock).
    pub now: DateTime<Utc>,
}

// Hand-written (Python's `__repr__`): the session id is a credential.
impl std::fmt::Debug for SessionChange<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionChange")
            .field("user_id", &self.user_id)
            .field("session_id", &"<redacted>")
            .field("replaces", &self.replaces)
            .field("password", &self.password)
            .field("keep_sessions", &self.keep_sessions)
            .field("now", &self.now)
            .finish()
    }
}

/// Changes a signed-in user's password and, unless kept, ends their other
/// sessions, inside the caller's transaction (which also queues the
/// notice, then commits).
///
/// The user's row is locked first (`FOR NO KEY UPDATE`, the lock order of
/// `email_tokens`' module docs), then, under that lock:
/// - the asking session must still be in force (held `FOR SHARE`), so a
///   reset or an admin change that ended it refuses the change;
/// - the stored password must still be the one [`SessionChange::replaces`]
///   names, so a change made meanwhile from another session (or a reset)
///   is never overwritten by someone who only knew the old password.
///
/// # Arguments
/// * `conn` - the caller's transaction (`&mut *tx`).
/// * `change` - what to store and for whom.
///
/// # Returns
/// How many other sessions ended.
///
/// # Errors
/// [`OkfError::Unauthenticated`] once the session or the user is gone;
/// [`OkfError::Forbidden`] when the password changed since it was checked;
/// [`OkfError::Database`].
pub async fn change_from_session_in(
    conn: &mut PgConnection,
    change: &SessionChange<'_>,
) -> Result<u64, OkfError> {
    // `Option<Option<String>>`: the outer `None` is "no such user", the
    // inner one "no password".
    let stored: Option<Option<String>> =
        sqlx::query_scalar("SELECT password_hash FROM users WHERE id = $1 FOR NO KEY UPDATE")
            .bind(change.user_id)
            .fetch_optional(&mut *conn)
            .await?;
    let live = stored.is_some()
        && crate::store::user_sessions::hold_live(
            &mut *conn,
            change.session_id,
            change.user_id,
            change.now,
        )
        .await?;
    let Some(stored) = stored.filter(|_| live) else {
        return Err(OkfError::Unauthenticated {
            reason: "this session has ended; sign in again".to_string(),
        });
    };
    if stored.as_deref() != change.replaces.map(PasswordStamp::as_str) {
        return Err(OkfError::Forbidden {
            reason: "the password changed meanwhile; give the current one again".to_string(),
        });
    }
    set_in(&mut *conn, change.user_id, change.password, change.now).await?;
    if change.keep_sessions {
        return Ok(0);
    }
    crate::store::user_sessions::revoke_all(&mut *conn, change.user_id, Some(change.session_id))
        .await
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

    const GOOD: &str = "violet staple orbit";

    /// Reads alice's row as `verify` does, so a test can change the row
    /// between that read and the check (the race window argon2 opens).
    async fn alice_candidate(pool: &PgPool) -> Option<Candidate> {
        sqlx::query_as::<_, Candidate>(concat!(candidate_columns!(), " WHERE u.name = 'alice'"))
            .fetch_optional(pool)
            .await
            .unwrap()
    }

    /// alice's failure count and lock, as stored.
    async fn alice_counters(pool: &PgPool) -> (i32, Option<DateTime<Utc>>) {
        sqlx::query_as("SELECT failed_sign_ins, locked_until FROM users WHERE name = 'alice'")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// Runs `count` wrong-password sign-ins for alice at once, each on its
    /// own task, and returns their outcomes.
    async fn concurrent_failures(pool: &PgPool, count: i32) -> Vec<SignInOutcome> {
        let tasks: Vec<_> = (0..count)
            .map(|_| {
                let pool = pool.clone();
                tokio::spawn(async move {
                    verify(&pool, "alice@example.com", "wrong password!", now()).await
                })
            })
            .collect();
        let mut outcomes = Vec::with_capacity(tasks.len());
        for task in tasks {
            outcomes.push(task.await.unwrap().unwrap());
        }
        outcomes
    }

    #[test]
    fn the_policy_wants_twelve_characters_and_nothing_common() {
        let short = check_policy("elevenchars");
        let common = check_policy("Password1234");
        let long = check_policy(&"x".repeat(MAX_LENGTH + 1));
        // Twelve code points, more than twelve bytes: length counts characters.
        let multibyte = check_policy("ééééééééééé1");

        for refused in [short, common, long] {
            assert!(
                matches!(
                    &refused,
                    Err(OkfError::LimitExceeded { limit: "password", reason })
                        if !reason.contains("elevenchars") && !reason.contains("Password1234")
                ),
                "{refused:?}"
            );
        }
        assert!(multibyte.is_ok());
        assert!(check_policy(GOOD).is_ok());
    }

    #[test]
    fn the_common_list_is_loaded_and_lowercase() {
        assert!(COMMON.len() >= 50);
        assert!(COMMON.iter().all(|entry| *entry == entry.to_lowercase()));
        assert!(COMMON
            .iter()
            .all(|entry| entry.chars().count() >= MIN_LENGTH));
    }

    #[test]
    fn hashing_is_argon2id_at_owasp_parameters_with_a_fresh_salt() {
        let first = hash(GOOD).unwrap();
        let second = hash(GOOD).unwrap();

        assert!(
            first.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
            "{first}"
        );
        assert_ne!(first, second, "each hash has its own salt");
        assert!(matches(&first, GOOD));
        assert!(!matches(&first, "violet staple orbits"));
        assert!(!matches("not a phc string", GOOD));
    }

    #[test]
    fn the_dummy_hash_parses_and_matches_nothing_a_user_types() {
        assert!(
            DUMMY_HASH.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"),
            "the dummy must cost what a real hash costs"
        );
        assert!(PasswordHash::new(DUMMY_HASH).is_ok());
        assert!(!matches(DUMMY_HASH, GOOD));
        assert!(!matches(DUMMY_HASH, ""));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_right_password_signs_in_and_case_of_the_email_does_not_matter() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", Some("alice@example.com")).await;
        set(&db.pool, &alice.id, GOOD, now()).await.unwrap();

        let outcome = verify(&db.pool, "ALICE@example.com", GOOD, now())
            .await
            .unwrap();

        assert!(matches!(outcome, SignInOutcome::Ok(ref verified) if verified.user == alice));
        let changed: Option<DateTime<Utc>> =
            sqlx::query_scalar("SELECT password_changed_at FROM users WHERE name = 'alice'")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(changed, Some(now()));
        let stored: String =
            sqlx::query_scalar("SELECT password_hash FROM users WHERE name = 'alice'")
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert!(!stored.contains(GOOD), "only the hash is stored");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_wrong_password_an_unknown_email_and_no_password_all_fail_alike() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", Some("alice@example.com")).await;
        seed_user(&db.pool, "bob", Some("bob@example.com")).await;
        set(&db.pool, &alice.id, GOOD, now()).await.unwrap();

        let wrong = verify(&db.pool, "alice@example.com", "orbit staple violet", now()).await;
        let unknown = verify(&db.pool, "carol@example.com", GOOD, now()).await;
        let no_password = verify(&db.pool, "bob@example.com", GOOD, now()).await;

        assert_eq!(wrong.unwrap(), SignInOutcome::Failed);
        assert_eq!(unknown.unwrap(), SignInOutcome::Failed);
        assert_eq!(no_password.unwrap(), SignInOutcome::Failed);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn verify_user_checks_by_id_even_without_an_email() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        set(&db.pool, &alice.id, GOOD, now()).await.unwrap();

        let right = verify_user(&db.pool, &alice.id, GOOD, now()).await.unwrap();
        let wrong = verify_user(&db.pool, &alice.id, "orbit staple violet", now())
            .await
            .unwrap();
        let unknown = verify_user(&db.pool, "no-such-id", GOOD, now())
            .await
            .unwrap();

        assert!(matches!(right, SignInOutcome::Ok(ref verified) if verified.user.id == alice.id));
        assert_eq!(wrong, SignInOutcome::Failed);
        assert_eq!(unknown, SignInOutcome::Failed);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ten_failures_lock_for_fifteen_minutes_even_against_the_right_password() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", Some("alice@example.com")).await;
        set(&db.pool, &alice.id, GOOD, now()).await.unwrap();

        let mut outcomes = Vec::new();
        for _ in 0..LOCK_AFTER {
            outcomes.push(
                verify(&db.pool, "alice@example.com", "wrong", now())
                    .await
                    .unwrap(),
            );
        }
        let while_locked = verify(&db.pool, "alice@example.com", GOOD, now())
            .await
            .unwrap();
        let by_id_while_locked = verify_user(&db.pool, &alice.id, GOOD, now()).await.unwrap();
        let just_before = now() + TimeDelta::minutes(LOCK_MINUTES) - TimeDelta::seconds(1);
        let still_locked = verify(&db.pool, "alice@example.com", GOOD, just_before)
            .await
            .unwrap();
        let after = now() + TimeDelta::minutes(LOCK_MINUTES) + TimeDelta::seconds(1);
        let unlocked = verify(&db.pool, "alice@example.com", GOOD, after)
            .await
            .unwrap();

        // `[..9]` is a slice of the first nine, like Python's `outcomes[:9]`.
        assert!(
            outcomes[..9].iter().all(|o| *o == SignInOutcome::Failed),
            "{outcomes:?}"
        );
        assert_eq!(outcomes[9], SignInOutcome::Locked);
        assert_eq!(while_locked, SignInOutcome::Locked);
        assert_eq!(by_id_while_locked, SignInOutcome::Locked);
        assert_eq!(still_locked, SignInOutcome::Locked);
        assert!(matches!(unlocked, SignInOutcome::Ok(ref verified) if verified.user == alice));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_success_resets_the_failure_count() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", Some("alice@example.com")).await;
        set(&db.pool, &alice.id, GOOD, now()).await.unwrap();
        for _ in 0..LOCK_AFTER - 1 {
            verify(&db.pool, "alice@example.com", "wrong", now())
                .await
                .unwrap();
        }

        verify(&db.pool, "alice@example.com", GOOD, now())
            .await
            .unwrap();
        let next = verify(&db.pool, "alice@example.com", "wrong", now())
            .await
            .unwrap();

        assert_eq!(
            next,
            SignInOutcome::Failed,
            "the count started again from zero"
        );
        assert_eq!(alice_counters(&db.pool).await, (1, None));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_failures_all_count() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", Some("alice@example.com")).await;
        set(&db.pool, &alice.id, GOOD, now()).await.unwrap();

        let outcomes = concurrent_failures(&db.pool, LOCK_AFTER - 1).await;

        assert!(
            outcomes.iter().all(|o| *o == SignInOutcome::Failed),
            "{outcomes:?}"
        );
        assert_eq!(alice_counters(&db.pool).await, (LOCK_AFTER - 1, None));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ten_concurrent_failures_lock_exactly_once() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", Some("alice@example.com")).await;
        set(&db.pool, &alice.id, GOOD, now()).await.unwrap();

        let outcomes = concurrent_failures(&db.pool, LOCK_AFTER).await;
        let then = verify(&db.pool, "alice@example.com", GOOD, now())
            .await
            .unwrap();

        let locked = outcomes
            .iter()
            .filter(|o| **o == SignInOutcome::Locked)
            .count();
        assert_eq!(locked, 1, "{outcomes:?}");
        assert_eq!(then, SignInOutcome::Locked);
        assert_eq!(
            alice_counters(&db.pool).await,
            (0, Some(now() + TimeDelta::minutes(LOCK_MINUTES)))
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_lock_set_while_the_right_password_is_checked_still_holds() {
        // The row is read, then a concurrent failure locks the account, then
        // the (stale) check of the right password finishes.
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", Some("alice@example.com")).await;
        set(&db.pool, &alice.id, GOOD, now()).await.unwrap();
        let stale = alice_candidate(&db.pool).await;
        let lock = now() + TimeDelta::minutes(LOCK_MINUTES);
        sqlx::query("UPDATE users SET failed_sign_ins = 0, locked_until = $2 WHERE id = $1")
            .bind(&alice.id)
            .bind(lock)
            .execute(&db.pool)
            .await
            .unwrap();

        let outcome = check_candidate(&db.pool, stale, GOOD, now()).await.unwrap();

        assert_eq!(outcome, SignInOutcome::Locked);
        assert_eq!(alice_counters(&db.pool).await, (0, Some(lock)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failure_that_read_the_row_before_a_lock_does_not_count_during_it() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", Some("alice@example.com")).await;
        set(&db.pool, &alice.id, GOOD, now()).await.unwrap();
        let stale = alice_candidate(&db.pool).await;
        let lock = now() + TimeDelta::minutes(LOCK_MINUTES);
        sqlx::query("UPDATE users SET failed_sign_ins = 0, locked_until = $2 WHERE id = $1")
            .bind(&alice.id)
            .bind(lock)
            .execute(&db.pool)
            .await
            .unwrap();

        let outcome = check_candidate(&db.pool, stale, "wrong", now())
            .await
            .unwrap();

        assert_eq!(outcome, SignInOutcome::Locked);
        assert_eq!(alice_counters(&db.pool).await, (0, Some(lock)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_old_password_checked_while_it_is_changed_fails() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", Some("alice@example.com")).await;
        set(&db.pool, &alice.id, GOOD, now()).await.unwrap();
        let stale = alice_candidate(&db.pool).await;
        set(&db.pool, &alice.id, "a brand new passphrase", now())
            .await
            .unwrap();

        let outcome = check_candidate(&db.pool, stale, GOOD, now()).await.unwrap();

        assert_eq!(outcome, SignInOutcome::Failed);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn setting_a_password_checks_the_policy_clears_a_lock_and_knows_the_user() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", Some("alice@example.com")).await;
        sqlx::query("UPDATE users SET failed_sign_ins = 4, locked_until = $2 WHERE id = $1")
            .bind(&alice.id)
            .bind(now() + TimeDelta::hours(1))
            .execute(&db.pool)
            .await
            .unwrap();

        let weak = set(&db.pool, &alice.id, "short", now()).await;
        set(&db.pool, &alice.id, GOOD, now()).await.unwrap();
        let unknown = set(&db.pool, "no-such-id", GOOD, now()).await;

        assert!(matches!(
            weak,
            Err(OkfError::LimitExceeded {
                limit: "password",
                ..
            })
        ));
        assert!(matches!(
            unknown,
            Err(OkfError::UnknownName { noun: "user", .. })
        ));
        let outcome = verify(&db.pool, "alice@example.com", GOOD, now())
            .await
            .unwrap();
        assert!(matches!(outcome, SignInOutcome::Ok(ref verified) if verified.user == alice));
    }

    /// Two console sessions for `user_id`: (the one asking, another).
    async fn two_sessions(pool: &PgPool, user_id: &str) -> (String, String) {
        let start = || {
            crate::store::user_sessions::create(
                pool,
                user_id,
                None,
                "",
                std::time::Duration::from_secs(3600),
                now(),
            )
        };
        (start().await.unwrap().id, start().await.unwrap().id)
    }

    /// How many console sessions `user_id` has.
    async fn sessions_of(pool: &PgPool, user_id: &str) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM user_sessions WHERE user_id = $1")
            .bind(user_id)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// The stamp of alice's current password (`GOOD`).
    async fn alices_stamp(pool: &PgPool) -> PasswordStamp {
        match verify(pool, "alice@example.com", GOOD, now())
            .await
            .unwrap()
        {
            SignInOutcome::Ok(verified) => verified.stamp,
            other => panic!("alice's password was refused: {other:?}"),
        }
    }

    /// Runs [`change_from_session_in`] in its own transaction and commits.
    async fn change(pool: &PgPool, change: &SessionChange<'_>) -> Result<u64, OkfError> {
        let mut tx = pool.begin().await?;
        let ended = change_from_session_in(&mut tx, change).await?;
        tx.commit().await?;
        Ok(ended)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_change_keeps_its_session_and_ends_the_others_unless_kept() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", Some("alice@example.com")).await;
        let (asking, _) = two_sessions(&db.pool, &alice.id).await;
        let first = NewPassword::hash("amber quarry signal").await.unwrap();
        let second = NewPassword::hash("copper meadow lantern").await.unwrap();

        let ended = change(
            &db.pool,
            &SessionChange {
                user_id: &alice.id,
                session_id: &asking,
                replaces: None,
                password: &first,
                keep_sessions: true,
                now: now(),
            },
        )
        .await
        .unwrap();
        assert_eq!((ended, sessions_of(&db.pool, &alice.id).await), (0, 2));

        let stamp = first.stamp();
        let ended = change(
            &db.pool,
            &SessionChange {
                user_id: &alice.id,
                session_id: &asking,
                replaces: Some(&stamp),
                password: &second,
                keep_sessions: false,
                now: now(),
            },
        )
        .await
        .unwrap();

        assert_eq!((ended, sessions_of(&db.pool, &alice.id).await), (1, 1));
        let kept: i64 = sqlx::query_scalar("SELECT count(*) FROM user_sessions WHERE id_hash = $1")
            .bind(crate::keys::hash(&asking).as_slice())
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(kept, 1, "the asking session stays");
        let outcome = verify(
            &db.pool,
            "alice@example.com",
            "copper meadow lantern",
            now(),
        )
        .await
        .unwrap();
        assert!(matches!(outcome, SignInOutcome::Ok(_)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_session_change_refuses_a_password_that_changed_since_it_was_checked() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", Some("alice@example.com")).await;
        set(&db.pool, &alice.id, GOOD, now()).await.unwrap();
        let (asking, _) = two_sessions(&db.pool, &alice.id).await;
        let stale = alices_stamp(&db.pool).await;
        // Another change, from elsewhere and keeping the sessions, lands
        // between the check and this change.
        set(&db.pool, &alice.id, "copper meadow lantern", now())
            .await
            .unwrap();
        let mine = NewPassword::hash("amber quarry signal").await.unwrap();
        let first_ever = NewPassword::hash("violet harbor quill").await.unwrap();

        let refused = change(
            &db.pool,
            &SessionChange {
                user_id: &alice.id,
                session_id: &asking,
                replaces: Some(&stale),
                password: &mine,
                keep_sessions: false,
                now: now(),
            },
        )
        .await;
        // A "first password" when one has been set meanwhile is refused too.
        let also_refused = change(
            &db.pool,
            &SessionChange {
                user_id: &alice.id,
                session_id: &asking,
                replaces: None,
                password: &first_ever,
                keep_sessions: false,
                now: now(),
            },
        )
        .await;

        assert!(
            matches!(refused, Err(OkfError::Forbidden { .. })),
            "{refused:?}"
        );
        assert!(
            matches!(also_refused, Err(OkfError::Forbidden { .. })),
            "{also_refused:?}"
        );
        assert_eq!(sessions_of(&db.pool, &alice.id).await, 2, "nothing ended");
        let outcome = verify(
            &db.pool,
            "alice@example.com",
            "copper meadow lantern",
            now(),
        )
        .await
        .unwrap();
        assert!(
            matches!(outcome, SignInOutcome::Ok(_)),
            "the other change stands"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_change_queued_behind_a_reset_is_refused_and_the_reset_stands() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", Some("alice@example.com")).await;
        set(&db.pool, &alice.id, GOOD, now()).await.unwrap();
        let (asking, _) = two_sessions(&db.pool, &alice.id).await;
        let stamp = alices_stamp(&db.pool).await;
        let mine = NewPassword::hash("amber quarry signal").await.unwrap();
        let pool = db.pool.clone();
        // Hold alice's row, so the reset and then the change queue on it in
        // that order.
        let mut holder = pool.begin().await.unwrap();
        sqlx::query("SELECT 1 FROM users WHERE id = $1 FOR NO KEY UPDATE")
            .bind(&alice.id)
            .execute(&mut *holder)
            .await
            .unwrap();
        // `tokio::spawn` runs each on its own task, like
        // `asyncio.create_task`; `move` hands each owned copies.
        let reset_task = tokio::spawn({
            let pool = pool.clone();
            let user_id = alice.id.clone();
            async move { reset(&pool, &user_id, "copper meadow lantern", now()).await }
        });
        let first_queued = tokio::time::timeout(
            std::time::Duration::from_secs(30),
            crate::http::api::testing::wait_for_lock_waiters(&pool, 1),
        )
        .await;
        if first_queued.is_err() {
            holder.rollback().await.unwrap();
            panic!("the reset never queued on alice's row within 30 s");
        }
        let change_task = tokio::spawn({
            let pool = pool.clone();
            let user_id = alice.id.clone();
            async move {
                change(
                    &pool,
                    &SessionChange {
                        user_id: &user_id,
                        session_id: &asking,
                        replaces: Some(&stamp),
                        password: &mine,
                        keep_sessions: true,
                        now: now(),
                    },
                )
                .await
            }
        });
        // Bounded, and the holder is released before any failure is
        // reported, so a broken lock order fails fast instead of hanging.
        let bound = std::time::Duration::from_secs(30);
        let queued = tokio::time::timeout(
            bound,
            crate::http::api::testing::wait_for_lock_waiters(&pool, 2),
        )
        .await;
        holder.commit().await.unwrap();
        queued.expect("the reset and the change to queue on alice's row within 30 s");
        let ended = tokio::time::timeout(bound, reset_task)
            .await
            .expect("the reset to finish within 30 s")
            .unwrap()
            .unwrap();
        let refused = tokio::time::timeout(bound, change_task)
            .await
            .expect("the change to finish within 30 s")
            .unwrap();

        assert_eq!(ended, 2, "the reset ended both sessions");
        assert!(
            matches!(refused, Err(OkfError::Unauthenticated { .. })),
            "{refused:?}"
        );
        let outcome = verify(
            &db.pool,
            "alice@example.com",
            "copper meadow lantern",
            now(),
        )
        .await
        .unwrap();
        assert!(matches!(outcome, SignInOutcome::Ok(_)), "the reset stands");
        assert_eq!(sessions_of(&db.pool, &alice.id).await, 0);
    }

    #[test]
    fn a_session_change_debug_shows_no_secret() {
        let password =
            NewPassword("$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2g".to_string());
        let stamp = PasswordStamp("$argon2id$old-hash".to_string());
        let change = SessionChange {
            user_id: "user-1",
            session_id: "raw-session-id-value",
            replaces: Some(&stamp),
            password: &password,
            keep_sessions: false,
            now: now(),
        };

        let shown = format!("{change:?}");

        assert!(shown.contains("user-1"), "{shown}");
        for secret in ["raw-session-id-value", "argon2id", "old-hash"] {
            assert!(!shown.contains(secret), "{shown}");
        }
    }
}

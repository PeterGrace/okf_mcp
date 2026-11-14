//! Users: the people credentials belong to (spec "Data model"). Creating a
//! user also creates its personal team, so every user can own catalogs.
//! Users are addressed by name; ids stay inside SQL.

use std::collections::HashMap;

use chrono::{DateTime, Utc};
use serde_json::json;
use sqlx::{PgConnection, PgPool};

use crate::error::OkfError;
use crate::store::audit::{self, Action, AuditActor, Entry, People, RowLock, Target};
use crate::store::catalogs::{bump_admin_generation, is_unique_violation, is_valid_name};
use crate::store::changesets::{self, ChangesetState};
use crate::store::user_sessions;

/// A user to create.
// `<'a>` is a lifetime parameter: the struct borrows its strings from the
// caller (`&'a str`) instead of owning copies, and cannot outlive them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewUser<'a> {
    /// `[a-z0-9-]+`; also its personal team's name.
    pub name: &'a str,
    /// Free text shown beside the name.
    pub display_name: &'a str,
}

/// A stored user.
// `sqlx::FromRow` maps a result row's columns onto the fields by name.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct UserRecord {
    /// Its name.
    pub name: String,
    /// Its display name (may be empty).
    pub display_name: String,
    /// When it was created (server clock).
    pub created_at: DateTime<Utc>,
    /// How many keys, grants (live or revoked) and sign-in codes name it.
    pub credentials: i64,
    /// Its teams' names, sorted, its personal team included.
    pub teams: Vec<String>,
    /// Its email address, as entered (compared case-insensitively), if set.
    pub email: Option<String>,
    /// Whether a signed-in session of this user acts as an admin.
    pub is_admin: bool,
    /// Whether it has a local password (the hash itself never leaves SQL).
    pub has_password: bool,
}

/// What signing in needs to know about a user: who it is and whether its
/// session is an admin's. Returned by every sign-in path. It holds no
/// secret, so the derived `Debug` is safe to log.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct UserAuth {
    /// `users.id` (a UUID as text), the user handle sessions and passkeys use.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Its email address, if set.
    pub email: Option<String>,
    /// Whether its session acts as an admin.
    pub is_admin: bool,
}

// One SELECT shared by `list` and `find` (a macro so `concat!` can append
// each query's tail at compile time; see `select_codes!` in codes.rs).
// `email::text`: `citext` decodes as a string too, but the cast keeps the
// column's type plain for every reader.
macro_rules! select_users {
    () => {
        "SELECT u.name, u.display_name, u.created_at, \
                (SELECT count(*) FROM api_keys k WHERE k.user_id = u.id) \
              + (SELECT count(*) FROM sign_in_codes s WHERE s.user_id = u.id) AS credentials, \
                COALESCE((SELECT array_agg(t.name ORDER BY t.name) FROM team_members m \
                          JOIN teams t ON t.id = m.team_id WHERE m.user_id = u.id), \
                         '{}'::text[]) AS teams, \
                u.email::text AS email, u.is_admin, \
                u.password_hash IS NOT NULL AS has_password \
         FROM users u"
    };
}

/// The columns of a [`UserAuth`], for a query over `users u`.
// `pub(crate) use` below exports the macro to the other store modules
// (`email_tokens`, `passwords`), which splice it into their own `concat!`.
macro_rules! user_auth_columns {
    () => {
        "u.id, u.name, u.email::text AS email, u.is_admin"
    };
}
pub(crate) use user_auth_columns;

/// Refuses a user or team name that is not `[a-z0-9-]+`.
///
/// # Arguments
/// * `noun` - `"user"` or `"team"`, for the message.
/// * `name` - the proposed name.
///
/// # Errors
/// [`OkfError::Refused`] for an invalid name.
pub(crate) fn check_name(noun: &str, name: &str) -> Result<(), OkfError> {
    if is_valid_name(name) {
        Ok(())
    } else {
        Err(OkfError::Refused {
            reason: format!("{noun} name {name:?} must be lowercase letters, digits and '-'"),
        })
    }
}

/// Refuses `name` if a user or a team already has it: a user's personal
/// team takes the user's name, so the two must not clash.
///
/// # Arguments
/// * `conn` - the caller's transaction (pass `&mut tx`).
/// * `name` - the proposed name.
///
/// # Errors
/// [`OkfError::Refused`] when taken; [`OkfError::Database`].
pub(crate) async fn check_free(conn: &mut PgConnection, name: &str) -> Result<(), OkfError> {
    let taken: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM users WHERE name = $1) \
             OR EXISTS (SELECT 1 FROM teams WHERE name = $1)",
    )
    .bind(name)
    .fetch_one(&mut *conn)
    .await?;
    if taken {
        return Err(name_taken(name));
    }
    Ok(())
}

/// The refusal for a name a user or team already has.
fn name_taken(name: &str) -> OkfError {
    OkfError::Refused {
        reason: format!("the name {name:?} is already taken by a user or team"),
    }
}

/// Maps a UNIQUE violation from inserting a user or team row to the
/// "name taken" refusal. `check_free` only narrows the race: two creates of
/// one name (a user and a team, or two of either) can both pass it, and then
/// the `users.name` or `teams.name` constraint decides inside the INSERT.
/// Every user and team insert goes through this, so the loser always gets
/// the 409, never a database error.
///
/// # Arguments
/// * `inserted` - the INSERT's result.
/// * `name` - the name being created.
///
/// # Returns
/// The INSERT's value, unchanged.
///
/// # Errors
/// [`OkfError::Refused`] for a unique violation; [`OkfError::Database`]
/// otherwise.
// Generic over `T`: an insert may return an id (`String`) or anything else.
pub(crate) fn or_name_taken<T>(
    inserted: Result<T, sqlx::Error>,
    name: &str,
) -> Result<T, OkfError> {
    inserted.map_err(|error| {
        if is_unique_violation(&error) {
            name_taken(name)
        } else {
            error.into()
        }
    })
}

/// Creates a user and its personal team (the user as owner), and bumps the
/// administrative generation, in one transaction.
///
/// # Arguments
/// * `pool` - the database.
/// * `new` - its name and display name.
/// * `actor` - who, for the `user.created` audit entry.
/// * `now` - the creation time (server clock).
///
/// # Returns
/// The new user.
///
/// # Errors
/// [`OkfError::Refused`] for an invalid name or one a user or team already
/// has (including one taken by a concurrent create); [`OkfError::Database`]
/// otherwise.
pub async fn create(
    pool: &PgPool,
    new: &NewUser<'_>,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<UserRecord, OkfError> {
    check_name("user", new.name)?;
    // A transaction rolls back when `tx` is dropped without `commit`, so
    // every early `?` return below undoes the rows inserted before it.
    let mut tx = pool.begin().await?;
    // The acting user's rows before anything else (`audit::lock_people`).
    audit::lock_people(&mut tx, actor, &People::default()).await?;
    check_free(&mut tx, new.name).await?;
    let inserted = sqlx::query_scalar::<_, String>(
        "INSERT INTO users (name, display_name, created_at) VALUES ($1, $2, $3) RETURNING id",
    )
    .bind(new.name)
    .bind(new.display_name)
    .bind(now)
    .fetch_one(&mut *tx)
    .await;
    let user_id = or_name_taken(inserted, new.name)?;
    // The personal team's insert can lose the race too: to a concurrent
    // `teams::create` of the same name, which no `users` constraint sees.
    let inserted = sqlx::query_scalar::<_, String>(
        "INSERT INTO teams (name, display_name, personal_of, created_at) \
         VALUES ($1, $2, $3, $4) RETURNING id",
    )
    .bind(new.name)
    .bind(new.display_name)
    .bind(&user_id)
    .bind(now)
    .fetch_one(&mut *tx)
    .await;
    let team_id = or_name_taken(inserted, new.name)?;
    sqlx::query(
        "INSERT INTO team_members (team_id, user_id, role, created_at) \
         VALUES ($1, $2, 'owner', $3)",
    )
    .bind(&team_id)
    .bind(&user_id)
    .bind(now)
    .execute(&mut *tx)
    .await?;
    bump_admin_generation(&mut tx).await?;
    audit::record(
        &mut tx,
        actor,
        &user_entry(Action::UserCreated, &user_id, json!({})),
        now,
    )
    .await?;
    // The personal team's audit row from birth, like the user's own (which
    // the entry above makes): later entries naming either only read them
    // (`migrations/0014_audit_row_locks.sql`).
    audit::team_ref(&mut tx, new.name).await?;
    tx.commit().await?;
    Ok(UserRecord {
        name: new.name.to_string(),
        display_name: new.display_name.to_string(),
        created_at: now,
        credentials: 0,
        teams: vec![new.name.to_string()],
        email: None,
        is_admin: false,
        has_password: false,
    })
}

/// Every user, by name.
///
/// # Arguments
/// * `pool` - the database.
///
/// # Returns
/// The users, sorted by name.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn list(pool: &PgPool) -> Result<Vec<UserRecord>, OkfError> {
    Ok(
        sqlx::query_as::<_, UserRecord>(concat!(select_users!(), " ORDER BY u.name"))
            .fetch_all(pool)
            .await?,
    )
}

/// The user called `name`.
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the user's name.
///
/// # Returns
/// The user.
///
/// # Errors
/// [`OkfError::UnknownName`] if there is none; [`OkfError::Database`].
pub async fn find(pool: &PgPool, name: &str) -> Result<UserRecord, OkfError> {
    // A malformed name (a NUL, which Postgres refuses) names nobody.
    if !is_valid_name(name) {
        return Err(unknown(name));
    }
    sqlx::query_as::<_, UserRecord>(concat!(select_users!(), " WHERE u.name = $1"))
        .bind(name)
        .fetch_optional(pool)
        .await?
        .ok_or_else(|| unknown(name))
}

/// The id of the user called `name`, inside a caller's transaction.
///
/// The row is held `FOR KEY SHARE` until that transaction ends, so a
/// concurrent [`delete`] (which locks the user `FOR UPDATE`) waits for the
/// caller and then sees what it wrote (a key or code naming the user); one
/// that got there first leaves no row, which is `UnknownName` here rather
/// than a foreign-key failure at the caller's write.
///
/// Lock order: call this before locking any team row. [`delete`] locks the
/// user and then its personal team, so a caller holding a team while it
/// waits here could deadlock with it (`teams::set_member` resolves the user
/// first for this reason).
///
/// # Arguments
/// * `conn` - the caller's transaction (pass `&mut tx`).
/// * `name` - the user's name.
///
/// # Returns
/// Its `users.id`.
///
/// # Errors
/// [`OkfError::UnknownName`]; [`OkfError::Database`].
pub(crate) async fn id_of(conn: &mut PgConnection, name: &str) -> Result<String, OkfError> {
    sqlx::query_scalar("SELECT id FROM users WHERE name = $1 FOR KEY SHARE")
        .bind(name)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| unknown(name))
}

/// The id of the user called `name`, without a lock: for callers that
/// pass it to functions keyed by user id (sessions, sign-in events,
/// ceilings) and need no consistency with a later write.
///
/// A name that is not `[a-z0-9-]+` (a NUL among others, which Postgres
/// refuses as a database error) names nobody and never reaches SQL.
///
/// # Arguments
/// * `executor` - a `&PgPool`, or `&mut *tx` to read inside a transaction.
/// * `name` - the user's name.
///
/// # Returns
/// Its `users.id`.
///
/// # Errors
/// [`OkfError::UnknownName`]; [`OkfError::Database`].
// `impl PgExecutor<'e>` accepts `&PgPool` or `&mut PgConnection`, so one
// function serves both, like `catalogs::find`.
pub async fn id_by_name<'e>(
    executor: impl sqlx::PgExecutor<'e>,
    name: &str,
) -> Result<String, OkfError> {
    if !is_valid_name(name) {
        return Err(unknown(name));
    }
    sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
        .bind(name)
        .fetch_optional(executor)
        .await?
        .ok_or_else(|| unknown(name))
}

/// Ends every console session of the user called `name` (an admin's "sign
/// out everywhere"), under the user's row lock, and records
/// `user.sessions_revoked` when any ended (none ended records nothing).
///
/// The lock (`FOR NO KEY UPDATE`) conflicts with the `FOR SHARE` that
/// `user_sessions::start` takes on the same row, so a session whose insert
/// is still uncommitted when this starts is waited for and then ended too;
/// one that starts later is a sign-in after the sign-out. Lock order: the
/// user's row (with the actor's, `audit::lock_people`), then its sessions
/// (`email_tokens`' module docs).
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the user's name.
/// * `actor` - who, for the audit log.
/// * `now` - the server clock.
///
/// # Returns
/// How many sessions ended.
///
/// # Errors
/// [`OkfError::UnknownName`]; [`OkfError::Database`].
pub async fn end_sessions(
    pool: &PgPool,
    name: &str,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<u64, OkfError> {
    let mut tx = pool.begin().await?;
    let (user_id, _) = lock_target(&mut tx, name, RowLock::NoKeyUpdate, actor).await?;
    // The session store owns its SQL; `&mut *tx` runs it in this transaction.
    let ended = user_sessions::revoke_all(&mut *tx, &user_id, None).await?;
    if ended > 0 {
        let details = json!({ "sessions": ended });
        audit::record(
            &mut tx,
            actor,
            &user_entry(Action::UserSessionsRevoked, &user_id, details),
            now,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(ended)
}

/// Ends one console session of the user called `name`, by its id prefix
/// (an admin's per-row revoke), and records `user.sessions_revoked`, in
/// one transaction under the user's row lock (as [`end_sessions`]: the
/// user's row, then its sessions). The user's own revoke from the account
/// page calls `user_sessions::revoke_by_prefix` and records nothing.
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the user's name.
/// * `id_prefix` - the session's `SessionInfo::id_prefix`.
/// * `actor` - who, for the audit log.
/// * `now` - the server clock.
///
/// # Errors
/// [`OkfError::UnknownName`] for an unknown user (noun `"user"`) or for a
/// malformed prefix or one matching none of the user's sessions (noun
/// `"session"`); [`OkfError::Refused`] for a prefix matching several;
/// [`OkfError::Database`].
pub async fn end_session_by_prefix(
    pool: &PgPool,
    name: &str,
    id_prefix: &str,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    let mut tx = pool.begin().await?;
    let (user_id, _) = lock_target(&mut tx, name, RowLock::NoKeyUpdate, actor).await?;
    user_sessions::revoke_by_prefix_in(&mut tx, &user_id, id_prefix).await?;
    let details = json!({ "sessions": 1 });
    audit::record(
        &mut tx,
        actor,
        &user_entry(Action::UserSessionsRevoked, &user_id, details),
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Locks a change's target user and every other user row it will lock
/// ([`audit::lock_people`] with a target): its id and admin flag.
///
/// # Arguments
/// * `conn` - the change's transaction.
/// * `name` - the target's name.
/// * `lock` - how the target is locked.
/// * `actor` - who acts.
///
/// # Returns
/// The target's id and admin flag, under its lock.
///
/// # Errors
/// As [`audit::lock_people`].
pub(crate) async fn lock_target(
    conn: &mut PgConnection,
    name: &str,
    lock: RowLock,
    actor: &AuditActor,
) -> Result<(String, bool), OkfError> {
    let people = People {
        target: Some((name, lock)),
        ..People::default()
    };
    // `lock_people` answers `Some` whenever it is given a target.
    audit::lock_people(conn, actor, &people)
        .await?
        .ok_or_else(|| unknown(name))
}

/// An audit entry about one user (plan Task 6a): the user is the target,
/// and the entry belongs to no team. A caller whose entry belongs to teams
/// (an entitlement) sets `teams` with struct-update syntax:
/// `Entry { teams: &teams, ..user_entry(..) }`.
///
/// # Arguments
/// * `action` - what happened.
/// * `user_id` - the user's `users.id`.
/// * `details` - a small JSON object; never an email address or a secret.
///
/// # Returns
/// The entry, to pass to [`audit::record`].
pub(crate) fn user_entry(action: Action, user_id: &str, details: serde_json::Value) -> Entry<'_> {
    Entry {
        action,
        target: Target::User(user_id),
        person: None,
        teams: &[],
        details,
    }
}

/// The error for a user name that names no user.
pub(crate) fn unknown(name: &str) -> OkfError {
    OkfError::UnknownName {
        noun: "user",
        name: name.to_string(),
    }
}

/// The longest email address accepted (RFC 5321's path limit).
const MAX_EMAIL_LENGTH: usize = 254;

/// Checks that `email` is one plain address, `local@domain`, with nothing a
/// mail header could read as a second recipient or a display name.
///
/// Deliberately loose: the only real test of an address is mail reaching
/// it. It refuses what would be dangerous in a `To` header (commas,
/// semicolons, angle brackets, quotes, whitespace, control characters).
///
/// # Arguments
/// * `email` - the proposed address.
///
/// # Errors
/// [`OkfError::LimitExceeded`] with `limit: "email"`.
pub(crate) fn check_email(email: &str) -> Result<(), OkfError> {
    let refuse = |reason: &str| OkfError::LimitExceeded {
        limit: "email",
        reason: format!("{email:?} is not an email address: {reason}"),
    };
    if email.len() > MAX_EMAIL_LENGTH {
        return Err(refuse("it is longer than 254 characters"));
    }
    // `split_once` splits at the first '@' only, like Python's
    // `str.partition`; `None` when there is no '@' at all.
    let Some((local, domain)) = email.split_once('@') else {
        return Err(refuse("it has no '@'"));
    };
    if local.is_empty() || domain.is_empty() || domain.contains('@') {
        return Err(refuse("it needs one '@' with text on both sides"));
    }
    let forbidden = |c: char| c.is_whitespace() || c.is_control() || "<>,;\"()[]\\".contains(c);
    if email.chars().any(forbidden) {
        return Err(refuse(
            "it contains a space, a control character or one of <>,;\"()[]\\",
        ));
    }
    Ok(())
}

/// The form of a typed email address that sign-in looks up: trimmed and
/// lowercased, or `None` when it cannot be anyone's address (it fails
/// [`check_email`]: too long, no single `@`, or a space, a control character
/// such as NUL, or a mail-header special). Callers treat `None` as an
/// address that names nobody, so it takes the same path as an unknown one.
///
/// # Arguments
/// * `raw` - the address as the form sent it.
///
/// # Returns
/// The normalized address, or `None`.
pub(crate) fn normalize_email(raw: &str) -> Option<String> {
    let email = raw.trim().to_lowercase();
    is_lookup_email(&email).then_some(email)
}

/// Whether `email` may be bound in a lookup: what [`check_email`] accepts.
/// Store functions that take a typed address check it, so text Postgres
/// would refuse (a NUL) is an ordinary "no such user", never a database
/// error.
pub(crate) fn is_lookup_email(email: &str) -> bool {
    check_email(email).is_ok()
}

/// Sets or clears a user's email address. A changed address also deletes
/// the user's outstanding emailed links, so a link sent to the old address
/// stops working, bumps the administrative generation, so the key cache
/// (which console sessions read the email from) reloads, and records
/// `user.email_changed` (whether it was cleared, never the address). A
/// change of letter case only is not a change: it is stored, and records
/// nothing.
///
/// The user's row is locked (`FOR UPDATE`, which `set_admin` also takes)
/// with the actor's rows before anything else (`audit::lock_people`), so a
/// provisioner's refusal of an admin user cannot race a promotion.
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the user's name.
/// * `email` - the new address, or `None` to clear it.
/// * `actor` - who, for the audit log; a provisioner is refused an admin.
/// * `now` - the server clock.
///
/// # Errors
/// [`OkfError::LimitExceeded`] for a malformed address; [`OkfError::Refused`]
/// when another user has it (in any letter case); [`OkfError::Forbidden`]
/// for a provisioner and an admin user; [`OkfError::UnknownName`];
/// [`OkfError::Database`].
pub async fn set_email(
    pool: &PgPool,
    name: &str,
    email: Option<&str>,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    // `Option::map(check_email)` runs the check only for `Some`;
    // `transpose` turns `Option<Result<..>>` into `Result<Option<..>>` for `?`.
    email.map(check_email).transpose()?;
    // A malformed name (a NUL, which Postgres refuses) names nobody.
    if !is_valid_name(name) {
        return Err(unknown(name));
    }
    let mut tx = pool.begin().await?;
    let (user_id, is_admin) = lock_target(&mut tx, name, RowLock::Update, actor).await?;
    audit::refuse_admin_target(actor, is_admin, name)?;
    // The subquery reads the old value (the row is already locked), so
    // `changed` compares it with the new one in the same statement. Both
    // sides are `citext`, so `IS DISTINCT FROM` ignores letter case.
    let updated = sqlx::query_scalar::<_, bool>(
        "UPDATE users u SET email = $2::citext \
         FROM (SELECT id, email FROM users WHERE id = $1) old \
         WHERE u.id = old.id \
         RETURNING old.email IS DISTINCT FROM u.email",
    )
    .bind(&user_id)
    .bind(email)
    .fetch_one(&mut *tx)
    .await;
    let changed = match updated {
        Ok(changed) => changed,
        // `users.email` is the only UNIQUE column this statement writes.
        Err(error) if is_unique_violation(&error) => {
            return Err(OkfError::Refused {
                reason: "Another user has this email".to_string(),
            })
        }
        Err(error) => return Err(error.into()),
    };
    if changed {
        sqlx::query("DELETE FROM email_tokens WHERE user_id = $1")
            .bind(&user_id)
            .execute(&mut *tx)
            .await?;
        // The key cache holds every user's email for console sessions
        // (`auth_by_id`), so a change reloads it.
        bump_admin_generation(&mut tx).await?;
        // Never the address: only that it changed (spec "People in entries").
        let details = json!({ "cleared": email.is_none() });
        audit::record(
            &mut tx,
            actor,
            &user_entry(Action::UserEmailChanged, &user_id, details),
            now,
        )
        .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// What [`set_admin`] is asked to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdminChange {
    /// The new flag.
    pub on: bool,
    /// Lets the last admin user be cleared (the CLI, which has operator
    /// access; Decision 16). The API passes `false`.
    pub allow_last: bool,
}

/// Turns a user's admin flag on or off. A change bumps the administrative
/// generation (a session's level depends on it), ends the user's console
/// sessions, so its next request signs in again with the new flag
/// (Decision 20), and records `user.admin_changed`. No change records
/// nothing.
///
/// This is the only code that writes `users.is_admin` (a new user starts
/// with `false`), so every route that changes the flag ends the sessions.
///
/// Every admin row is locked first, in id order, then the user's row and
/// the actor's (`audit::lock_people`), then `server_state`: two
/// concurrent "off" calls on the last two admins serialize, and the second
/// sees only one admin left. (A concurrent "on" for another user is not
/// counted until it commits, which only ever refuses more, never less.)
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the user's name.
/// * `change` - the new flag, and whether the last admin may be cleared.
/// * `actor` - who, for the audit log.
/// * `now` - the server clock.
///
/// # Returns
/// The flag before the call.
///
/// # Errors
/// [`OkfError::UnknownName`]; [`OkfError::Refused`] when clearing the last
/// admin user without `allow_last`; [`OkfError::Database`].
pub async fn set_admin(
    pool: &PgPool,
    name: &str,
    change: AdminChange,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<bool, OkfError> {
    // Destructuring copies both fields out (`AdminChange` is `Copy`).
    let AdminChange { on, allow_last } = change;
    // A malformed name (a NUL, which Postgres refuses) names nobody.
    if !is_valid_name(name) {
        return Err(unknown(name));
    }
    let mut tx = pool.begin().await?;
    // A waiter re-reads a row another transaction changed before taking its
    // lock (Postgres' read-committed rule), so an admin cleared concurrently
    // drops out of this list instead of being counted.
    let admins: Vec<String> =
        sqlx::query_scalar("SELECT id FROM users WHERE is_admin ORDER BY id FOR UPDATE")
            .fetch_all(&mut *tx)
            .await?;
    // Then the user and the actor's rows, in the same order (the admins
    // among them are held already).
    let (user_id, previous) = lock_target(&mut tx, name, RowLock::Update, actor).await?;
    if previous == on {
        // Nothing to write; dropping `tx` rolls back and releases the locks.
        return Ok(previous);
    }
    if !on && !allow_last && admins.len() <= 1 {
        return Err(OkfError::Refused {
            reason: format!("{name:?} is the last admin user; make another user an admin first"),
        });
    }
    sqlx::query("UPDATE users SET is_admin = $2 WHERE id = $1")
        .bind(&user_id)
        .bind(on)
        .execute(&mut *tx)
        .await?;
    // The session store owns its SQL; `&mut *tx` runs it in this transaction.
    user_sessions::revoke_all(&mut *tx, &user_id, None).await?;
    bump_admin_generation(&mut tx).await?;
    let details = json!({ "from": previous, "to": on });
    audit::record(
        &mut tx,
        actor,
        &user_entry(Action::UserAdminChanged, &user_id, details),
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(previous)
}

/// The user whose email is `email`, compared case-insensitively.
///
/// # Arguments
/// * `pool` - the database.
/// * `email` - the address as typed.
///
/// # Returns
/// The user, or `None`.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn find_by_email(pool: &PgPool, email: &str) -> Result<Option<UserAuth>, OkfError> {
    // No user has an address `check_email` refuses, and such text (a NUL,
    // which Postgres rejects as a database error) never reaches SQL.
    if !is_lookup_email(email) {
        return Ok(None);
    }
    // `$1::citext`: without the cast, `citext = text` would compare as text,
    // case-sensitively.
    Ok(sqlx::query_as::<_, UserAuth>(concat!(
        "SELECT ",
        user_auth_columns!(),
        " FROM users u WHERE u.email = $1::citext"
    ))
    .bind(email)
    .fetch_optional(pool)
    .await?)
}

/// The user whose email is `email`, compared case-insensitively, as the
/// user list shows it. Backs `GET /api/v1/users?email=` and
/// `okf-mcp user list --email`.
///
/// # Arguments
/// * `pool` - the database.
/// * `email` - the address as typed (surrounding whitespace is ignored).
///
/// # Returns
/// The user, or `None` (emails are unique in any letter case, so at most
/// one). Text that cannot be an address is `None` too.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn find_record_by_email(
    pool: &PgPool,
    email: &str,
) -> Result<Option<UserRecord>, OkfError> {
    // As in `find_by_email`: text no address can be (a NUL, which Postgres
    // refuses) never reaches SQL, so it answers like an unknown address.
    let email = email.trim();
    if !is_lookup_email(email) {
        return Ok(None);
    }
    // `$1::citext` makes the comparison case-insensitive (see `find_by_email`).
    Ok(
        sqlx::query_as::<_, UserRecord>(concat!(select_users!(), " WHERE u.email = $1::citext"))
            .bind(email)
            .fetch_optional(pool)
            .await?,
    )
}

/// The user with id `user_id`.
///
/// # Arguments
/// * `pool` - the database.
/// * `user_id` - a `users.id`.
///
/// # Returns
/// The user, or `None`.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn find_auth(pool: &PgPool, user_id: &str) -> Result<Option<UserAuth>, OkfError> {
    Ok(sqlx::query_as::<_, UserAuth>(concat!(
        "SELECT ",
        user_auth_columns!(),
        " FROM users u WHERE u.id = $1"
    ))
    .bind(user_id)
    .fetch_optional(pool)
    .await?)
}

/// Every user's sign-in identity, by id, for the key cache: console
/// sessions are built from it without a query.
///
/// # Arguments
/// * `pool` - the database.
///
/// # Returns
/// The users, keyed by id.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn auth_by_id(pool: &PgPool) -> Result<HashMap<String, UserAuth>, OkfError> {
    let users =
        sqlx::query_as::<_, UserAuth>(concat!("SELECT ", user_auth_columns!(), " FROM users u"))
            .fetch_all(pool)
            .await?;
    // `(user.id.clone(), user)`: the map owns a copy of the id as its key.
    Ok(users
        .into_iter()
        .map(|user| (user.id.clone(), user))
        .collect())
}

/// What a user deletion removed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct DeletedUser {
    /// Keys and OAuth grants, revoked ones included.
    pub keys: i64,
    /// Sign-in codes, in every state.
    pub codes: i64,
    /// Open changesets of those keys and grants, discarded.
    pub discarded: i64,
}

/// The shared teams whose only owner is `$1`, by name.
const SOLE_OWNED_TEAMS: &str = "\
    SELECT t.name FROM team_members m JOIN teams t ON t.id = m.team_id \
    WHERE m.user_id = $1 AND m.role = 'owner' AND t.personal_of IS NULL \
      AND NOT EXISTS (SELECT 1 FROM team_members o \
                      WHERE o.team_id = t.id AND o.role = 'owner' AND o.user_id <> $1) \
    ORDER BY t.name";

/// Deletes a user and everything that is only theirs, in one transaction
/// (spec Part 3): its keys, grants and sign-in codes (revoked ones too),
/// sessions, sign-in methods, emailed links, entitlements, memberships and
/// personal team. Its keys' open changesets are discarded and audited.
/// Catalogs, their history (whose `actor` text stays) and every audit entry
/// are kept; the user's `audit_people` row becomes `deleted user #N`.
///
/// Lock order: every admin row in id order, then the user (as
/// [`set_admin`]), then its personal team, then its shared teams in id
/// order, then its keys and grants in id order, then their open changesets
/// in id order. A key or code being created for the user holds the user
/// `FOR KEY SHARE` (`id_of`); the deletion waits for it and then deletes
/// it too. A changeset being opened by one of its keys waits for the key's
/// lock and then finds the key gone. A publish by one of its keys that is
/// already running when the deletion starts may make Postgres abort one of
/// the two as a deadlock ([`OkfError::Busy`], retryable): the publish holds
/// its changeset and records its entry under the user's `FOR KEY SHARE`.
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the user's name.
/// * `allow_last_admin` - lets the last admin user go (the CLI, with
///   operator access; never the API).
/// * `actor` - who, for the audit log; a provisioner is refused an admin.
/// * `now` - the server clock.
///
/// # Returns
/// What went.
///
/// # Errors
/// [`OkfError::UnknownName`] (a malformed name included);
/// [`OkfError::Forbidden`] for a provisioner and an admin user;
/// [`OkfError::Refused`] naming the catalogs its personal team owns, the
/// shared teams it is the only owner of, or that it is the last admin user;
/// [`OkfError::Busy`] for a deadlock; [`OkfError::Database`].
pub async fn delete(
    pool: &PgPool,
    name: &str,
    allow_last_admin: bool,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<DeletedUser, OkfError> {
    // A malformed name (a NUL, which Postgres refuses) names nobody.
    if !is_valid_name(name) {
        return Err(unknown(name));
    }
    // Dropping `tx` without `commit()` (any `?` or early return) rolls back.
    let mut tx = pool.begin().await?;
    // As in `set_admin`: an admin cleared concurrently drops out of the list
    // before it is counted.
    let admins: Vec<String> =
        sqlx::query_scalar("SELECT id FROM users WHERE is_admin ORDER BY id FOR UPDATE")
            .fetch_all(&mut *tx)
            .await?;
    // `FOR UPDATE` holds the row, so a credential or membership created
    // concurrently (its foreign-key check takes `FOR KEY SHARE`) waits.
    let (user_id, is_admin): (String, bool) =
        sqlx::query_as("SELECT id, is_admin FROM users WHERE name = $1 FOR UPDATE")
            .bind(name)
            .fetch_optional(&mut *tx)
            .await?
            .ok_or_else(|| unknown(name))?;
    audit::refuse_admin_target(actor, is_admin, name)?;
    // The personal team: a catalog assigned to it concurrently takes
    // `FOR KEY SHARE` on this row, so either it waits for this delete (and
    // then fails its foreign-key check), or this waits for it and the
    // listing below sees the catalog. (`Option`: every user has one, but a
    // missing row should not panic.)
    let team_id: Option<String> =
        sqlx::query_scalar("SELECT id FROM teams WHERE personal_of = $1 FOR UPDATE")
            .bind(&user_id)
            .fetch_optional(&mut *tx)
            .await?;
    // Its shared teams, so no owner is added or removed while we count.
    sqlx::query(
        "SELECT t.id FROM teams t JOIN team_members m ON m.team_id = t.id \
         WHERE m.user_id = $1 AND t.personal_of IS NULL ORDER BY t.id FOR UPDATE OF t",
    )
    .bind(&user_id)
    .execute(&mut *tx)
    .await?;
    let owned: Vec<String> =
        sqlx::query_scalar("SELECT name FROM catalogs WHERE owner_team_id = $1 ORDER BY name")
            .bind(&team_id)
            .fetch_all(&mut *tx)
            .await?;
    let sole: Vec<String> = sqlx::query_scalar(SOLE_OWNED_TEAMS)
        .bind(&user_id)
        .fetch_all(&mut *tx)
        .await?;
    let blocker = if !owned.is_empty() {
        Some(format!(
            "its personal team owns catalog(s) {}; delete them or move them to another team \
             first (catalog set-owner)",
            quoted(&owned)
        ))
    } else if !sole.is_empty() {
        Some(format!(
            "it is the last owner of team(s) {}; add another owner first",
            quoted(&sole)
        ))
    } else if is_admin && !allow_last_admin && admins.len() <= 1 {
        Some("it is the last admin user; make another user an admin first".to_string())
    } else {
        None
    };
    if let Some(what) = blocker {
        return Err(OkfError::Refused {
            reason: format!("user {name:?} cannot be deleted: {what}"),
        });
    }
    // Its keys and grants, so none opens a changeset after the discard
    // below (opening one takes `FOR KEY SHARE` on its key).
    let keys: Vec<String> =
        sqlx::query_scalar("SELECT id FROM api_keys WHERE user_id = $1 ORDER BY id FOR UPDATE")
            .bind(&user_id)
            .fetch_all(&mut *tx)
            .await?;
    let codes: i64 = sqlx::query_scalar("SELECT count(*) FROM sign_in_codes WHERE user_id = $1")
        .bind(&user_id)
        .fetch_one(&mut *tx)
        .await?;
    let discarded = discard_open_changesets(&mut tx, &user_id, actor, now).await?;
    let deleted = DeletedUser {
        keys: count_of(keys.len()),
        codes,
        discarded,
    };
    // Recorded while the user row exists, so the entry's person resolves.
    let details = json!({
        "keys": deleted.keys,
        "codes": deleted.codes,
        "changesets_discarded": deleted.discarded,
    });
    audit::record(
        &mut tx,
        actor,
        &user_entry(Action::UserDeleted, &user_id, details),
        now,
    )
    .await?;
    // The team row goes before the user row, because `teams.personal_of`
    // restricts the user's delete; its memberships cascade with it.
    if let Some(team_id) = &team_id {
        sqlx::query("DELETE FROM teams WHERE id = $1")
            .bind(team_id)
            .execute(&mut *tx)
            .await?;
    }
    // Before the user row goes: it finds the user's credentials through
    // `api_keys` and `sign_in_codes`, which that delete cascades.
    audit::forget_person(&mut tx, &user_id).await?;
    // Cascades: keys (and their grants and tokens), codes, memberships,
    // sessions, passkeys, identities, emailed links, entitlements, events.
    sqlx::query("DELETE FROM users WHERE id = $1")
        .bind(&user_id)
        .execute(&mut *tx)
        .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(deleted)
}

/// Names for a refusal, each quoted: `"a", "b"`.
fn quoted(names: &[String]) -> String {
    // `{:?}` of a string quotes (and escapes) it.
    names
        .iter()
        .map(|name| format!("{name:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

/// A row count as the `i64` the summary reports.
fn count_of(rows: usize) -> i64 {
    // `try_from`: a `usize` into an `i64` cannot fail for any real count.
    i64::try_from(rows).unwrap_or(i64::MAX)
}

/// Discards the open changesets of `user_id`'s keys and grants (else their
/// `key_id` would become NULL and leave them open with no owner), with one
/// `catalog.changeset_discarded` entry each. Like `changesets::prune_empty`
/// it runs without the catalog lock, so it cannot deadlock with
/// `catalogs::delete` (catalog, then changesets); the changeset row lock is
/// what a publish or discard of the same changeset waits on. A cached
/// overlay of a discarded changeset ages out of the cache on its own.
///
/// # Arguments
/// * `conn` - the deletion's transaction, holding the user's keys.
/// * `user_id` - the user being deleted.
/// * `actor` - who, for the entries.
/// * `now` - the server clock.
///
/// # Returns
/// How many were discarded.
///
/// # Errors
/// [`OkfError::Database`].
async fn discard_open_changesets(
    conn: &mut PgConnection,
    user_id: &str,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<i64, OkfError> {
    // Each row: id, handle, catalog id, catalog name, entry count.
    let open: Vec<(i64, String, i64, String, i64)> = sqlx::query_as(
        "SELECT c.id, c.handle, c.catalog_id, cat.name, \
                (SELECT count(*) FROM changeset_entries e WHERE e.changeset_id = c.id) \
         FROM changesets c JOIN api_keys k ON k.id = c.key_id \
                           JOIN catalogs cat ON cat.id = c.catalog_id \
         WHERE k.user_id = $1 AND c.state = 'open' ORDER BY c.id FOR UPDATE OF c",
    )
    .bind(user_id)
    .fetch_all(&mut *conn)
    .await?;
    for (id, handle, catalog_id, catalog, files) in &open {
        changesets::set_state(conn, *id, ChangesetState::Discarded, now).await?;
        let teams = audit::catalog_teams(conn, *catalog_id).await?;
        audit::record(
            conn,
            actor,
            &Entry {
                action: Action::CatalogChangesetDiscarded,
                target: Target::Catalog(catalog),
                person: Some(user_id),
                teams: &teams,
                details: serde_json::json!({
                    "changeset": handle,
                    "files": files,
                    "reason": "user_deleted",
                }),
            },
            now,
        )
        .await?;
    }
    Ok(count_of(open.len()))
}

#[cfg(test)]
// `pub(crate)` so `teams::tests` shares the seeding helpers below.
pub(crate) mod tests {
    use super::*;
    use crate::db::test_db;
    use crate::store::catalogs::{self, tests::until_a_lock_is_awaited};
    use crate::store::ceiling::NewEntitlement;
    use crate::store::keys::{self as key_rows, Access, Grants, NewKey};
    use crate::store::teams;

    /// How long a race test waits for a held lock to be released.
    const LOCK_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

    /// Gives `team` a catalog, `shared`.
    pub(crate) async fn seed_catalog_owned_by(pool: &PgPool, team: &str) {
        catalogs::create(
            pool,
            &crate::store::catalogs::NewCatalog {
                name: "shared",
                description: "",
                owner_team: team,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
    }

    /// Makes catalog `shared` belong to the team named `$1`, by writing
    /// `catalogs.owner_team_id` inside a transaction the race test holds open.
    const OWN_SHARED: &str = "UPDATE catalogs SET owner_team_id = \
                              (SELECT id FROM teams WHERE name = $1) WHERE name = 'shared'";

    /// Asserts a refused user delete changed nothing: the user is still
    /// there, still names `credentials` credentials, and the generation is
    /// still `generation`.
    async fn assert_unchanged(pool: &PgPool, credentials: i64, generation: i64) {
        let user = find(pool, "alice").await.unwrap();
        assert_eq!(user.credentials, credentials);
        assert_eq!(user.teams, ["alice"]);
        assert_eq!(catalogs::admin_generation(pool).await.unwrap(), generation);
    }

    /// Creates user `name` with `email` (if any) and returns its
    /// [`UserAuth`]. Shared by the passwords, emailed-link, sign-in event
    /// and console-session tests.
    pub(crate) async fn seed_user(pool: &PgPool, name: &str, email: Option<&str>) -> UserAuth {
        create(
            pool,
            &NewUser {
                name,
                display_name: "",
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        set_email(pool, name, email, &AuditActor::cli(), now())
            .await
            .unwrap();
        let id: String = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(name)
            .fetch_one(pool)
            .await
            .unwrap();
        find_auth(pool, &id).await.unwrap().unwrap()
    }

    /// Gives user `user_id` one console session.
    async fn seed_session(pool: &PgPool, user_id: &str) {
        user_sessions::create(
            pool,
            user_id,
            None,
            "",
            std::time::Duration::from_secs(86_400),
            now(),
        )
        .await
        .unwrap();
    }

    /// How many console sessions exist.
    async fn session_count(pool: &PgPool) -> i64 {
        sqlx::query_scalar("SELECT count(*) FROM user_sessions")
            .fetch_one(pool)
            .await
            .unwrap()
    }

    fn now() -> DateTime<Utc> {
        // A fixed instant: tests never read the wall clock.
        DateTime::parse_from_rfc3339("2026-11-11T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn alice() -> NewUser<'static> {
        NewUser {
            name: "alice",
            display_name: "Alice Example",
        }
    }

    fn family_owned_by_alice() -> teams::NewTeam<'static> {
        teams::NewTeam {
            name: "family",
            display_name: "",
            owner: "alice",
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_new_user_has_no_email_no_password_and_is_not_an_admin() {
        let db = test_db().await;

        let user = create(&db.pool, &alice(), &AuditActor::cli(), now())
            .await
            .unwrap();

        assert_eq!(user.email, None);
        assert!(!user.is_admin);
        assert!(!user.has_password);
        assert_eq!(find(&db.pool, "alice").await.unwrap(), user);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_email_stores_clears_and_is_found_case_insensitively() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", Some("Alice@Example.com")).await;

        let found = find_by_email(&db.pool, "alice@example.COM").await.unwrap();
        set_email(&db.pool, "alice", None, &AuditActor::cli(), now())
            .await
            .unwrap();
        let cleared = find(&db.pool, "alice").await.unwrap();

        assert_eq!(found, Some(alice.clone()));
        assert_eq!(alice.email.as_deref(), Some("Alice@Example.com"));
        assert_eq!(cleared.email, None);
        assert_eq!(
            find_by_email(&db.pool, "alice@example.com").await.unwrap(),
            None
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn find_record_by_email_ignores_letter_case_and_finds_none_for_others() {
        let db = test_db().await;
        seed_user(&db.pool, "alice", Some("Alice@Example.com")).await;
        seed_user(&db.pool, "bob", Some("robert@example.com")).await;

        let found = find_record_by_email(&db.pool, " alice@EXAMPLE.com ")
            .await
            .unwrap();
        let none = find_record_by_email(&db.pool, "bob@example.com")
            .await
            .unwrap();
        let malformed = find_record_by_email(&db.pool, "not an address\0")
            .await
            .unwrap();

        assert_eq!(found, Some(find(&db.pool, "alice").await.unwrap()));
        assert_eq!(none, None);
        assert_eq!(malformed, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_email_change_bumps_the_generation_and_a_no_op_does_not() {
        let db = test_db().await;
        seed_user(&db.pool, "alice", Some("alice@example.com")).await;
        let generation = || catalogs::admin_generation(&db.pool);
        let start = generation().await.unwrap();

        set_email(
            &db.pool,
            "alice",
            Some("ALICE@example.com"),
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let after_case_only = generation().await.unwrap();
        set_email(
            &db.pool,
            "alice",
            Some("alice@example.org"),
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let after_change = generation().await.unwrap();

        assert_eq!(after_case_only, start, "a case-only change is no change");
        assert_eq!(after_change, start + 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn auth_by_id_holds_every_user_by_id() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", Some("alice@example.com")).await;
        set_admin(
            &db.pool,
            "alice",
            AdminChange {
                on: true,
                allow_last: false,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();

        let users = auth_by_id(&db.pool).await.unwrap();

        let stored = find_by_email(&db.pool, "alice@example.com")
            .await
            .unwrap()
            .expect("alice");
        assert_eq!(users.get(&stored.id), Some(&stored));
        assert!(stored.is_admin);
        assert_eq!(stored.name, alice.name);
        assert!(
            users.values().any(|user| user.name == crate::db::TEST_USER),
            "every user, not only those with an email"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_duplicate_email_in_any_case_is_refused() {
        let db = test_db().await;
        seed_user(&db.pool, "alice", Some("alice@example.com")).await;
        seed_user(&db.pool, "bob", None).await;

        let clash = set_email(
            &db.pool,
            "bob",
            Some("ALICE@example.com"),
            &AuditActor::cli(),
            now(),
        )
        .await;

        assert!(
            matches!(&clash, Err(OkfError::Refused { reason }) if reason == "Another user has this email"),
            "{clash:?}"
        );
        assert_eq!(find(&db.pool, "bob").await.unwrap().email, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_malformed_email_is_refused_and_an_unknown_user_is_unknown_name() {
        let db = test_db().await;
        seed_user(&db.pool, "alice", None).await;
        let too_long = format!("{}@example.com", "a".repeat(MAX_EMAIL_LENGTH));

        let malformed: Vec<_> = [
            "",
            "alice",
            "@example.com",
            "alice@",
            "alice@@example.com",
            "a b@example.com",
            "alice@example.com, bob@example.com",
            "<alice@example.com>",
            "alice@example.com\r\nBcc: bob@example.com",
            too_long.as_str(),
        ]
        .into_iter()
        .map(check_email)
        .collect();
        let unknown = set_email(
            &db.pool,
            "nobody",
            Some("nobody@example.com"),
            &AuditActor::cli(),
            now(),
        )
        .await;
        let stored = set_email(
            &db.pool,
            "alice",
            Some("a b@example.com"),
            &AuditActor::cli(),
            now(),
        )
        .await;

        for result in malformed {
            assert!(
                matches!(result, Err(OkfError::LimitExceeded { limit: "email", .. })),
                "{result:?}"
            );
        }
        assert!(check_email("alice+okf@example.com").is_ok());
        assert!(matches!(
            unknown,
            Err(OkfError::UnknownName { noun: "user", .. })
        ));
        assert!(matches!(
            stored,
            Err(OkfError::LimitExceeded { limit: "email", .. })
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_admin_returns_the_previous_flag_and_bumps_only_on_a_change() {
        let db = test_db().await;
        seed_user(&db.pool, "alice", None).await;
        let start = catalogs::admin_generation(&db.pool).await.unwrap();

        let was = set_admin(
            &db.pool,
            "alice",
            AdminChange {
                on: true,
                allow_last: false,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let again = set_admin(
            &db.pool,
            "alice",
            AdminChange {
                on: true,
                allow_last: false,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let unknown = set_admin(
            &db.pool,
            "nobody",
            AdminChange {
                on: true,
                allow_last: false,
            },
            &AuditActor::cli(),
            now(),
        )
        .await;

        assert!(!was);
        assert!(again);
        assert!(find(&db.pool, "alice").await.unwrap().is_admin);
        assert_eq!(
            catalogs::admin_generation(&db.pool).await.unwrap(),
            start + 1
        );
        assert!(matches!(
            unknown,
            Err(OkfError::UnknownName { noun: "user", .. })
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_last_admin_user_is_kept_unless_allowed() {
        let db = test_db().await;
        seed_user(&db.pool, "alice", None).await;
        set_admin(
            &db.pool,
            "alice",
            AdminChange {
                on: true,
                allow_last: false,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();

        let refused = set_admin(
            &db.pool,
            "alice",
            AdminChange {
                on: false,
                allow_last: false,
            },
            &AuditActor::cli(),
            now(),
        )
        .await;
        let allowed = set_admin(
            &db.pool,
            "alice",
            AdminChange {
                on: false,
                allow_last: true,
            },
            &AuditActor::cli(),
            now(),
        )
        .await;

        assert!(
            matches!(refused, Err(OkfError::Refused { .. })),
            "{refused:?}"
        );
        assert!(allowed.unwrap());
        assert!(!find(&db.pool, "alice").await.unwrap().is_admin);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn two_admins_cleared_at_once_leave_one() {
        let db = test_db().await;
        seed_user(&db.pool, "alice", None).await;
        seed_user(&db.pool, "bob", None).await;
        set_admin(
            &db.pool,
            "alice",
            AdminChange {
                on: true,
                allow_last: false,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        set_admin(
            &db.pool,
            "bob",
            AdminChange {
                on: true,
                allow_last: false,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();

        // Both run at once (`tokio::spawn`, on the multi-threaded runtime):
        // the admin rows are locked in one order, so the second sees the
        // first's change and refuses.
        let clear = |name: &'static str| {
            let pool = db.pool.clone();
            tokio::spawn(async move {
                set_admin(
                    &pool,
                    name,
                    AdminChange {
                        on: false,
                        allow_last: false,
                    },
                    &AuditActor::cli(),
                    now(),
                )
                .await
            })
        };
        let (alice, bob) = (clear("alice"), clear("bob"));
        let (alice, bob) = (alice.await.unwrap(), bob.await.unwrap());

        assert_eq!(
            [alice.is_ok(), bob.is_ok()]
                .iter()
                .filter(|ok| **ok)
                .count(),
            1,
            "{alice:?} {bob:?}"
        );
        let admins: i64 = sqlx::query_scalar("SELECT count(*) FROM users WHERE is_admin")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(admins, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_admin_cleared_while_another_clear_waits_is_not_counted() {
        // Deterministic form of the race above: a transaction has cleared
        // alice and holds its commit while bob's clear waits on the lock.
        let db = test_db().await;
        seed_user(&db.pool, "alice", None).await;
        seed_user(&db.pool, "bob", None).await;
        set_admin(
            &db.pool,
            "alice",
            AdminChange {
                on: true,
                allow_last: false,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        set_admin(
            &db.pool,
            "bob",
            AdminChange {
                on: true,
                allow_last: false,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let mut clearing = db.pool.begin().await.unwrap();
        sqlx::query("UPDATE users SET is_admin = false WHERE name = 'alice'")
            .execute(&mut *clearing)
            .await
            .unwrap();
        let pool = db.pool.clone();
        let waiting = tokio::spawn(async move {
            set_admin(
                &pool,
                "bob",
                AdminChange {
                    on: false,
                    allow_last: false,
                },
                &AuditActor::cli(),
                now(),
            )
            .await
        });
        until_a_lock_is_awaited(&db.pool).await;

        clearing.commit().await.unwrap();
        let outcome = waiting.await.unwrap();

        assert!(
            matches!(outcome, Err(OkfError::Refused { .. })),
            "{outcome:?}"
        );
        assert!(find(&db.pool, "bob").await.unwrap().is_admin);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn changing_the_admin_flag_ends_the_users_console_sessions() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        seed_session(&db.pool, &alice.id).await;

        set_admin(
            &db.pool,
            "alice",
            AdminChange {
                on: false,
                allow_last: false,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let after_no_change = session_count(&db.pool).await;
        set_admin(
            &db.pool,
            "alice",
            AdminChange {
                on: true,
                allow_last: false,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();

        assert_eq!(after_no_change, 1, "no change keeps the session");
        assert_eq!(session_count(&db.pool).await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn id_by_name_finds_a_user_and_names_an_unknown_one() {
        let db = test_db().await;

        let id = id_by_name(&db.pool, crate::db::TEST_USER).await;
        let unknown = id_by_name(&db.pool, "nobody").await;
        let malformed = id_by_name(&db.pool, "a\0b").await;

        assert!(id.is_ok_and(|id| !id.is_empty()));
        assert!(
            matches!(unknown, Err(OkfError::UnknownName { noun: "user", .. })),
            "{unknown:?}"
        );
        assert!(
            matches!(malformed, Err(OkfError::UnknownName { noun: "user", .. })),
            "a NUL never reaches SQL: {malformed:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_malformed_name_is_an_unknown_user_for_every_write() {
        let db = test_db().await;

        let admin = set_admin(
            &db.pool,
            "a\0b",
            AdminChange {
                on: true,
                allow_last: false,
            },
            &AuditActor::cli(),
            now(),
        )
        .await;
        let email = set_email(
            &db.pool,
            "a\0b",
            Some("a@example.com"),
            &AuditActor::cli(),
            now(),
        )
        .await;
        let sessions = end_sessions(&db.pool, "a\0b", &AuditActor::cli(), now()).await;

        for outcome in [admin.map(|_| ()), email, sessions.map(|_| ())] {
            assert!(
                matches!(outcome, Err(OkfError::UnknownName { noun: "user", .. })),
                "{outcome:?}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn end_sessions_ends_every_session_of_one_user() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        let bob = seed_user(&db.pool, "bob", None).await;
        seed_session(&db.pool, &alice.id).await;
        seed_session(&db.pool, &alice.id).await;
        seed_session(&db.pool, &bob.id).await;

        let ended = end_sessions(&db.pool, "alice", &AuditActor::cli(), now())
            .await
            .unwrap();
        let unknown = end_sessions(&db.pool, "nobody", &AuditActor::cli(), now()).await;

        assert_eq!(ended, 2);
        assert_eq!(session_count(&db.pool).await, 1, "bob's session stays");
        assert!(
            matches!(unknown, Err(OkfError::UnknownName { .. })),
            "{unknown:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn find_auth_knows_ids_and_not_strangers() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", Some("alice@example.com")).await;

        assert_eq!(alice.name, "alice");
        assert_eq!(alice.email.as_deref(), Some("alice@example.com"));
        assert!(!alice.is_admin);
        assert_eq!(find_auth(&db.pool, "no-such-id").await.unwrap(), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn creating_a_user_creates_its_personal_team_with_it_as_owner() {
        let db = test_db().await;

        let user = create(&db.pool, &alice(), &AuditActor::cli(), now())
            .await
            .unwrap();

        assert_eq!(user.name, "alice");
        assert_eq!(user.teams, ["alice"]);
        assert_eq!(find(&db.pool, "alice").await.unwrap(), user);
        let team = teams::find(&db.pool, "alice").await.unwrap();
        assert_eq!(team.personal_of.as_deref(), Some("alice"));
        assert_eq!(team.members.len(), 1);
        assert_eq!(team.members[0].user, "alice");
        assert_eq!(team.members[0].role, teams::Role::Owner);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_user_name_taken_by_a_team_is_refused() {
        let db = test_db().await;
        create(&db.pool, &alice(), &AuditActor::cli(), now())
            .await
            .unwrap();
        teams::create(
            &db.pool,
            &family_owned_by_alice(),
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();

        let clash = create(
            &db.pool,
            &NewUser {
                name: "family",
                display_name: "",
            },
            &AuditActor::cli(),
            now(),
        )
        .await;

        assert!(matches!(clash, Err(OkfError::Refused { .. })), "{clash:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_invalid_or_duplicate_name_is_refused() {
        let db = test_db().await;
        create(&db.pool, &alice(), &AuditActor::cli(), now())
            .await
            .unwrap();

        let duplicate = create(&db.pool, &alice(), &AuditActor::cli(), now()).await;
        let invalid = create(
            &db.pool,
            &NewUser {
                name: "Alice",
                display_name: "",
            },
            &AuditActor::cli(),
            now(),
        )
        .await;

        assert!(matches!(duplicate, Err(OkfError::Refused { .. })));
        assert!(matches!(invalid, Err(OkfError::Refused { .. })));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_name_that_loses_the_insert_race_is_refused_not_a_database_error() {
        // `check_free` passes for both of two concurrent creates; the INSERT
        // then hits the UNIQUE constraint. Reproduce that deterministically:
        // a transaction that passed `check_free` before another committed the
        // same team name.
        let db = test_db().await;
        create(&db.pool, &alice(), &AuditActor::cli(), now())
            .await
            .unwrap();
        let mut tx = db.pool.begin().await.unwrap();
        check_free(&mut tx, "family").await.unwrap();
        teams::create(
            &db.pool,
            &family_owned_by_alice(),
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();

        let inserted = sqlx::query_scalar::<_, String>(
            "INSERT INTO teams (name, created_at) VALUES ('family', now()) RETURNING id",
        )
        .fetch_one(&mut *tx)
        .await;

        let mapped = or_name_taken(inserted, "family");
        assert!(
            matches!(mapped, Err(OkfError::Refused { .. })),
            "{mapped:?}"
        );
    }

    #[tokio::test]
    async fn concurrent_user_and_team_creates_of_one_name_give_one_winner_and_a_409() {
        let db = test_db().await;
        create(&db.pool, &alice(), &AuditActor::cli(), now())
            .await
            .unwrap();
        let user = NewUser {
            name: "family",
            display_name: "",
        };
        let team = family_owned_by_alice();

        // `tokio::join!` runs both futures concurrently, like `asyncio.gather`.
        let actor = AuditActor::cli();
        let (made_user, made_team) = tokio::join!(
            create(&db.pool, &user, &actor, now()),
            teams::create(&db.pool, &team, &actor, now()),
        );

        let refused = [made_user.is_err(), made_team.is_err()];
        assert_eq!(
            refused.iter().filter(|r| **r).count(),
            1,
            "{made_user:?} {made_team:?}"
        );
        for error in [made_user.err(), made_team.err()].into_iter().flatten() {
            assert!(matches!(error, OkfError::Refused { .. }), "{error:?}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_includes_the_fixture_and_counts_credentials() {
        let db = test_db().await;
        create(&db.pool, &alice(), &AuditActor::cli(), now())
            .await
            .unwrap();

        let users = list(&db.pool).await.unwrap();

        let names: Vec<&str> = users.iter().map(|u| u.name.as_str()).collect();
        assert_eq!(names, ["alice", crate::db::TEST_USER]);
        assert_eq!(users[0].credentials, 0);
    }

    /// A key of `user` on catalog `shared`; revoked when `revoked`.
    async fn key_for(pool: &PgPool, user: &str, name: &str, revoked: bool) -> String {
        let catalogs = ["shared".to_string()];
        let created = key_rows::create_with_id(
            pool,
            &NewKey {
                name,
                access: Access::Write,
                maintenance_report: false,
                grants: Grants::Catalogs(&catalogs),
                default_catalog: None,
                actor: name,
                user,
                self_issued: false,
                expires_at: None,
                levels: &[],
            },
            now(),
        )
        .await
        .unwrap();
        if revoked {
            key_rows::revoke_by_id(pool, &created.id, now())
                .await
                .unwrap();
        }
        created.id
    }

    /// Gives `user` one unredeemed sign-in code.
    async fn code_for(pool: &PgPool, user: &str) {
        sqlx::query(
            "INSERT INTO sign_in_codes \
                 (name, sha256, access, all_catalogs, actor, created_at, expires_at, user_id) \
             VALUES ('laptop', decode(repeat('cd', 32), 'hex'), 'read', true, 'laptop', \
                     now(), now() + interval '1 hour', \
                     (SELECT id FROM users WHERE name = $1))",
        )
        .bind(user)
        .execute(pool)
        .await
        .unwrap();
    }

    /// The single number `sql` answers.
    async fn count(pool: &PgPool, sql: &str) -> i64 {
        // `AssertSqlSafe`: the text is a test's own constant, not input.
        sqlx::query_scalar(sqlx::AssertSqlSafe(sql.to_string()))
            .fetch_one(pool)
            .await
            .unwrap()
    }

    /// The reason of a refused deletion.
    fn reason(result: Result<DeletedUser, OkfError>) -> String {
        match result {
            Err(OkfError::Refused { reason }) => reason,
            other => panic!("expected a refusal, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unreferenced_user_is_deleted_with_its_personal_team() {
        let db = test_db().await;
        create(&db.pool, &alice(), &AuditActor::cli(), now())
            .await
            .unwrap();
        let before = crate::store::catalogs::admin_generation(&db.pool)
            .await
            .unwrap();

        let deleted = delete(&db.pool, "alice", false, &AuditActor::cli(), now())
            .await
            .unwrap();

        assert_eq!(deleted, DeletedUser::default());
        assert!(matches!(
            find(&db.pool, "alice").await,
            Err(OkfError::UnknownName { .. })
        ));
        assert!(matches!(
            teams::find(&db.pool, "alice").await,
            Err(OkfError::UnknownName { .. })
        ));
        let after = crate::store::catalogs::admin_generation(&db.pool)
            .await
            .unwrap();
        assert!(after > before, "the delete bumps the admin generation");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deleting_a_user_removes_its_credentials_and_keeps_the_history() {
        let db = test_db().await;
        seed_user(&db.pool, "alice", Some("alice@example.com")).await;
        let family = teams::NewTeam {
            name: "family",
            display_name: "",
            owner: crate::db::TEST_USER,
        };
        teams::create(&db.pool, &family, &AuditActor::cli(), now())
            .await
            .unwrap();
        teams::set_member(
            &db.pool,
            &crate::store::teams::Membership {
                team: "family",
                user: "alice",
                role: teams::Role::Member,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        seed_catalog_owned_by(&db.pool, "family").await;
        key_for(&db.pool, "alice", "alice-laptop", false).await;
        key_for(&db.pool, "alice", "alice-old", true).await;
        code_for(&db.pool, "alice").await;
        seed_session(&db.pool, &id_by_name(&db.pool, "alice").await.unwrap()).await;
        crate::store::ceiling::entitle(
            &db.pool,
            &NewEntitlement {
                user: "alice",
                catalog: "shared",
                access: Access::Read,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        // A revision alice's key wrote: history keeps its actor text.
        sqlx::query(
            "INSERT INTO revisions (catalog_id, path, revision, op, content, executable, actor, \
                                    message, created_at) \
             SELECT id, 'a.md', 1, 'create', 'x', false, 'alice-laptop', 'm', $1 \
             FROM catalogs WHERE name = 'shared'",
        )
        .bind(now())
        .execute(&db.pool)
        .await
        .unwrap();

        let deleted = delete(&db.pool, "alice", false, &AuditActor::cli(), now())
            .await
            .unwrap();

        assert_eq!(
            deleted,
            DeletedUser {
                keys: 2,
                codes: 1,
                discarded: 0
            }
        );
        assert!(matches!(
            find(&db.pool, "alice").await,
            Err(OkfError::UnknownName { .. })
        ));
        for table in [
            "api_keys",
            "sign_in_codes",
            "user_catalogs",
            "user_sessions",
        ] {
            let sql = format!("SELECT count(*) FROM {table}");
            assert_eq!(count(&db.pool, &sql).await, 0, "{table}");
        }
        assert_eq!(
            count(&db.pool, "SELECT count(*) FROM teams WHERE name = 'alice'").await,
            0
        );
        assert_eq!(
            teams::find(&db.pool, "family").await.unwrap().members.len(),
            1
        );
        assert_eq!(
            teams::find(&db.pool, "family").await.unwrap().catalogs,
            ["shared"]
        );
        assert_eq!(
            count(
                &db.pool,
                "SELECT count(*) FROM revisions WHERE actor = 'alice-laptop'"
            )
            .await,
            1
        );
        let (person, details): (String, serde_json::Value) = sqlx::query_as(
            "SELECT p.name, e.details FROM audit_entries e \
             JOIN audit_people p ON p.id = e.target_person_id \
             WHERE e.action = 'user.deleted'",
        )
        .fetch_one(&db.pool)
        .await
        .unwrap();
        assert!(person.starts_with("deleted user #"), "{person}");
        assert_eq!(
            details,
            serde_json::json!({"keys": 2, "codes": 1, "changesets_discarded": 0})
        );
        assert_eq!(
            count(
                &db.pool,
                "SELECT count(*) FROM audit_people WHERE name = 'alice' OR user_id IS NOT NULL \
                 AND user_id NOT IN (SELECT id FROM users)"
            )
            .await,
            0,
            "no audit row names alice or points at her"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_deleted_users_open_changesets_are_discarded_and_audited() {
        let db = test_db().await;
        seed_user(&db.pool, "alice", None).await;
        seed_catalog_owned_by(&db.pool, crate::db::TEST_TEAM).await;
        let key = key_for(&db.pool, "alice", "alice-laptop", false).await;
        let catalog_id: i64 = sqlx::query_scalar("SELECT id FROM catalogs WHERE name = 'shared'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        let mut tx = db.pool.begin().await.unwrap();
        let changeset = changesets::open(&mut tx, catalog_id, &key, "alice-laptop", now())
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let deleted = delete(&db.pool, "alice", false, &AuditActor::cli(), now())
            .await
            .unwrap();

        assert_eq!(deleted.discarded, 1);
        let state: String = sqlx::query_scalar("SELECT state FROM changesets WHERE id = $1")
            .bind(changeset.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(state, "discarded");
        let (target, teams, details): (Option<String>, Vec<i64>, serde_json::Value) =
            sqlx::query_as(
                "SELECT target_name, teams, details FROM audit_entries \
                 WHERE action = 'catalog.changeset_discarded'",
            )
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(target.as_deref(), Some("shared"));
        assert_eq!(teams.len(), 1, "the catalog's owner team");
        assert_eq!(details["changeset"], changeset.handle);
        assert_eq!(details["files"], 0);
        assert_eq!(details["reason"], "user_deleted");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deletion_is_refused_for_owned_catalogs_a_sole_owner_and_the_last_admin() {
        let db = test_db().await;
        seed_user(&db.pool, "alice", None).await;
        seed_catalog_owned_by(&db.pool, "alice").await;
        let owns = delete(&db.pool, "alice", false, &AuditActor::cli(), now()).await;
        catalogs::delete(&db.pool, "shared", true, &AuditActor::cli(), Utc::now())
            .await
            .unwrap();
        teams::create(
            &db.pool,
            &family_owned_by_alice(),
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let sole = delete(&db.pool, "alice", false, &AuditActor::cli(), now()).await;
        teams::set_member(
            &db.pool,
            &crate::store::teams::Membership {
                team: "family",
                user: crate::db::TEST_USER,
                role: teams::Role::Owner,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        set_admin(
            &db.pool,
            "alice",
            AdminChange {
                on: true,
                allow_last: true,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let last_admin = delete(&db.pool, "alice", false, &AuditActor::cli(), now()).await;
        let by_the_cli = delete(&db.pool, "alice", true, &AuditActor::cli(), now()).await;

        assert!(reason(owns).contains("catalog(s) \"shared\""));
        assert!(reason(sole).contains("last owner of team(s) \"family\""));
        assert!(reason(last_admin).contains("last admin user"));
        assert!(by_the_cli.is_ok(), "the CLI may delete the last admin");
        assert_eq!(
            teams::find(&db.pool, "family").await.unwrap().members.len(),
            1,
            "the other owner stays"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_provisioner_is_refused_an_admin_user() {
        let db = test_db().await;
        seed_user(&db.pool, "alice", None).await;
        set_admin(
            &db.pool,
            "alice",
            AdminChange {
                on: true,
                allow_last: true,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let provisioner = AuditActor {
            provisioner: true,
            ..AuditActor::cli()
        };

        let refused = delete(&db.pool, "alice", true, &provisioner, now()).await;

        assert!(
            matches!(refused, Err(OkfError::Forbidden { .. })),
            "{refused:?}"
        );
        assert!(find(&db.pool, "alice").await.unwrap().is_admin);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deleting_an_unknown_or_malformed_user_is_unknown_name() {
        let db = test_db().await;
        for name in ["nobody", "a\0b"] {
            let missing = delete(&db.pool, name, false, &AuditActor::cli(), now()).await;
            assert!(
                matches!(missing, Err(OkfError::UnknownName { noun: "user", .. })),
                "{name:?}: {missing:?}"
            );
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_refused_deletion_changes_nothing() {
        let db = test_db().await;
        create(&db.pool, &alice(), &AuditActor::cli(), now())
            .await
            .unwrap();
        seed_catalog_owned_by(&db.pool, "alice").await;
        code_for(&db.pool, "alice").await;
        let generation = catalogs::admin_generation(&db.pool).await.unwrap();
        let entries = "SELECT count(*) FROM audit_entries";
        let recorded = count(&db.pool, entries).await;

        let refused = delete(&db.pool, "alice", false, &AuditActor::cli(), now()).await;

        assert!(reason(refused).contains("catalog"));
        assert_unchanged(&db.pool, 1, generation).await;
        let team = teams::find(&db.pool, "alice").await.unwrap();
        assert_eq!(team.catalogs, ["shared"]);
        assert_eq!(count(&db.pool, entries).await, recorded, "no new entry");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_catalog_assigned_during_the_delete_is_refused_not_a_database_error() {
        // Arrange: a transaction assigns a catalog to alice's personal team
        // and holds its commit while the delete runs.
        let db = test_db().await;
        create(&db.pool, &alice(), &AuditActor::cli(), now())
            .await
            .unwrap();
        catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog {
                name: "shared",
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let mut assigning = db.pool.begin().await.unwrap();
        sqlx::query(OWN_SHARED)
            .bind("alice")
            .execute(&mut *assigning)
            .await
            .unwrap();
        // `tokio::spawn` runs the delete concurrently, like a thread; it
        // blocks on the personal team's row, which the assignment's
        // foreign-key check holds `FOR KEY SHARE`.
        let pool = db.pool.clone();
        let deleting =
            tokio::spawn(
                async move { delete(&pool, "alice", false, &AuditActor::cli(), now()).await },
            );
        until_a_lock_is_awaited(&db.pool).await;

        assigning.commit().await.unwrap();
        let outcome = tokio::time::timeout(LOCK_WAIT, deleting)
            .await
            .expect("the delete waited for the assignment for too long")
            .unwrap();

        assert!(reason(outcome).contains("catalog"));
        assert_eq!(
            teams::find(&db.pool, "alice").await.unwrap().catalogs,
            ["shared"]
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_key_created_while_deletion_waits_is_deleted_with_the_user() {
        let db = test_db().await;
        seed_user(&db.pool, "alice", None).await;
        // A key creation in flight holds alice `FOR KEY SHARE` (`id_of`).
        let mut creating = db.pool.begin().await.unwrap();
        id_of(&mut creating, "alice").await.unwrap();
        let pool = db.pool.clone();
        let deletion =
            tokio::spawn(
                async move { delete(&pool, "alice", false, &AuditActor::cli(), now()).await },
            );
        until_a_lock_is_awaited(&db.pool).await;
        sqlx::query(
            "INSERT INTO api_keys (id, name, sha256, access, all_catalogs, actor, created_at, \
                                   user_id) \
             SELECT 'late', 'late', sha256('late'::bytea), 'read', false, 'late', $1, id \
             FROM users WHERE name = 'alice'",
        )
        .bind(now())
        .execute(&mut *creating)
        .await
        .unwrap();
        creating.commit().await.unwrap();

        let deleted = tokio::time::timeout(LOCK_WAIT, deletion)
            .await
            .expect("the delete waited for the key creation for too long")
            .unwrap()
            .unwrap();

        assert_eq!(
            deleted.keys, 1,
            "the waiting deletion saw the committed key"
        );
        assert_eq!(count(&db.pool, "SELECT count(*) FROM api_keys").await, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_changeset_opened_while_deletion_waits_is_discarded_with_the_user() {
        let db = test_db().await;
        seed_user(&db.pool, "alice", None).await;
        seed_catalog_owned_by(&db.pool, crate::db::TEST_TEAM).await;
        let key = key_for(&db.pool, "alice", "alice-laptop", false).await;
        let catalog_id: i64 = sqlx::query_scalar("SELECT id FROM catalogs WHERE name = 'shared'")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        // An open in flight holds the key `FOR KEY SHARE` (its foreign-key
        // check) until it commits.
        let mut opening = db.pool.begin().await.unwrap();
        let changeset = changesets::open(&mut opening, catalog_id, &key, "alice-laptop", now())
            .await
            .unwrap();
        let pool = db.pool.clone();
        let deletion =
            tokio::spawn(
                async move { delete(&pool, "alice", false, &AuditActor::cli(), now()).await },
            );
        until_a_lock_is_awaited(&db.pool).await;
        opening.commit().await.unwrap();

        let deleted = tokio::time::timeout(LOCK_WAIT, deletion)
            .await
            .expect("the delete waited for the open for too long")
            .unwrap()
            .unwrap();

        assert_eq!(
            deleted.discarded, 1,
            "the waiting deletion saw the changeset"
        );
        let state: String = sqlx::query_scalar("SELECT state FROM changesets WHERE id = $1")
            .bind(changeset.id)
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(
            state, "discarded",
            "no changeset is left open without a key"
        );
    }

    /// The CLI's admin-flag change (it may clear the last admin).
    fn admin(on: bool) -> AdminChange {
        AdminChange {
            on,
            allow_last: true,
        }
    }

    /// The CLI actor with the provisioner flag (Task 5 sets it from a key).
    fn provisioner() -> AuditActor {
        AuditActor {
            provisioner: true,
            ..AuditActor::cli()
        }
    }

    /// The entries with `action` about user `name`, oldest first.
    async fn entries_about(
        pool: &PgPool,
        action: &str,
        name: &str,
    ) -> Vec<audit::testing::RecordedEntry> {
        audit::testing::entries(pool, action)
            .await
            .into_iter()
            .filter(|entry| entry.person.as_deref() == Some(name))
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_records_one_entry_and_a_refused_create_none() {
        let db = test_db().await;

        create(&db.pool, &alice(), &AuditActor::cli(), now())
            .await
            .unwrap();
        let taken = create(&db.pool, &alice(), &AuditActor::cli(), now()).await;

        assert!(matches!(taken, Err(OkfError::Refused { .. })), "{taken:?}");
        let entries = entries_about(&db.pool, "user.created", "alice").await;
        assert_eq!(entries.len(), 1, "the refused create recorded nothing");
        assert_eq!(entries[0].source, "cli");
        assert_eq!(entries[0].target_kind, "user");
        assert!(entries[0].teams.is_empty());
        assert_eq!(entries[0].details, json!({}));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_email_records_one_entry_and_none_for_no_change() {
        let db = test_db().await;
        seed_user(&db.pool, "alice", None).await;
        let cli = AuditActor::cli();

        set_email(&db.pool, "alice", Some("alice@example.com"), &cli, now())
            .await
            .unwrap();
        set_email(&db.pool, "alice", Some("ALICE@example.com"), &cli, now())
            .await
            .unwrap();
        set_email(&db.pool, "alice", None, &cli, now())
            .await
            .unwrap();
        set_email(&db.pool, "alice", None, &cli, now())
            .await
            .unwrap();

        let entries = entries_about(&db.pool, "user.email_changed", "alice").await;
        assert_eq!(
            entries.len(),
            2,
            "a change of letter case, or clearing no address, is not a change"
        );
        assert_eq!(entries[0].details, json!({"cleared": false}));
        assert_eq!(entries[1].details, json!({"cleared": true}));
        assert!(entries[0].teams.is_empty());
        assert!(
            !entries[0].details.to_string().contains('@'),
            "no email in details"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_audit_write_fails_the_change() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        seed_session(&db.pool, &alice.id).await;
        crate::http::api::testing::fail_on(&db.pool, "BEFORE INSERT ON audit_entries FOR EACH ROW")
            .await;
        let cli = AuditActor::cli();

        let email = set_email(&db.pool, "alice", Some("alice@example.com"), &cli, now()).await;
        let flag = set_admin(&db.pool, "alice", admin(true), &cli, now()).await;
        let ended = end_sessions(&db.pool, "alice", &cli, now()).await;
        let made = create(
            &db.pool,
            &NewUser {
                name: "bob",
                display_name: "",
            },
            &cli,
            now(),
        )
        .await;

        assert!(email.is_err() && flag.is_err() && ended.is_err() && made.is_err());
        let after = find(&db.pool, "alice").await.unwrap();
        assert_eq!(after.email, None, "the email change rolled back");
        assert!(!after.is_admin, "the admin change rolled back");
        assert_eq!(session_count(&db.pool).await, 1, "the session stayed");
        assert!(matches!(
            find(&db.pool, "bob").await,
            Err(OkfError::UnknownName { .. })
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_provisioner_cannot_set_an_admins_email() {
        let db = test_db().await;
        seed_user(&db.pool, "staff", None).await;
        seed_user(&db.pool, "alice", None).await;
        set_admin(&db.pool, "staff", admin(true), &AuditActor::cli(), now())
            .await
            .unwrap();

        let refused = set_email(
            &db.pool,
            "staff",
            Some("x@example.com"),
            &provisioner(),
            now(),
        )
        .await;
        let allowed = set_email(
            &db.pool,
            "alice",
            Some("a@example.com"),
            &provisioner(),
            now(),
        )
        .await;

        assert!(
            matches!(refused, Err(OkfError::Forbidden { .. })),
            "{refused:?}"
        );
        assert_eq!(find(&db.pool, "staff").await.unwrap().email, None);
        assert!(entries_about(&db.pool, "user.email_changed", "staff")
            .await
            .is_empty());
        allowed.expect("a provisioner may set a non-admin's email");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_admin_records_one_entry_and_none_for_no_change() {
        let db = test_db().await;
        seed_user(&db.pool, "alice", None).await;
        let cli = AuditActor::cli();

        set_admin(&db.pool, "alice", admin(true), &cli, now())
            .await
            .unwrap();
        set_admin(&db.pool, "alice", admin(true), &cli, now())
            .await
            .unwrap();
        set_admin(&db.pool, "alice", admin(false), &cli, now())
            .await
            .unwrap();
        let last = set_admin(
            &db.pool,
            "alice",
            AdminChange {
                on: false,
                allow_last: false,
            },
            &cli,
            now(),
        )
        .await;

        last.expect("clearing a flag already clear is no change, not a refusal");
        let entries = entries_about(&db.pool, "user.admin_changed", "alice").await;
        assert_eq!(entries.len(), 2, "no change records nothing");
        assert_eq!(entries[0].details, json!({"from": false, "to": true}));
        assert_eq!(entries[1].details, json!({"from": true, "to": false}));
        assert!(entries[0].teams.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ending_sessions_records_the_count_and_nothing_when_none_ended() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        seed_session(&db.pool, &alice.id).await;
        seed_session(&db.pool, &alice.id).await;

        end_sessions(&db.pool, "alice", &AuditActor::cli(), now())
            .await
            .unwrap();
        let none = end_sessions(&db.pool, "alice", &AuditActor::cli(), now())
            .await
            .unwrap();

        assert_eq!(none, 0);
        let entries = entries_about(&db.pool, "user.sessions_revoked", "alice").await;
        assert_eq!(entries.len(), 1, "ending no session records nothing");
        assert_eq!(entries[0].details, json!({"sessions": 2}));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ending_one_session_records_one_entry_and_a_refusal_none() {
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        seed_session(&db.pool, &alice.id).await;
        seed_session(&db.pool, &alice.id).await;
        let sessions = user_sessions::list(&db.pool, &alice.id, None)
            .await
            .unwrap();
        let cli = AuditActor::cli();

        end_session_by_prefix(&db.pool, "alice", &sessions[0].id_prefix, &cli, now())
            .await
            .unwrap();
        let again =
            end_session_by_prefix(&db.pool, "alice", &sessions[0].id_prefix, &cli, now()).await;
        let malformed = end_session_by_prefix(&db.pool, "alice", "xyz", &cli, now()).await;
        let stranger =
            end_session_by_prefix(&db.pool, "nobody", &sessions[1].id_prefix, &cli, now()).await;
        // The user's own revoke (the account page) is not an administrative
        // change, so it records nothing.
        user_sessions::revoke_by_prefix(&db.pool, &alice.id, &sessions[1].id_prefix)
            .await
            .unwrap();

        assert!(matches!(
            again,
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
        assert!(matches!(
            stranger,
            Err(OkfError::UnknownName { noun: "user", .. })
        ));
        assert_eq!(session_count(&db.pool).await, 0);
        let entries = entries_about(&db.pool, "user.sessions_revoked", "alice").await;
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].details, json!({"sessions": 1}));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn ending_one_session_waits_for_a_sign_in_holding_the_user() {
        // `user_sessions::start` holds the user `FOR SHARE` while it inserts
        // a session; the admin's revoke locks the user first, so it waits
        // rather than taking the session rows first.
        let db = test_db().await;
        let alice = seed_user(&db.pool, "alice", None).await;
        seed_session(&db.pool, &alice.id).await;
        let prefix = user_sessions::list(&db.pool, &alice.id, None)
            .await
            .unwrap()[0]
            .id_prefix
            .clone();
        let mut holder = db.pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM users WHERE name = 'alice' FOR SHARE")
            .execute(&mut *holder)
            .await
            .unwrap();
        let pool = db.pool.clone();
        // `tokio::spawn` runs the revoke on another task, so this one can
        // watch it wait and then release the row.
        let revoke = tokio::spawn(async move {
            end_session_by_prefix(&pool, "alice", &prefix, &AuditActor::cli(), now()).await
        });
        tokio::time::timeout(LOCK_WAIT, until_a_lock_is_awaited(&db.pool))
            .await
            .expect("the revoke never waited on the user's row");
        holder.rollback().await.unwrap();

        tokio::time::timeout(LOCK_WAIT, revoke)
            .await
            .expect("the revoke never finished")
            .unwrap()
            .unwrap();

        assert_eq!(session_count(&db.pool).await, 0);
    }

    /// User `name`, made an admin, acting through a console session: its
    /// entries resolve (and lock) its row, as a REST change by an admin does.
    pub(crate) async fn admin_session_actor(pool: &PgPool, name: &str) -> AuditActor {
        let user = seed_user(pool, name, None).await;
        set_admin(pool, name, admin(true), &AuditActor::cli(), now())
            .await
            .unwrap();
        AuditActor {
            source: audit::Source::Console,
            user_id: Some(user.id),
            label: name.to_string(),
            via: audit::Via::Session,
            ..AuditActor::cli()
        }
    }

    /// Runs `change` and a `set_admin` that promotes `promoted` with their
    /// order forced: a held `server_state` row queues `change` first, then
    /// `set_admin`, and is released only once both wait. A change that
    /// locked the generation row before its actor's row would then wait on
    /// the actor (an admin, which `set_admin` holds `FOR UPDATE`) while
    /// `set_admin` waited on the generation row: a deadlock Postgres aborts.
    ///
    /// # Returns
    /// Both results, each within 30 s.
    pub(crate) async fn queued_with_set_admin<F>(
        pool: &PgPool,
        promoted: &str,
        change: F,
    ) -> (Result<(), OkfError>, Result<bool, OkfError>)
    where
        // `F` is any future the runtime can move to another thread.
        F: std::future::Future<Output = Result<(), OkfError>> + Send + 'static,
    {
        use crate::http::api::testing::wait_for_lock_waiters;
        let mut holder = pool.begin().await.unwrap();
        sqlx::query("UPDATE server_state SET admin_generation = admin_generation")
            .execute(&mut *holder)
            .await
            .unwrap();
        let changing = tokio::spawn(change);
        tokio::time::timeout(LOCK_WAIT, wait_for_lock_waiters(pool, 1))
            .await
            .expect("the change never queued");
        let promoting = tokio::spawn({
            let pool = pool.clone();
            let promoted = promoted.to_string();
            async move { set_admin(&pool, &promoted, admin(true), &AuditActor::cli(), now()).await }
        });
        tokio::time::timeout(LOCK_WAIT, wait_for_lock_waiters(pool, 2))
            .await
            .expect("set_admin never queued");
        holder.commit().await.unwrap();
        let changed = tokio::time::timeout(LOCK_WAIT, changing)
            .await
            .expect("the change finished within 30 s")
            .expect("the change did not panic");
        let promoted = tokio::time::timeout(LOCK_WAIT, promoting)
            .await
            .expect("set_admin finished within 30 s")
            .expect("set_admin did not panic");
        (changed, promoted)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_admins_change_and_a_concurrent_set_admin_both_finish() {
        let db = test_db().await;
        let staff = admin_session_actor(&db.pool, "staff").await;
        seed_user(&db.pool, "alice", None).await;
        seed_user(&db.pool, "bob", None).await;
        seed_user(&db.pool, "carol", None).await;
        let pool = db.pool.clone();
        let actor = staff.clone();
        let email = async move {
            set_email(&pool, "alice", Some("alice@example.com"), &actor, now()).await
        };
        let pool = db.pool.clone();
        let actor = staff.clone();
        let created = async move {
            let new = NewUser {
                name: "dave",
                display_name: "",
            };
            create(&pool, &new, &actor, now()).await.map(|_| ())
        };

        let (emailed, first) = queued_with_set_admin(&db.pool, "bob", email).await;
        let (made, second) = queued_with_set_admin(&db.pool, "carol", created).await;

        emailed.expect("the email change");
        first.expect("the first promotion");
        made.expect("the create");
        second.expect("the second promotion");
        assert_eq!(
            entries_about(&db.pool, "user.email_changed", "alice").await[0]
                .actor_person
                .as_deref(),
            Some("staff")
        );
    }

    /// Two admins acting by session, the lower user id first, with their
    /// audit rows made stale (a display-name change) so the next entry
    /// naming them refreshes both rows.
    pub(crate) async fn two_admins(pool: &PgPool) -> [(String, AuditActor); 2] {
        let mut pair = [
            ("ann".to_string(), admin_session_actor(pool, "ann").await),
            ("ben".to_string(), admin_session_actor(pool, "ben").await),
        ];
        // `sort_by` with the user ids: the lower id first.
        pair.sort_by(|left, right| left.1.user_id.cmp(&right.1.user_id));
        sqlx::query("UPDATE users SET display_name = 'renamed' WHERE name IN ('ann', 'ben')")
            .execute(pool)
            .await
            .unwrap();
        pair
    }

    /// Runs `first` and `second`, two changes that name the same two people
    /// in opposite order, at once, while another transaction holds
    /// `held_user`'s audit row as a refresh does (`FOR NO KEY UPDATE`).
    /// Resolving a person must never wait on that row: when every resolver
    /// upserted (and so locked) each row it named, the two changes each held
    /// one person's row and waited for the other's, a deadlock; here both
    /// would wait for the holder and time out.
    ///
    /// # Returns
    /// Both results, each within 30 s; the holder is released after.
    pub(crate) async fn both_finish_while_a_person_is_held<F, G>(
        pool: &PgPool,
        held_user: &str,
        first: F,
        second: G,
    ) -> (Result<(), OkfError>, Result<(), OkfError>)
    where
        // Any futures the runtime can move to another thread.
        F: std::future::Future<Output = Result<(), OkfError>> + Send + 'static,
        G: std::future::Future<Output = Result<(), OkfError>> + Send + 'static,
    {
        let mut holder = pool.begin().await.unwrap();
        sqlx::query("SELECT 1 FROM audit_people WHERE user_id = $1 FOR NO KEY UPDATE")
            .bind(held_user)
            .execute(&mut *holder)
            .await
            .unwrap();
        let (first, second) = (tokio::spawn(first), tokio::spawn(second));
        let first = tokio::time::timeout(LOCK_WAIT, first)
            .await
            .expect("the first change finished while a person's audit row was held")
            .expect("the first change did not panic");
        let second = tokio::time::timeout(LOCK_WAIT, second)
            .await
            .expect("the second change finished while a person's audit row was held")
            .expect("the second change did not panic");
        holder.rollback().await.unwrap();
        (first, second)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn admins_setting_each_others_email_at_once_both_finish() {
        let db = test_db().await;
        let [(low_name, low), (high_name, high)] = two_admins(&db.pool).await;
        let held = low.user_id.clone().unwrap();
        let pool = db.pool.clone();
        let first = async move {
            set_email(&pool, &high_name, Some("high@example.com"), &low, now()).await
        };
        let pool = db.pool.clone();
        let second =
            async move { set_email(&pool, &low_name, Some("low@example.com"), &high, now()).await };

        let (one, other) = both_finish_while_a_person_is_held(&db.pool, &held, first, second).await;

        one.expect("the first email change");
        other.expect("the second email change");
        assert_eq!(
            audit::testing::entries(&db.pool, "user.email_changed")
                .await
                .len(),
            2
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn admins_ending_each_others_sessions_at_once_both_finish() {
        let db = test_db().await;
        let [(low_name, low), (high_name, high)] = two_admins(&db.pool).await;
        let held = low.user_id.clone().unwrap();
        let pool = db.pool.clone();
        let target = high_name.clone();
        let first = async move {
            let session = seed_session_of(&pool, &target).await;
            end_session_by_prefix(&pool, &target, &session, &low, now()).await
        };
        let pool = db.pool.clone();
        let target = low_name.clone();
        let second = async move {
            let session = seed_session_of(&pool, &target).await;
            end_session_by_prefix(&pool, &target, &session, &high, now()).await
        };

        let (one, other) = both_finish_while_a_person_is_held(&db.pool, &held, first, second).await;

        one.expect("the first revoke");
        other.expect("the second revoke");
    }

    /// Gives user `name` a console session and answers its id prefix.
    async fn seed_session_of(pool: &PgPool, name: &str) -> String {
        let id = id_by_name(pool, name).await.unwrap();
        seed_session(pool, &id).await;
        user_sessions::list(pool, &id, None).await.unwrap()[0]
            .id_prefix
            .clone()
    }
}

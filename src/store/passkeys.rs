//! Users' WebAuthn passkeys (`user_passkeys`): public keys only, stored as
//! webauthn-rs's serialized `Passkey` (its signature counter included).
//! Passkeys are not part of the key cache, so nothing here bumps
//! `admin_generation`.
//!
//! Lock order, as everywhere in the login tables (`store::email_tokens`):
//! a transaction that locks a user's row takes it before that user's
//! passkey, token or session rows.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{DateTime, Utc};
use sqlx::types::Json;
use sqlx::{PgConnection, PgPool};
use webauthn_rs::prelude::Passkey;

use crate::error::OkfError;
use crate::store::catalogs::is_unique_violation;
use crate::store::users::{user_auth_columns, UserAuth};
use crate::store::{sign_in_methods, user_sessions};

/// A passkey as the account page lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PasskeyInfo {
    /// Its credential id, unpadded base64url (the id the API takes).
    pub id: String,
    /// The name its owner gave it.
    pub name: String,
    /// When it was registered.
    pub created_at: DateTime<Utc>,
    /// When it last signed in.
    pub last_used_at: Option<DateTime<Utc>>,
}

/// Stores a newly registered passkey.
///
/// # Arguments
/// * `pool` - the database.
/// * `user_id` - its owner.
/// * `passkey` - from `finish_passkey_registration`.
/// * `name` - its name (checked by the caller).
/// * `now` - the server clock.
///
/// # Errors
/// As [`add_in`].
pub async fn add(
    pool: &PgPool,
    user_id: &str,
    passkey: &Passkey,
    name: &str,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    let mut conn = pool.acquire().await?;
    add_in(&mut conn, user_id, passkey, name, now).await
}

/// [`add`], inside the caller's transaction: the setup link stores its
/// passkey in the transaction that uses the link, so a failure leaves the
/// link usable.
///
/// # Arguments
/// * `conn` - the caller's connection or transaction (`&mut tx`).
/// * `user_id` - its owner.
/// * `passkey` - from `finish_passkey_registration`.
/// * `name` - its name (checked by the caller).
/// * `now` - the server clock.
///
/// # Errors
/// [`OkfError::Refused`] if the credential is already registered;
/// [`OkfError::Database`].
pub async fn add_in(
    conn: &mut PgConnection,
    user_id: &str,
    passkey: &Passkey,
    name: &str,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    // `Json(...)` binds the value as `jsonb` through serde.
    let inserted = sqlx::query(
        "INSERT INTO user_passkeys (credential_id, user_id, passkey, name, created_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(passkey.cred_id().as_slice())
    .bind(user_id)
    .bind(Json(passkey))
    .bind(name)
    .bind(now)
    .execute(&mut *conn)
    .await;
    match inserted {
        Ok(_) => Ok(()),
        Err(error) if is_unique_violation(&error) => Err(OkfError::Refused {
            reason: "this passkey is already registered".to_string(),
        }),
        Err(error) => Err(error.into()),
    }
}

/// A passkey registered from a signed-in console session (no `Debug`: it
/// holds the session id).
pub struct SessionRegistration<'a> {
    /// The owner.
    pub user_id: &'a str,
    /// The raw id of the session that registered it.
    pub session_id: &'a str,
    /// From `finish_passkey_registration`.
    pub passkey: &'a Passkey,
    /// Its name (checked by the caller).
    pub name: &'a str,
    /// Keep the user's other sessions (otherwise they end).
    pub keep_sessions: bool,
    /// The server clock.
    pub now: DateTime<Utc>,
}

/// Stores a passkey registered from a console session and, unless kept,
/// ends the user's other sessions, inside the caller's transaction (which
/// also queues the notice, then commits). The user's row is
/// locked first and the registering session is checked under that lock:
/// a password reset (or any change that ended the session) committed
/// meanwhile leaves no passkey behind, so a session that was signed out
/// cannot give itself a lasting way back in.
///
/// # Arguments
/// * `conn` - the caller's transaction (`&mut tx`).
/// * `registration` - what to store and for whom.
///
/// # Returns
/// How many other sessions ended.
///
/// # Errors
/// [`OkfError::Unauthenticated`] once the session or the user is gone;
/// [`OkfError::Refused`] for a credential already registered;
/// [`OkfError::Database`].
pub async fn add_from_session_in(
    conn: &mut PgConnection,
    registration: &SessionRegistration<'_>,
) -> Result<u64, OkfError> {
    // `FOR NO KEY UPDATE`, as every credential change takes it: a reset or
    // a removal on this user queues behind this transaction, or this one
    // behind it.
    let locked: Option<String> =
        sqlx::query_scalar("SELECT id FROM users WHERE id = $1 FOR NO KEY UPDATE")
            .bind(registration.user_id)
            .fetch_optional(&mut *conn)
            .await?;
    let live = locked.is_some()
        && user_sessions::hold_live(
            &mut *conn,
            registration.session_id,
            registration.user_id,
            registration.now,
        )
        .await?;
    if !live {
        return Err(OkfError::Unauthenticated {
            reason: "this session has ended; sign in again".to_string(),
        });
    }
    add_in(
        &mut *conn,
        registration.user_id,
        registration.passkey,
        registration.name,
        registration.now,
    )
    .await?;
    let ended = if registration.keep_sessions {
        0
    } else {
        user_sessions::revoke_all(
            &mut *conn,
            registration.user_id,
            Some(registration.session_id),
        )
        .await?
    };
    Ok(ended)
}

/// A user's passkeys, oldest first, for `exclude_credentials`.
///
/// # Arguments
/// * `pool` - the database.
/// * `user_id` - the user.
///
/// # Returns
/// Each passkey's name and value.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn for_user(pool: &PgPool, user_id: &str) -> Result<Vec<(String, Passkey)>, OkfError> {
    let rows: Vec<(String, Json<Passkey>)> = sqlx::query_as(
        "SELECT name, passkey FROM user_passkeys WHERE user_id = $1 ORDER BY created_at",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    // `Json(passkey)` in the closure's pattern unwraps the wrapper.
    Ok(rows
        .into_iter()
        .map(|(name, Json(passkey))| (name, passkey))
        .collect())
}

/// The passkey with this credential id (no lock).
///
/// # Arguments
/// * `pool` - the database.
/// * `credential_id` - the raw id.
///
/// # Returns
/// Its owner's id and the passkey, or `None`.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn find(
    pool: &PgPool,
    credential_id: &[u8],
) -> Result<Option<(String, Passkey)>, OkfError> {
    let row: Option<(String, Json<Passkey>)> =
        sqlx::query_as("SELECT user_id, passkey FROM user_passkeys WHERE credential_id = $1")
            .bind(credential_id)
            .fetch_optional(pool)
            .await?;
    Ok(row.map(|(user_id, Json(passkey))| (user_id, passkey)))
}

/// The passkey a sign-in presents and its owner, both locked until the
/// caller's transaction ends: the owner's row `FOR SHARE` first (lock
/// order), then the passkey's row `FOR UPDATE`. The caller verifies the
/// assertion, stores the raised counter ([`update_in`]) and inserts the
/// session under these locks, so:
/// - two sign-ins with one passkey run one at a time, and the second is
///   checked against the counter the first stored (it never goes back);
/// - a removal of the passkey that commits first leaves nothing to find,
///   so no session starts; one that queues behind this sign-in removes it
///   after a session it already started, as it would after any earlier
///   sign-in (a removal ends no sessions);
/// - a password reset either commits first (this is then a sign-in after
///   the reset, with a passkey the reset left in place) or queues behind
///   this sign-in and ends the session it started.
///
/// There is no moment between the check and the insert in which a
/// removal or a reset can commit.
///
/// # Arguments
/// * `conn` - the caller's transaction (`&mut tx`).
/// * `credential_id` - the raw id from the assertion.
///
/// # Returns
/// The owner and the passkey, or `None` for an unknown credential (or one
/// removed while its owner was being locked).
///
/// # Errors
/// [`OkfError::Database`].
pub async fn lock_for_sign_in(
    conn: &mut PgConnection,
    credential_id: &[u8],
) -> Result<Option<(UserAuth, Passkey)>, OkfError> {
    // No lock yet: only to learn which user's row to lock first.
    let owner: Option<String> =
        sqlx::query_scalar("SELECT user_id FROM user_passkeys WHERE credential_id = $1")
            .bind(credential_id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some(owner) = owner else {
        return Ok(None);
    };
    let user: Option<UserAuth> = sqlx::query_as(concat!(
        "SELECT ",
        user_auth_columns!(),
        " FROM users u WHERE u.id = $1 FOR SHARE"
    ))
    .bind(&owner)
    .fetch_optional(&mut *conn)
    .await?;
    let Some(user) = user else {
        return Ok(None);
    };
    // Under the user's lock: a passkey removed meanwhile is not found, and
    // one with a counter another sign-in raised is read with that counter.
    let passkey: Option<Json<Passkey>> = sqlx::query_scalar(
        "SELECT passkey FROM user_passkeys \
         WHERE credential_id = $1 AND user_id = $2 FOR UPDATE",
    )
    .bind(credential_id)
    .bind(&owner)
    .fetch_optional(&mut *conn)
    .await?;
    Ok(passkey.map(|Json(passkey)| (user, passkey)))
}

/// Stores a passkey after a sign-in (its raised counter and backup flags)
/// and when it was used, inside the transaction of
/// [`lock_for_sign_in`].
///
/// # Arguments
/// * `conn` - the caller's transaction.
/// * `credential_id` - its raw id.
/// * `passkey` - after `update_credential`.
/// * `now` - the server clock.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn update_in(
    conn: &mut PgConnection,
    credential_id: &[u8],
    passkey: &Passkey,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    sqlx::query(
        "UPDATE user_passkeys SET passkey = $2, last_used_at = $3 WHERE credential_id = $1",
    )
    .bind(credential_id)
    .bind(Json(passkey))
    .bind(now)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// A user's passkeys, oldest first.
///
/// # Arguments
/// * `pool` - the database.
/// * `user_id` - the user.
///
/// # Returns
/// Their listing.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn list(pool: &PgPool, user_id: &str) -> Result<Vec<PasskeyInfo>, OkfError> {
    let rows: Vec<Listed> = sqlx::query_as(
        "SELECT credential_id, name, created_at, last_used_at \
         FROM user_passkeys WHERE user_id = $1 ORDER BY created_at",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| PasskeyInfo {
            id: URL_SAFE_NO_PAD.encode(row.credential_id),
            name: row.name,
            created_at: row.created_at,
            last_used_at: row.last_used_at,
        })
        .collect())
}

/// One row of [`list`] as read.
#[derive(sqlx::FromRow)]
struct Listed {
    credential_id: Vec<u8>,
    name: String,
    created_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
}

/// The raw credential id of an API id; not found when it is not base64url.
fn credential_id(id: &str) -> Result<Vec<u8>, OkfError> {
    URL_SAFE_NO_PAD.decode(id).map_err(|_| not_found(id))
}

/// The 404 for a passkey the user does not have.
fn not_found(id: &str) -> OkfError {
    OkfError::NotFound {
        path: format!("me/passkeys/{id}"),
    }
}

/// Renames one of the user's passkeys.
///
/// # Arguments
/// * `pool` - the database.
/// * `user_id` - the owner.
/// * `id` - the passkey's base64url id.
/// * `name` - the new name (checked by the caller).
///
/// # Errors
/// [`OkfError::NotFound`] unless the user owns it; [`OkfError::Database`].
pub async fn rename(pool: &PgPool, user_id: &str, id: &str, name: &str) -> Result<(), OkfError> {
    let renamed =
        sqlx::query("UPDATE user_passkeys SET name = $3 WHERE credential_id = $1 AND user_id = $2")
            .bind(credential_id(id)?)
            .bind(user_id)
            .bind(name)
            .execute(pool)
            .await?;
    if renamed.rows_affected() == 0 {
        return Err(not_found(id));
    }
    Ok(())
}

/// Removes one of the user's passkeys, unless it is their last sign-in
/// method.
///
/// # Arguments
/// * `pool` - the database.
/// * `user_id` - the owner.
/// * `id` - the passkey's base64url id.
///
/// # Errors
/// [`OkfError::NotFound`] unless the user owns it (an unknown user
/// included); [`OkfError::Refused`] for the last sign-in method;
/// [`OkfError::Database`].
pub async fn remove(pool: &PgPool, user_id: &str, id: &str) -> Result<(), OkfError> {
    let credential_id = credential_id(id)?;
    let mut tx = pool.begin().await?;
    // Locks the user first, so concurrent removals (and sign-ins with this
    // passkey) see each other. An unknown user owns no passkey.
    let methods = match sign_in_methods::count(&mut tx, user_id).await {
        Err(OkfError::NotFound { .. }) => return Err(not_found(id)),
        other => other?,
    };
    let deleted =
        sqlx::query("DELETE FROM user_passkeys WHERE credential_id = $1 AND user_id = $2")
            .bind(&credential_id)
            .bind(user_id)
            .execute(&mut *tx)
            .await?
            .rows_affected();
    if deleted == 0 {
        return Err(not_found(id));
    }
    if methods <= 1 {
        // Dropping `tx` rolls the delete back.
        return Err(OkfError::Refused {
            reason: "this is your last way to sign in; add a password or another passkey first"
                .to_string(),
        });
    }
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
pub(crate) mod testing {
    //! A real registered passkey for store tests, made with the software
    //! authenticator (no HTTP).

    use webauthn_authenticator_rs::prelude::WebauthnAuthenticator;
    use webauthn_authenticator_rs::softpasskey::SoftPasskey;
    use webauthn_rs::prelude::{Passkey, Url, Uuid, WebauthnBuilder};

    /// Registers a new software passkey for `user_id` (a UUID).
    pub(crate) fn passkey_for(user_id: &str) -> Passkey {
        let origin = Url::parse("http://localhost:8080").expect("an origin");
        let webauthn = WebauthnBuilder::new("localhost", &origin)
            .expect("a builder")
            .build()
            .expect("webauthn");
        let handle = Uuid::parse_str(user_id).expect("user ids are UUIDs");
        let (challenge, state) = webauthn
            .start_passkey_registration(handle, "alice@example.com", "alice", None)
            .expect("a challenge");
        // `WebauthnAuthenticator` is a trait: importing it gives
        // `SoftPasskey` its `do_registration` method.
        let credential = SoftPasskey::new(true)
            .do_registration(origin, challenge)
            .expect("the authenticator registers");
        webauthn
            .finish_passkey_registration(&credential, &state)
            .expect("a passkey")
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeZone;

    use super::testing::passkey_for;
    use super::*;
    use crate::db::test_db;
    use crate::store::audit::AuditActor;
    use crate::store::{passwords, users};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 12, 9, 0, 0)
            .single()
            .expect("a time")
    }

    async fn user(pool: &PgPool, name: &str) -> String {
        users::create(
            pool,
            &users::NewUser {
                name,
                display_name: "",
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .expect("create the user");
        sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(name)
            .fetch_one(pool)
            .await
            .expect("the user's id")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_added_passkey_is_listed_found_and_updated() {
        let db = test_db().await;
        let alice = user(&db.pool, "alice").await;
        let passkey = passkey_for(&alice);

        add(&db.pool, &alice, &passkey, "Laptop", now())
            .await
            .expect("add");
        let listed = list(&db.pool, &alice).await.expect("list");
        let found = find(&db.pool, passkey.cred_id().as_slice())
            .await
            .expect("find");
        let mut tx = db.pool.begin().await.expect("a transaction");
        let locked = lock_for_sign_in(&mut tx, passkey.cred_id().as_slice())
            .await
            .expect("lock");
        update_in(&mut tx, passkey.cred_id().as_slice(), &passkey, now())
            .await
            .expect("update");
        tx.commit().await.expect("commit");
        let used = list(&db.pool, &alice).await.expect("list");

        assert_eq!(listed.len(), 1);
        assert_eq!(
            listed[0].id,
            URL_SAFE_NO_PAD.encode(passkey.cred_id().as_slice())
        );
        assert_eq!(listed[0].name, "Laptop");
        assert_eq!(listed[0].last_used_at, None);
        let (owner, stored) = found.expect("the passkey");
        assert_eq!(owner, alice);
        assert_eq!(stored.cred_id(), passkey.cred_id());
        let (locked_user, _) = locked.expect("the passkey, locked");
        assert_eq!(locked_user.id, alice);
        assert_eq!(locked_user.name, "alice");
        assert_eq!(used[0].last_used_at, Some(now()));
        assert_eq!(for_user(&db.pool, &alice).await.expect("for_user").len(), 1);
        let mut conn = db.pool.acquire().await.expect("a connection");
        let unknown = lock_for_sign_in(&mut conn, b"no-such-credential")
            .await
            .expect("lock");
        assert!(unknown.is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_credential_registered_twice_is_refused() {
        let db = test_db().await;
        let alice = user(&db.pool, "alice").await;
        let passkey = passkey_for(&alice);
        add(&db.pool, &alice, &passkey, "Laptop", now())
            .await
            .expect("add");

        let again = add(&db.pool, &alice, &passkey, "Again", now()).await;

        assert!(matches!(again, Err(OkfError::Refused { .. })), "{again:?}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn renaming_or_removing_another_users_passkey_is_not_found() {
        let db = test_db().await;
        let alice = user(&db.pool, "alice").await;
        let bob = user(&db.pool, "bob").await;
        let passkey = passkey_for(&alice);
        add(&db.pool, &alice, &passkey, "Laptop", now())
            .await
            .expect("add");
        passwords::set(&db.pool, &alice, "violet-lantern-orbit-42", now())
            .await
            .expect("password");
        let id = URL_SAFE_NO_PAD.encode(passkey.cred_id().as_slice());

        let renamed = rename(&db.pool, &bob, &id, "Mine").await;
        let removed = remove(&db.pool, &bob, &id).await;
        let garbled = remove(&db.pool, &alice, "not base64!").await;
        let nobody = remove(&db.pool, "00000000-0000-0000-0000-000000000000", &id).await;

        assert!(
            matches!(renamed, Err(OkfError::NotFound { .. })),
            "{renamed:?}"
        );
        assert!(
            matches!(removed, Err(OkfError::NotFound { .. })),
            "{removed:?}"
        );
        assert!(
            matches!(garbled, Err(OkfError::NotFound { .. })),
            "{garbled:?}"
        );
        assert!(
            matches!(&nobody, Err(OkfError::NotFound { path }) if path.ends_with(&id)),
            "{nobody:?}"
        );
        rename(&db.pool, &alice, &id, "Work laptop")
            .await
            .expect("rename");
        assert_eq!(
            list(&db.pool, &alice).await.expect("list")[0].name,
            "Work laptop"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_last_sign_in_method_cannot_be_removed() {
        let db = test_db().await;
        let alice = user(&db.pool, "alice").await;
        let passkey = passkey_for(&alice);
        add(&db.pool, &alice, &passkey, "Laptop", now())
            .await
            .expect("add");
        let id = URL_SAFE_NO_PAD.encode(passkey.cred_id().as_slice());

        let refused = remove(&db.pool, &alice, &id).await;
        let kept = list(&db.pool, &alice).await.expect("list").len();
        passwords::set(&db.pool, &alice, "violet-lantern-orbit-42", now())
            .await
            .expect("password");
        let removed = remove(&db.pool, &alice, &id).await;

        assert!(
            matches!(refused, Err(OkfError::Refused { .. })),
            "{refused:?}"
        );
        assert_eq!(kept, 1, "the refused removal changed nothing");
        removed.expect("removed once a password exists");
        assert!(list(&db.pool, &alice).await.expect("list").is_empty());
    }
}

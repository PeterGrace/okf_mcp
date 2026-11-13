//! Users' linked OIDC identities (`user_identities`), matched by
//! `(provider, subject)`. A user links at most one account per provider.
//! Identities are not part of the key cache, so nothing here bumps
//! `admin_generation`.
//!
//! Every write that links or unlinks takes the user's row first (`FOR NO
//! KEY UPDATE`), then the identity, session or token rows: the lock order
//! of `email_tokens`' module docs, so a link, an unlink and a password
//! reset of one user run one at a time. [`touch`] on its own is the
//! exception: it only records `last_used_at` on one identity row, decides
//! nothing, and waits on no other lock, so it cannot close a lock cycle.
//!
//! An OIDC sign-in is one transaction: [`sign_in_in`] locks the user,
//! re-reads (or makes) the link and stamps the identity, then the caller
//! (`http::oidc`) starts the session, ends the browser's old one and
//! records the event (`signin::start_in`), queues the notice of a new link
//! and commits. So no session starts for an identity unlinked meanwhile,
//! and nothing that can block or fail runs after the commit: a committed
//! sign-in always reaches the browser with its cookie, and one that fails
//! or times out first changes nothing.

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgExecutor, PgPool};

use crate::config::is_provider_id;
use crate::error::OkfError;
use crate::store::catalogs::is_unique_violation;
use crate::store::sign_in_methods;
use crate::store::user_sessions;
use crate::store::users::{is_lookup_email, user_auth_columns, UserAuth};

/// A linked identity as the account page lists it (never the subject).
// `sqlx::FromRow`: read straight from a row by column name.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct IdentityInfo {
    /// The provider's id.
    pub provider: String,
    /// The email the provider gave when it was linked (empty when it gave
    /// none, or one no address can be).
    pub email_at_link: String,
    /// When it was linked.
    pub created_at: DateTime<Utc>,
    /// When it last signed in.
    pub last_used_at: Option<DateTime<Utc>>,
}

/// An identity to link to the user of a console session, from the account
/// page (spec "OIDC": whatever its email).
pub struct SessionLink<'a> {
    /// The user who started linking.
    pub user_id: &'a str,
    /// The raw id of the session that started it.
    pub session_id: &'a str,
    /// The provider's id.
    pub provider: &'a str,
    /// The ID token's `sub`.
    pub subject: &'a str,
    /// Its email, recorded as `email_at_link` (may be empty).
    pub email: &'a str,
    /// Keep the user's other sessions (otherwise they end with the link).
    pub keep_sessions: bool,
}

// Hand-written (Python's `__repr__`): the session id is a credential.
impl std::fmt::Debug for SessionLink<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionLink")
            .field("user_id", &self.user_id)
            .field("session_id", &"<redacted>")
            .field("provider", &self.provider)
            .field("keep_sessions", &self.keep_sessions)
            .finish_non_exhaustive()
    }
}

/// The user an identity is linked to.
///
/// # Arguments
/// * `pool` - the database.
/// * `provider` - the provider's id.
/// * `subject` - the ID token's `sub`.
///
/// # Returns
/// The user, or `None`.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn find(
    pool: &PgPool,
    provider: &str,
    subject: &str,
) -> Result<Option<UserAuth>, OkfError> {
    Ok(sqlx::query_as::<_, UserAuth>(concat!(
        "SELECT ",
        user_auth_columns!(),
        " FROM user_identities i JOIN users u ON u.id = i.user_id \
         WHERE i.provider = $1 AND i.subject = $2"
    ))
    .bind(provider)
    .bind(subject)
    .fetch_optional(pool)
    .await?)
}

/// Links an identity to a user. Linking the same one again is a no-op.
///
/// # Arguments
/// * `pool` - the database.
/// * `user_id` - the user.
/// * `provider` - the provider's id.
/// * `subject` - the ID token's `sub`.
/// * `email` - its email, recorded as `email_at_link` (may be empty).
/// * `now` - the server clock.
///
/// # Errors
/// [`OkfError::Refused`] if another user holds the identity, or this user
/// already links another account at this provider; [`OkfError::NotFound`]
/// for an unknown user; [`OkfError::Database`].
pub async fn link(
    pool: &PgPool,
    user_id: &str,
    provider: &str,
    subject: &str,
    email: &str,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    let mut tx = pool.begin().await?;
    lock_user(&mut tx, user_id).await?;
    link_in(&mut tx, user_id, provider, subject, email, now).await?;
    tx.commit().await?;
    Ok(())
}

/// Links an identity to the user of a console session, whatever its email,
/// and (unless `keep_sessions`) ends the user's other sessions, inside the
/// caller's transaction (which also queues the notice, then commits): the
/// user's row, then the session `FOR SHARE` (so it cannot be signed out
/// half way), then the link and the revocation.
///
/// # Arguments
/// * `conn` - the caller's transaction (`&mut tx`).
/// * `request` - the user, their session and the identity.
/// * `now` - the server clock.
///
/// # Returns
/// How many other sessions ended.
///
/// # Errors
/// [`OkfError::Unauthenticated`] when the session has ended or is not the
/// user's; as [`link`].
pub async fn link_for_session_in(
    conn: &mut PgConnection,
    request: &SessionLink<'_>,
    now: DateTime<Utc>,
) -> Result<u64, OkfError> {
    lock_user(&mut *conn, request.user_id).await?;
    if !user_sessions::hold_live(&mut *conn, request.session_id, request.user_id, now).await? {
        return Err(OkfError::Unauthenticated {
            reason: "the session that started linking has ended; sign in again".to_string(),
        });
    }
    link_in(
        &mut *conn,
        request.user_id,
        request.provider,
        request.subject,
        request.email,
        now,
    )
    .await?;
    let ended = if request.keep_sessions {
        0
    } else {
        // `&mut *conn`: the session store's SQL, in this transaction.
        user_sessions::revoke_all(&mut *conn, request.user_id, Some(request.session_id)).await?
    };
    Ok(ended)
}

/// Locks the user's row (the first lock of every link and unlink here).
///
/// # Errors
/// [`OkfError::NotFound`] for an unknown user; [`OkfError::Database`].
async fn lock_user(conn: &mut PgConnection, user_id: &str) -> Result<(), OkfError> {
    sqlx::query_scalar::<_, i32>("SELECT 1 FROM users WHERE id = $1 FOR NO KEY UPDATE")
        .bind(user_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(|| OkfError::NotFound {
            path: format!("users/{user_id}"),
        })?;
    Ok(())
}

/// [`link`]'s checks and insert, inside the caller's transaction, which
/// holds the user's row lock.
///
/// # Errors
/// As [`link`].
async fn link_in(
    conn: &mut PgConnection,
    user_id: &str,
    provider: &str,
    subject: &str,
    email: &str,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    let holder = holder_of(conn, provider, subject).await?;
    match holder.as_deref() {
        // Already linked here: nothing to change.
        Some(holder) if holder == user_id => return Ok(()),
        Some(_) => return Err(taken(provider)),
        None => {}
    }
    let another: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM user_identities WHERE user_id = $1 AND provider = $2)",
    )
    .bind(user_id)
    .bind(provider)
    .fetch_one(&mut *conn)
    .await?;
    if another {
        return Err(OkfError::Refused {
            reason: format!("another {provider} account is already linked; unlink it first"),
        });
    }
    // Text Postgres would refuse (a NUL) or that is no address is recorded
    // as no email, never a database error.
    let email_at_link = if is_lookup_email(email) { email } else { "" };
    let inserted = sqlx::query(
        "INSERT INTO user_identities (provider, subject, user_id, email_at_link, created_at) \
         VALUES ($1, $2, $3, $4, $5)",
    )
    .bind(provider)
    .bind(subject)
    .bind(user_id)
    .bind(email_at_link)
    .bind(now)
    .execute(&mut *conn)
    .await;
    match inserted {
        Ok(_) => Ok(()),
        // Another user linked it between the read and the insert.
        Err(error) if is_unique_violation(&error) => Err(taken(provider)),
        Err(error) => Err(error.into()),
    }
}

/// The refusal for an identity another user holds.
fn taken(provider: &str) -> OkfError {
    OkfError::Refused {
        reason: format!("this {provider} account is already linked to another user"),
    }
}

/// Records that an identity signed in.
///
/// # Arguments
/// * `executor` - the database: a `&PgPool`, or `&mut *tx` inside a
///   caller's transaction.
/// * `provider` - the provider's id.
/// * `subject` - the `sub`.
/// * `now` - the server clock.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn touch(
    executor: impl PgExecutor<'_>,
    provider: &str,
    subject: &str,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    sqlx::query(
        "UPDATE user_identities SET last_used_at = $3 WHERE provider = $1 AND subject = $2",
    )
    .bind(provider)
    .bind(subject)
    .bind(now)
    .execute(executor)
    .await?;
    Ok(())
}

/// An OIDC sign-in to complete: the identity the ID token proved.
#[derive(Debug, Clone, Copy)]
pub struct OidcSignIn<'a> {
    /// The provider's id.
    pub provider: &'a str,
    /// The ID token's `sub`.
    pub subject: &'a str,
    /// The token's email, when the provider verified it (trimmed): used
    /// only when the identity is not linked yet.
    pub verified_email: Option<&'a str>,
}

/// How an OIDC sign-in's lookup ended.
#[derive(Debug)]
pub enum OidcOutcome {
    /// The identity's user, locked in the caller's transaction, and whether
    /// the identity was linked by its verified email just now.
    SignedIn {
        /// The user.
        user: UserAuth,
        /// The identity was linked in this sign-in.
        linked: bool,
    },
    /// No user for this identity or verified email (nothing changed).
    NoAccount,
    /// The identity was unlinked (or its user deleted) while the sign-in
    /// waited for the user's row (nothing changed).
    Unlinked,
}

/// The first part of an OIDC sign-in, inside the caller's transaction. It
/// finds the user by `(provider, sub)`, else by the verified email (linking
/// the identity), locks that user's row, re-reads the link under the lock
/// and stamps `last_used_at`. The caller then starts the session
/// (`signin::start_in`) and commits. A returning identity always signs in
/// as its own user, whatever email it now carries.
///
/// # Arguments
/// * `conn` - the caller's transaction (`&mut tx`).
/// * `request` - the identity.
/// * `now` - the server clock.
///
/// # Returns
/// The outcome; only [`OidcOutcome::SignedIn`] changed anything.
///
/// # Errors
/// [`OkfError::Refused`] when the verified email's user already links
/// another account at this provider (or another user took the identity
/// meanwhile); [`OkfError::Database`].
pub async fn sign_in_in(
    conn: &mut PgConnection,
    request: &OidcSignIn<'_>,
    now: DateTime<Utc>,
) -> Result<OidcOutcome, OkfError> {
    let holder = holder_of(&mut *conn, request.provider, request.subject).await?;
    let (user, linked) = match holder {
        Some(holder) => {
            let Some(user) = lock_user_auth(&mut *conn, &holder).await? else {
                return Ok(OidcOutcome::Unlinked);
            };
            // A fresh read under the lock: an unlink that committed while
            // this waited is seen here.
            let still = holder_of(&mut *conn, request.provider, request.subject).await?;
            if still.as_deref() != Some(holder.as_str()) {
                return Ok(OidcOutcome::Unlinked);
            }
            (user, false)
        }
        None => {
            let verified = request
                .verified_email
                .filter(|email| is_lookup_email(email));
            let Some(email) = verified else {
                return Ok(OidcOutcome::NoAccount);
            };
            // `$1::citext`: a case-insensitive match. The lock re-checks the
            // `WHERE` on the newest row, so an address changed meanwhile
            // drops out.
            let found = sqlx::query_as::<_, UserAuth>(concat!(
                "SELECT ",
                user_auth_columns!(),
                " FROM users u WHERE u.email = $1::citext FOR NO KEY UPDATE"
            ))
            .bind(email)
            .fetch_optional(&mut *conn)
            .await?;
            let Some(user) = found else {
                return Ok(OidcOutcome::NoAccount);
            };
            link_in(
                &mut *conn,
                &user.id,
                request.provider,
                request.subject,
                email,
                now,
            )
            .await?;
            (user, true)
        }
    };
    touch(&mut *conn, request.provider, request.subject, now).await?;
    Ok(OidcOutcome::SignedIn { user, linked })
}

/// The user an identity is linked to, read without a lock.
async fn holder_of(
    conn: &mut PgConnection,
    provider: &str,
    subject: &str,
) -> Result<Option<String>, OkfError> {
    Ok(sqlx::query_scalar(
        "SELECT user_id FROM user_identities WHERE provider = $1 AND subject = $2",
    )
    .bind(provider)
    .bind(subject)
    .fetch_optional(&mut *conn)
    .await?)
}

/// Locks a user's row and reads them, or `None` once deleted.
async fn lock_user_auth(
    conn: &mut PgConnection,
    user_id: &str,
) -> Result<Option<UserAuth>, OkfError> {
    Ok(sqlx::query_as::<_, UserAuth>(concat!(
        "SELECT ",
        user_auth_columns!(),
        " FROM users u WHERE u.id = $1 FOR NO KEY UPDATE"
    ))
    .bind(user_id)
    .fetch_optional(&mut *conn)
    .await?)
}

/// A user's linked identities, by provider.
///
/// # Arguments
/// * `pool` - the database.
/// * `user_id` - the user.
///
/// # Returns
/// The listing.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn list(pool: &PgPool, user_id: &str) -> Result<Vec<IdentityInfo>, OkfError> {
    Ok(sqlx::query_as::<_, IdentityInfo>(
        "SELECT provider, email_at_link::text AS email_at_link, created_at, last_used_at \
         FROM user_identities WHERE user_id = $1 ORDER BY provider",
    )
    .bind(user_id)
    .fetch_all(pool)
    .await?)
}

/// Unlinks the user's account at `provider`, unless it is their last
/// sign-in method.
///
/// # Arguments
/// * `pool` - the database.
/// * `user_id` - the user.
/// * `provider` - the provider's id.
///
/// # Errors
/// [`OkfError::NotFound`] when nothing is linked there (or the user is
/// unknown); [`OkfError::Refused`] for the last sign-in method;
/// [`OkfError::Database`].
pub async fn unlink(pool: &PgPool, user_id: &str, provider: &str) -> Result<(), OkfError> {
    let not_linked = || OkfError::NotFound {
        path: format!("me/identities/{provider}"),
    };
    // Only a well-formed provider id can be linked, so anything else (a
    // NUL, which Postgres would refuse with a 500, included) is unknown.
    if !is_provider_id(provider) {
        return Err(not_linked());
    }
    let mut tx = pool.begin().await?;
    // Locks the user's row first, so two removals of different methods run
    // one at a time. An unknown user has nothing linked.
    let methods = match sign_in_methods::count(&mut tx, user_id).await {
        Err(OkfError::NotFound { .. }) => return Err(not_linked()),
        other => other?,
    };
    let linked: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM user_identities WHERE user_id = $1 AND provider = $2",
    )
    .bind(user_id)
    .bind(provider)
    .fetch_one(&mut *tx)
    .await?;
    if linked == 0 {
        return Err(not_linked());
    }
    if methods - linked < 1 {
        return Err(OkfError::Refused {
            reason: "this is your last way to sign in; add a password or a passkey first"
                .to_string(),
        });
    }
    sqlx::query("DELETE FROM user_identities WHERE user_id = $1 AND provider = $2")
        .bind(user_id)
        .bind(provider)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use chrono::{DateTime, TimeZone, Utc};
    use sqlx::PgPool;

    use super::*;
    use crate::db::test_db;
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
    async fn a_linked_identity_finds_its_user_and_lists_with_its_email() {
        let db = test_db().await;
        let alice = user(&db.pool, "alice").await;

        link(
            &db.pool,
            &alice,
            "example",
            "sub-1",
            "alice@example.com",
            now(),
        )
        .await
        .expect("link");
        touch(&db.pool, "example", "sub-1", now())
            .await
            .expect("touch");

        let found = find(&db.pool, "example", "sub-1")
            .await
            .expect("find")
            .expect("linked");
        assert_eq!(found.id, alice);
        assert_eq!(found.name, "alice");
        assert!(find(&db.pool, "example", "sub-2")
            .await
            .expect("find")
            .is_none());
        let listed = list(&db.pool, &alice).await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].provider, "example");
        assert_eq!(listed[0].email_at_link, "alice@example.com");
        assert_eq!(listed[0].last_used_at, Some(now()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn linking_again_is_a_no_op_but_another_users_subject_is_refused() {
        let db = test_db().await;
        let alice = user(&db.pool, "alice").await;
        let bob = user(&db.pool, "bob").await;
        link(
            &db.pool,
            &alice,
            "example",
            "sub-1",
            "alice@example.com",
            now(),
        )
        .await
        .expect("link");

        link(
            &db.pool,
            &alice,
            "example",
            "sub-1",
            "alice@example.com",
            now(),
        )
        .await
        .expect("again");
        let taken = link(&db.pool, &bob, "example", "sub-1", "bob@example.com", now()).await;
        let second = link(
            &db.pool,
            &alice,
            "example",
            "sub-2",
            "alice@example.com",
            now(),
        )
        .await;

        assert!(matches!(taken, Err(OkfError::Refused { .. })), "{taken:?}");
        assert!(
            matches!(second, Err(OkfError::Refused { .. })),
            "one account per provider: {second:?}"
        );
        assert_eq!(list(&db.pool, &alice).await.expect("list").len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unlinking_refuses_the_last_sign_in_method_and_an_unlinked_provider() {
        let db = test_db().await;
        let alice = user(&db.pool, "alice").await;
        link(
            &db.pool,
            &alice,
            "example",
            "sub-1",
            "alice@example.com",
            now(),
        )
        .await
        .expect("link");

        let last = unlink(&db.pool, &alice, "example").await;
        let missing = unlink(&db.pool, &alice, "other").await;
        passwords::set(&db.pool, &alice, "violet-lantern-orbit-42", now())
            .await
            .expect("password");
        unlink(&db.pool, &alice, "example")
            .await
            .expect("unlinked once a password exists");

        assert!(matches!(last, Err(OkfError::Refused { .. })), "{last:?}");
        assert!(
            matches!(missing, Err(OkfError::NotFound { .. })),
            "{missing:?}"
        );
        assert!(list(&db.pool, &alice).await.expect("list").is_empty());
    }

    /// An OIDC sign-in of `subject` with this verified email.
    fn oidc<'a>(subject: &'a str, email: Option<&'a str>) -> OidcSignIn<'a> {
        OidcSignIn {
            provider: "example",
            subject,
            verified_email: email,
        }
    }

    /// [`sign_in_in`] in a transaction of its own, committed.
    async fn sign_in(
        pool: &PgPool,
        request: &OidcSignIn<'_>,
        now: DateTime<Utc>,
    ) -> Result<OidcOutcome, OkfError> {
        let mut tx = pool.begin().await?;
        let outcome = sign_in_in(&mut tx, request, now).await?;
        tx.commit().await?;
        Ok(outcome)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn signing_in_by_email_links_case_insensitively_and_never_errs_on_bad_text() {
        let db = test_db().await;
        let alice = user(&db.pool, "alice").await;
        users::set_email(&db.pool, "alice", Some("alice@example.com"))
            .await
            .expect("email");

        let linked = sign_in(&db.pool, &oidc("sub-1", Some("Alice@Example.com")), now())
            .await
            .expect("sign in");
        let again = sign_in(&db.pool, &oidc("sub-1", None), now())
            .await
            .expect("a returning identity");
        let nobody = sign_in(&db.pool, &oidc("sub-2", Some("nobody@example.com")), now())
            .await
            .expect("no user");
        let nul = sign_in(&db.pool, &oidc("sub-3", Some("alice\0@example.com")), now())
            .await
            .expect("a NUL names nobody");
        let unverified = sign_in(&db.pool, &oidc("sub-4", None), now())
            .await
            .expect("no email");

        assert!(
            matches!(&linked, OidcOutcome::SignedIn { user, linked: true, .. } if user.id == alice),
            "{linked:?}"
        );
        assert!(
            matches!(&again, OidcOutcome::SignedIn { linked: false, .. }),
            "{again:?}"
        );
        for outcome in [&nobody, &nul, &unverified] {
            assert!(matches!(outcome, OidcOutcome::NoAccount), "{outcome:?}");
        }
        let listed = list(&db.pool, &alice).await.expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].email_at_link, "Alice@Example.com");
        assert_eq!(listed[0].last_used_at, Some(now()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_email_postgres_would_refuse_is_linked_as_no_email() {
        let db = test_db().await;
        let alice = user(&db.pool, "alice").await;

        link(
            &db.pool,
            &alice,
            "example",
            "sub-1",
            "alice\0@example.com",
            now(),
        )
        .await
        .expect("linked without an email, not a database error");

        assert_eq!(
            list(&db.pool, &alice).await.expect("list")[0].email_at_link,
            ""
        );
    }

    #[test]
    fn a_session_link_never_prints_its_session_id() {
        let request = SessionLink {
            user_id: "user-1",
            session_id: "session-secret",
            provider: "example",
            subject: "sub-1",
            email: "",
            keep_sessions: false,
        };

        let printed = format!("{request:?}");

        assert!(!printed.contains("session-secret"), "{printed}");
    }
}

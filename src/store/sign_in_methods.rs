//! How many ways a user can sign in: the guard that keeps the last one
//! from being removed (spec "Self-service": 409).

use sqlx::PgConnection;

use crate::error::OkfError;

/// A user's sign-in methods: a password (0 or 1), plus passkeys, plus
/// linked identities. Locks the user's row `FOR UPDATE` first, so two
/// removals of different methods run one at a time and cannot both see a
/// count of two. Call it inside the transaction that removes a method,
/// before touching the method's own rows (lock order: the user first;
/// `store::email_tokens`).
///
/// # Arguments
/// * `conn` - the caller's transaction (`&mut tx`).
/// * `user_id` - the user's id.
///
/// # Returns
/// The count.
///
/// # Errors
/// [`OkfError::NotFound`] for an unknown user; [`OkfError::Database`].
pub async fn count(conn: &mut PgConnection, user_id: &str) -> Result<i64, OkfError> {
    // `&mut *conn` reborrows the connection for one query, so it can be used
    // again for the next (a `&mut` can be lent out but not copied).
    let has_password: Option<bool> =
        sqlx::query_scalar("SELECT password_hash IS NOT NULL FROM users WHERE id = $1 FOR UPDATE")
            .bind(user_id)
            .fetch_optional(&mut *conn)
            .await?;
    let Some(has_password) = has_password else {
        return Err(OkfError::NotFound {
            path: format!("users/{user_id}"),
        });
    };
    let others: i64 = sqlx::query_scalar(
        "SELECT (SELECT count(*) FROM user_passkeys WHERE user_id = $1) \
              + (SELECT count(*) FROM user_identities WHERE user_id = $1)",
    )
    .bind(user_id)
    .fetch_one(&mut *conn)
    .await?;
    Ok(others + i64::from(has_password))
}

#[cfg(test)]
mod tests {
    use chrono::{TimeZone, Utc};

    use super::*;
    use crate::db::test_db;
    use crate::store::passkeys::{self, testing::passkey_for};
    use crate::store::{passwords, users};

    #[tokio::test(flavor = "multi_thread")]
    async fn count_adds_a_password_passkeys_and_identities() {
        let db = test_db().await;
        let now = Utc
            .with_ymd_and_hms(2026, 11, 12, 9, 0, 0)
            .single()
            .expect("a time");
        users::create(
            &db.pool,
            &users::NewUser {
                name: "alice",
                display_name: "",
            },
            now,
        )
        .await
        .expect("create");
        let alice: String = sqlx::query_scalar("SELECT id FROM users WHERE name = 'alice'")
            .fetch_one(&db.pool)
            .await
            .expect("id");
        let mut conn = db.pool.acquire().await.expect("a connection");
        assert_eq!(count(&mut conn, &alice).await.expect("count"), 0);

        passwords::set(&db.pool, &alice, "violet-lantern-orbit-42", now)
            .await
            .expect("password");
        passkeys::add(&db.pool, &alice, &passkey_for(&alice), "Laptop", now)
            .await
            .expect("passkey");
        sqlx::query(
            "INSERT INTO user_identities (provider, subject, user_id, email_at_link, created_at) \
             VALUES ('example', 'sub-1', $1, 'alice@example.com', now())",
        )
        .bind(&alice)
        .execute(&db.pool)
        .await
        .expect("identity");

        assert_eq!(count(&mut conn, &alice).await.expect("count"), 3);
        let unknown = count(&mut conn, "00000000-0000-0000-0000-000000000000").await;
        assert!(
            matches!(unknown, Err(OkfError::NotFound { .. })),
            "{unknown:?}"
        );
    }
}

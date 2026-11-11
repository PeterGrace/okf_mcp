//! `sign_in_codes`: admin-issued one-time codes, each bound to a grant
//! template (catalogs, access, default catalog, actor). Task 2 prunes;
//! Task 4 creates, lists, finds, revokes and redeems.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::error::OkfError;

/// Deletes the unredeemed codes that expired before `expired_before`.
/// Redeemed codes stay: they record which code made which grant.
///
/// # Arguments
/// * `pool` - the database.
/// * `expired_before` - the cut-off (the poll loop passes a week ago).
///
/// # Returns
/// How many were deleted.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn prune(pool: &PgPool, expired_before: DateTime<Utc>) -> Result<u64, OkfError> {
    let deleted =
        sqlx::query("DELETE FROM sign_in_codes WHERE redeemed_at IS NULL AND expires_at < $1")
            .bind(expired_before)
            .execute(pool)
            .await?;
    Ok(deleted.rows_affected())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeDelta, TimeZone};

    use crate::db::test_db;
    use crate::store::catalogs;
    use crate::store::keys::Access;
    use crate::store::oauth::testing::{insert_client, insert_grant};

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prune_deletes_only_old_unredeemed_codes() {
        let db = test_db().await;
        catalogs::create(&db.pool, "main", "", now()).await.unwrap();
        let client = insert_client(
            &db.pool,
            "https://client.example.com/client.json",
            "Example",
            now(),
        )
        .await;
        let grant = insert_grant(&db.pool, &client, Access::Read, &["main"], now()).await;
        let old = now() - TimeDelta::days(8);
        for (byte, expires_at, grant_id) in [
            (1u8, old, None),
            (2, old, Some(grant.as_str())),
            (3, now() - TimeDelta::days(6), None),
            (4, now() + TimeDelta::hours(1), None),
        ] {
            sqlx::query(
                "INSERT INTO sign_in_codes \
                 (name, sha256, access, all_catalogs, actor, created_at, expires_at, redeemed_at, grant_id) \
                 VALUES ('laptop', $1, 'read', true, 'example-actor', $2, $3, $4, $5)",
            )
            .bind([byte; 32].as_slice())
            .bind(expires_at - TimeDelta::hours(24))
            .bind(expires_at)
            .bind(grant_id.map(|_| expires_at - TimeDelta::hours(1)))
            .bind(grant_id)
            .execute(&db.pool)
            .await
            .unwrap();
        }

        let pruned = prune(&db.pool, now() - TimeDelta::days(7)).await.unwrap();

        assert_eq!(pruned, 1);
        let left: Vec<Vec<u8>> =
            sqlx::query_scalar("SELECT sha256 FROM sign_in_codes ORDER BY sha256")
                .fetch_all(&db.pool)
                .await
                .unwrap();
        assert_eq!(left, [vec![2u8; 32], vec![3u8; 32], vec![4u8; 32]]);
    }
}

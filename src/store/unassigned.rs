//! Rows from before migration 0009 that no admin has assigned yet: keys and
//! grants with no user, sign-in codes with no user, catalogs with no owner.
//! Every create path requires a user or owner, so only legacy rows show up.

use sqlx::PgPool;

use crate::error::OkfError;
use crate::store::keys::{self, KeyRecord};
use crate::store::oauth::codes::{self, SignInCodeRecord};

/// Everything still unassigned.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Unassigned {
    /// Keys and grants with no user, live and revoked, in `keys::list` order.
    pub keys: Vec<KeyRecord>,
    /// Sign-in codes with no user, in `codes::list` order.
    pub codes: Vec<SignInCodeRecord>,
    /// Catalogs with no owner, by name.
    pub catalogs: Vec<String>,
}

impl Unassigned {
    /// Whether nothing is left to assign (the gate for the `NOT NULL`
    /// follow-up).
    ///
    /// # Returns
    /// `true` when every list is empty.
    pub fn is_empty(&self) -> bool {
        self.keys.is_empty() && self.codes.is_empty() && self.catalogs.is_empty()
    }
}

/// Every unassigned row.
///
/// # Arguments
/// * `pool` - the database.
///
/// # Returns
/// The unassigned keys and grants, codes and catalogs.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn list(pool: &PgPool) -> Result<Unassigned, OkfError> {
    // The key and code listings already carry `user`; filtering them reuses
    // their joins instead of repeating them. The tables hold at most a few
    // thousand rows, so reading them whole is cheap.
    let keys = keys::list(pool)
        .await?
        .into_iter()
        .filter(|key| key.user.is_none())
        .collect();
    let codes = codes::list(pool)
        .await?
        .into_iter()
        .filter(|code| code.user.is_none())
        .collect();
    let catalogs =
        sqlx::query_scalar("SELECT name FROM catalogs WHERE owner_team_id IS NULL ORDER BY name")
            .fetch_all(pool)
            .await?;
    Ok(Unassigned {
        keys,
        codes,
        catalogs,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, TimeDelta, TimeZone, Utc};

    use crate::db::{test_db, TEST_TEAM, TEST_USER};
    use crate::store::catalogs;
    use crate::store::keys::{insert_legacy_test_key, Access, Grants, NewKey};
    use crate::store::oauth::codes::NewSignInCode;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap()
    }

    fn code(name: &str) -> NewSignInCode<'_> {
        NewSignInCode {
            name,
            access: Access::Read,
            grants: Grants::All,
            default_catalog: None,
            actor: "",
            expires_at: now() + TimeDelta::hours(1),
            user: TEST_USER,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unassigned_lists_exactly_the_rows_with_no_user_or_owner() {
        // Arrange: one legacy and one owned row of each kind.
        let db = test_db().await;
        insert_legacy_test_key(&db.pool, "legacy", Access::Read, "legacy", now()).await;
        let owned = NewKey {
            name: "owned",
            access: Access::Read,
            maintenance_report: false,
            grants: Grants::All,
            default_catalog: None,
            actor: "owned",
            user: TEST_USER,
            self_issued: false,
            expires_at: None,
            levels: &[],
        };
        keys::create(
            &db.pool,
            &owned,
            &crate::store::audit::AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let legacy_code = codes::create(
            &db.pool,
            &code("old"),
            &crate::store::audit::AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        sqlx::query("UPDATE sign_in_codes SET user_id = NULL WHERE id = $1")
            .bind(&legacy_code.id)
            .execute(&db.pool)
            .await
            .unwrap();
        codes::create(
            &db.pool,
            &code("new"),
            &crate::store::audit::AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        sqlx::query("INSERT INTO catalogs (name, created_at) VALUES ('orphan', now())")
            .execute(&db.pool)
            .await
            .unwrap();
        catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog {
                name: "owned",
                description: "",
                owner_team: TEST_TEAM,
            },
            &crate::store::audit::AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();

        let found = list(&db.pool).await.unwrap();

        let key_ids: Vec<&str> = found.keys.iter().map(|key| key.id.as_str()).collect();
        assert_eq!(key_ids, ["legacy"]);
        let code_ids: Vec<&str> = found.codes.iter().map(|code| code.id.as_str()).collect();
        assert_eq!(code_ids, [legacy_code.id.as_str()]);
        assert_eq!(found.catalogs, ["orphan"]);
        assert!(!found.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_fresh_database_has_nothing_unassigned() {
        let db = test_db().await;

        let found = list(&db.pool).await.unwrap();

        assert!(found.is_empty(), "{found:?}");
    }
}

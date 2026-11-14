//! Self-issued OAuth grants (spec "OAuth consent as the user"): what a
//! signed-in person approves on the consent page. Each one is an `api_keys`
//! row of kind `oauth` with `self_issued = true`, the person's user id and
//! one level per catalog. The key cache caps it by the user's ceiling on
//! every reload (Task 3), so lowering the ceiling later lowers it.
//!
//! Grants redeemed from admin-minted sign-in codes stay in
//! `store::oauth::codes::redeem`: they are admin-issued and uncapped.
//!
//! **Lock order** (`store::email_tokens`' module docs): the user's row
//! first, then the session's, then the inserts.

use std::collections::BTreeMap;

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

use crate::error::OkfError;
use crate::keys;
use crate::maintenance::types::new_id;
use crate::store::catalogs::bump_admin_generation;
use crate::store::ceiling;
use crate::store::keys::{resolve_grants, Access, Grants};
use crate::store::oauth::clients::ClientRecord;
use crate::store::oauth::codes::client_host;

/// The longest actor a client's name becomes (client names are untrusted
/// metadata and may be long).
const MAX_ACTOR_CHARS: usize = 100;

/// A grant to create from a signed-in person's consent.
// `Debug` by hand: `session_id` signs its holder in.
#[derive(Clone, Copy, PartialEq)]
pub struct NewSelfIssuedGrant<'a> {
    /// The signed-in user's id; the grant belongs to it.
    pub user_id: &'a str,
    /// The raw id of the console session approving (the cookie's value).
    /// The grant is made only while it is live.
    pub session_id: &'a str,
    /// The client being authorized (already stored).
    pub client: &'a ClientRecord,
    /// Each chosen catalog's level, `Read` or `Write`. Checked again here
    /// against the user's ceiling.
    pub levels: &'a BTreeMap<String, Access>,
    /// The default catalog; must be one of `levels`.
    pub default_catalog: Option<&'a str>,
}

impl std::fmt::Debug for NewSelfIssuedGrant<'_> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NewSelfIssuedGrant")
            .field("user_id", &self.user_id)
            .field("session_id", &"<redacted>")
            .field("client_id", &self.client.client_id)
            .field("levels", &self.levels)
            .field("default_catalog", &self.default_catalog)
            .finish()
    }
}

/// A self-issued grant's name: `<user>.<client host>-<8 random hex>`. The
/// user leads, as a code's name does for a redeemed grant. The suffix tells
/// apart two consents of one user to one client.
///
/// # Arguments
/// * `user` - the user's name (`[a-z0-9-]+`).
/// * `client` - the client.
///
/// # Returns
/// A name that passes the `api_keys` name check.
///
/// # Errors
/// [`OkfError::Io`] if the OS random source fails.
fn grant_name(user: &str, client: &ClientRecord) -> Result<String, OkfError> {
    let id = new_id()?;
    // `new_id` is 32 hex characters; `get(..8)` returns `None` rather than
    // panicking if it ever were shorter.
    let suffix = id.get(..8).unwrap_or(&id);
    Ok(format!("{user}.{}-{suffix}", client_host(client)))
}

/// A self-issued grant's actor: the client's name, trimmed and cut to
/// [`MAX_ACTOR_CHARS`], or its host when it gave none.
///
/// # Arguments
/// * `client` - the client.
///
/// # Returns
/// A non-empty actor.
fn actor_for(client: &ClientRecord) -> String {
    match client.client_name.as_deref().map(str::trim) {
        // `chars().take(n)` cuts on character boundaries, never inside a
        // multi-byte character as a byte slice could.
        Some(name) if !name.is_empty() => name.chars().take(MAX_ACTOR_CHARS).collect(),
        Some(_) | None => client_host(client),
    }
}

/// Checks a consent's levels before any query: at least one catalog, none
/// `Admin`.
///
/// # Arguments
/// * `levels` - the chosen levels.
///
/// # Returns
/// The grant's own level: the highest chosen.
///
/// # Errors
/// [`OkfError::Refused`].
fn grant_access(levels: &BTreeMap<String, Access>) -> Result<Access, OkfError> {
    // The schema also refuses an admin self-issued row; checking here gives
    // a reason instead of a CHECK violation.
    if levels.values().any(|&access| access == Access::Admin) {
        return Err(OkfError::Refused {
            reason: "a grant from consent is never admin".to_string(),
        });
    }
    // `max` is `None` only for no catalogs.
    levels
        .values()
        .copied()
        .max()
        .ok_or_else(|| OkfError::Refused {
            reason: "a grant from consent needs at least one catalog".to_string(),
        })
}

/// Locks the user's row and then the approving session's, and checks both
/// still stand.
///
/// The user is held `FOR SHARE` until commit: a password reset, an
/// admin-flag change or a delete (each of which ends the user's sessions)
/// waits for this grant, or commits first and this finds the session gone.
/// The session is held `FOR KEY SHARE`, so a sign-out waits too, while a
/// concurrent `touch` (a non-key update) does not.
///
/// # Arguments
/// * `conn` - the open transaction.
/// * `new` - the user and the session.
/// * `now` - the server clock.
///
/// # Returns
/// The user's name.
///
/// # Errors
/// [`OkfError::Unauthenticated`] when the user or the session is gone or
/// ended; [`OkfError::Database`].
async fn lock_signed_in(
    conn: &mut PgConnection,
    new: &NewSelfIssuedGrant<'_>,
    now: DateTime<Utc>,
) -> Result<String, OkfError> {
    let signed_out = || OkfError::Unauthenticated {
        reason: "the session that approved this has ended; sign in again".to_string(),
    };
    let name: String = sqlx::query_scalar("SELECT name FROM users WHERE id = $1 FOR SHARE")
        .bind(new.user_id)
        .fetch_optional(&mut *conn)
        .await?
        .ok_or_else(signed_out)?;
    let live: Option<i32> = sqlx::query_scalar(
        "SELECT 1 FROM user_sessions \
         WHERE id_hash = $1 AND user_id = $2 AND expires_at > $3 FOR KEY SHARE",
    )
    .bind(keys::hash(new.session_id).as_slice())
    .bind(new.user_id)
    .bind(now)
    .fetch_optional(&mut *conn)
    .await?;
    live.map(|_| name).ok_or_else(signed_out)
}

/// Creates a self-issued OAuth grant and its per-catalog levels, and bumps
/// the administrative generation so the key cache loads it, in one
/// transaction. Under the user's row lock it re-checks that the approving
/// session is live and re-reads the ceiling, refusing any level above it.
///
/// # Arguments
/// * `pool` - the database.
/// * `new` - the user, session, client, levels and default catalog.
/// * `now` - the creation time (server clock).
///
/// # Returns
/// The grant's id (its `api_keys.id`).
///
/// # Errors
/// [`OkfError::Refused`] for no catalog, an `Admin` level, a level above
/// the ceiling now, or a default that is not chosen;
/// [`OkfError::Unauthenticated`] when the user or the session is gone;
/// [`OkfError::CatalogNotSelected`] for a catalog deleted meanwhile;
/// [`OkfError::Io`] if the OS random source fails; [`OkfError::Database`].
pub async fn create_self_issued(
    pool: &PgPool,
    new: &NewSelfIssuedGrant<'_>,
    now: DateTime<Utc>,
) -> Result<String, OkfError> {
    let access = grant_access(new.levels)?;
    let names: Vec<String> = new.levels.keys().cloned().collect();
    // The same order as `names`: both iterate the map in key order.
    let levels: Vec<&'static str> = new.levels.values().map(|access| access.as_str()).collect();
    // Dropping `tx` without `commit` (every early return below) rolls back.
    let mut tx = pool.begin().await?;
    let user = lock_signed_in(&mut tx, new, now).await?;
    let ceiling = ceiling::ceiling_in(&mut tx, new.user_id).await?;
    // `is_none_or`: absent from the ceiling, or allowed below the choice.
    if let Some(catalog) = new.levels.iter().find_map(|(catalog, &level)| {
        ceiling
            .get(catalog)
            .is_none_or(|&allowed| level > allowed)
            .then_some(catalog)
    }) {
        return Err(OkfError::Refused {
            reason: format!("{user} may not grant that level of {catalog:?}"),
        });
    }
    let name = grant_name(&user, new.client)?;
    let (catalog_ids, default_catalog_id) = resolve_grants(
        &mut tx,
        "grant",
        &name,
        Grants::Catalogs(&names),
        new.default_catalog,
    )
    .await?;
    let grant_id: String = sqlx::query_scalar(
        "INSERT INTO api_keys \
         (name, sha256, kind, client_id, access, maintenance_report, all_catalogs, \
          default_catalog_id, actor, created_at, user_id, self_issued) \
         VALUES ($1, NULL, 'oauth', $2, $3, false, false, $4, $5, $6, $7, true) RETURNING id",
    )
    .bind(&name)
    .bind(&new.client.client_id)
    .bind(access.as_str())
    .bind(default_catalog_id)
    .bind(actor_for(new.client))
    .bind(now)
    .bind(new.user_id)
    .fetch_one(&mut *tx)
    .await?;
    // Two arrays unnested side by side give one row per catalog with its
    // level, like `zip` in Python.
    sqlx::query(
        "INSERT INTO key_catalogs (key_id, catalog_id, access) \
         SELECT $1, chosen.id, chosen.access \
         FROM unnest($2::bigint[], $3::text[]) AS chosen (id, access)",
    )
    .bind(&grant_id)
    .bind(&catalog_ids)
    .bind(&levels)
    .execute(&mut *tx)
    .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(grant_id)
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;
    use std::time::Duration;

    use chrono::{DateTime, Utc};
    use sqlx::PgPool;

    use super::{create_self_issued, NewSelfIssuedGrant};
    use crate::db::test_db;
    use crate::error::OkfError;
    use crate::store::audit::AuditActor;
    use crate::store::ceiling::NewEntitlement;
    use crate::store::keys::{self as key_rows, Access, KeyKind};
    use crate::store::oauth::clients::{ClientKind, ClientRecord};
    use crate::store::oauth::codes::insert_test_client;
    use crate::store::users::{self, NewUser};
    use crate::store::{catalogs, ceiling, user_sessions};

    fn now() -> DateTime<Utc> {
        "2026-11-12T12:00:00Z".parse().expect("a time")
    }

    /// `main` and `other`, the test client, and alice with these
    /// entitlements and a live session.
    ///
    /// # Returns
    /// The client, alice's id and her raw session id.
    async fn alice_with(
        pool: &PgPool,
        grants: &[(&str, Access)],
    ) -> (ClientRecord, String, String) {
        catalogs::create(
            pool,
            &crate::store::catalogs::NewCatalog {
                name: "main",
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &crate::store::audit::AuditActor::cli(),
            now(),
        )
        .await
        .expect("main");
        catalogs::create(
            pool,
            &crate::store::catalogs::NewCatalog {
                name: "other",
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &crate::store::audit::AuditActor::cli(),
            now(),
        )
        .await
        .expect("other");
        users::create(
            pool,
            &NewUser {
                name: "alice",
                display_name: "",
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .expect("alice");
        for (catalog, access) in grants {
            ceiling::entitle(
                pool,
                &NewEntitlement {
                    user: "alice",
                    catalog,
                    access: *access,
                },
                &AuditActor::cli(),
                now(),
            )
            .await
            .expect("entitle");
        }
        let user_id: String = sqlx::query_scalar("SELECT id FROM users WHERE name = 'alice'")
            .fetch_one(pool)
            .await
            .expect("alice's id");
        // Sessions run on the real clock's `expires_at > now` check below,
        // so this one lasts long past the test's fixed `now`.
        let session = user_sessions::create(
            pool,
            &user_id,
            None,
            "okf-test",
            Duration::from_secs(3600),
            now(),
        )
        .await
        .expect("a session");
        let client = insert_test_client(
            pool,
            "test-client",
            ClientKind::Dcr,
            "https://client.example.com/callback",
        )
        .await;
        (client, user_id, session.id)
    }

    fn levels(pairs: &[(&str, Access)]) -> BTreeMap<String, Access> {
        pairs
            .iter()
            .map(|(catalog, access)| (catalog.to_string(), *access))
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn debug_never_prints_the_session_id() {
        let db = test_db().await;
        let (client, user_id, _) = alice_with(&db.pool, &[("main", Access::Read)]).await;
        let chosen = levels(&[("main", Access::Read)]);
        let new = NewSelfIssuedGrant {
            user_id: &user_id,
            session_id: "session-id-that-must-not-leak",
            client: &client,
            levels: &chosen,
            default_catalog: None,
        };

        let printed = format!("{new:?}");

        assert!(
            !printed.contains("session-id-that-must-not-leak"),
            "{printed}"
        );
        assert!(printed.contains("test-client"), "{printed}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_self_issued_grant_records_its_user_levels_and_client() {
        let db = test_db().await;
        let (client, user_id, session_id) = alice_with(
            &db.pool,
            &[("main", Access::Write), ("other", Access::Write)],
        )
        .await;
        let chosen = levels(&[("main", Access::Write), ("other", Access::Read)]);
        let before = catalogs::admin_generation(&db.pool)
            .await
            .expect("generation");

        let grant_id = create_self_issued(
            &db.pool,
            &NewSelfIssuedGrant {
                user_id: &user_id,
                session_id: &session_id,
                client: &client,
                levels: &chosen,
                default_catalog: Some("main"),
            },
            now(),
        )
        .await
        .expect("the grant");

        let record = key_rows::list(&db.pool)
            .await
            .expect("list")
            .into_iter()
            .find(|record| record.id == grant_id)
            .expect("the grant row");
        assert_eq!(record.kind, KeyKind::OAuth);
        assert!(record.self_issued);
        assert!(!record.all_catalogs);
        assert_eq!(record.user.as_deref(), Some("alice"));
        assert_eq!(record.access, Access::Write, "the highest catalog level");
        assert_eq!(record.catalog_access, chosen);
        assert_eq!(record.default_catalog.as_deref(), Some("main"));
        assert_eq!(record.actor, "Example Client");
        assert_eq!(record.client_id.as_deref(), Some("test-client"));
        assert!(
            record.name.starts_with("alice.client.example.com-"),
            "{}",
            record.name
        );
        assert!(
            catalogs::admin_generation(&db.pool)
                .await
                .expect("generation")
                > before
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_grant_with_no_catalog_or_an_admin_level_is_refused() {
        let db = test_db().await;
        let (client, user_id, session_id) = alice_with(&db.pool, &[("main", Access::Write)]).await;
        let none = BTreeMap::new();
        let admin = levels(&[("main", Access::Admin)]);

        for chosen in [&none, &admin] {
            let made = create_self_issued(
                &db.pool,
                &NewSelfIssuedGrant {
                    user_id: &user_id,
                    session_id: &session_id,
                    client: &client,
                    levels: chosen,
                    default_catalog: None,
                },
                now(),
            )
            .await;
            assert!(matches!(made, Err(OkfError::Refused { .. })), "{made:?}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_level_above_the_ceiling_now_is_refused_and_grants_nothing() {
        let db = test_db().await;
        let (client, user_id, session_id) = alice_with(&db.pool, &[("main", Access::Read)]).await;
        let grant = |chosen: BTreeMap<String, Access>| {
            let (client, user_id, session_id) = (&client, &user_id, &session_id);
            let pool = &db.pool;
            async move {
                create_self_issued(
                    pool,
                    &NewSelfIssuedGrant {
                        user_id,
                        session_id,
                        client,
                        levels: &chosen,
                        default_catalog: None,
                    },
                    now(),
                )
                .await
            }
        };

        let above = grant(levels(&[("main", Access::Write)])).await;
        let outside = grant(levels(&[("main", Access::Read), ("other", Access::Read)])).await;

        assert!(matches!(above, Err(OkfError::Refused { .. })), "{above:?}");
        assert!(
            matches!(outside, Err(OkfError::Refused { .. })),
            "{outside:?}"
        );
        let grants = key_rows::list(&db.pool)
            .await
            .expect("list")
            .into_iter()
            .filter(|record| record.kind == KeyKind::OAuth)
            .count();
        assert_eq!(grants, 0);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_ended_or_another_users_session_cannot_grant() {
        let db = test_db().await;
        let (client, user_id, session_id) = alice_with(&db.pool, &[("main", Access::Read)]).await;
        let chosen = levels(&[("main", Access::Read)]);
        let other_user: String = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(crate::db::TEST_USER)
            .fetch_one(&db.pool)
            .await
            .expect("the test user's id");
        let grant = |user_id: &str, session_id: &str| {
            let (user_id, session_id) = (user_id.to_string(), session_id.to_string());
            let (client, chosen, pool) = (&client, &chosen, &db.pool);
            async move {
                create_self_issued(
                    pool,
                    &NewSelfIssuedGrant {
                        user_id: &user_id,
                        session_id: &session_id,
                        client,
                        levels: chosen,
                        default_catalog: None,
                    },
                    now(),
                )
                .await
            }
        };

        let not_hers = grant(&other_user, &session_id).await;
        user_sessions::revoke(&db.pool, &session_id)
            .await
            .expect("sign out");
        let ended = grant(&user_id, &session_id).await;

        assert!(
            matches!(not_hers, Err(OkfError::Unauthenticated { .. })),
            "{not_hers:?}"
        );
        assert!(
            matches!(ended, Err(OkfError::Unauthenticated { .. })),
            "{ended:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_grant_waiting_on_a_reset_in_progress_grants_nothing() {
        // A password reset holds the user's row while it ends every session
        // (`passwords::reset_in`). A grant queued behind it must find the
        // approving session gone once the reset commits.
        let db = test_db().await;
        let (client, user_id, session_id) = alice_with(&db.pool, &[("main", Access::Read)]).await;
        let mut reset = db.pool.begin().await.expect("begin");
        sqlx::query("SELECT 1 FROM users WHERE id = $1 FOR NO KEY UPDATE")
            .bind(&user_id)
            .execute(&mut *reset)
            .await
            .expect("hold the user");
        let chosen = levels(&[("main", Access::Read)]);
        let pool = db.pool.clone();
        let (user, session, record) = (user_id.clone(), session_id.clone(), client.clone());
        // `tokio::spawn` runs the grant concurrently; it blocks on the user.
        let granting = tokio::spawn(async move {
            create_self_issued(
                &pool,
                &NewSelfIssuedGrant {
                    user_id: &user,
                    session_id: &session,
                    client: &record,
                    levels: &chosen,
                    default_catalog: None,
                },
                now(),
            )
            .await
        });
        wait_for_lock_waiters(&db.pool, 1).await;

        user_sessions::revoke_all(&mut *reset, &user_id, None)
            .await
            .expect("end the sessions");
        reset.commit().await.expect("commit the reset");
        let made = granting.await.expect("the grant task");

        assert!(
            matches!(made, Err(OkfError::Unauthenticated { .. })),
            "{made:?}"
        );
        let grants = key_rows::list(&db.pool)
            .await
            .expect("list")
            .into_iter()
            .filter(|record| record.kind == KeyKind::OAuth)
            .count();
        assert_eq!(grants, 0);
    }

    /// Waits until `want` backends wait on a lock.
    async fn wait_for_lock_waiters(pool: &PgPool, want: i64) {
        for _ in 0..200 {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity WHERE wait_event_type = 'Lock'",
            )
            .fetch_one(pool)
            .await
            .expect("count waiters");
            if waiting >= want {
                return;
            }
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
        panic!("no backend waited on the lock");
    }
}

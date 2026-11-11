//! `oauth_tokens`: access and refresh tokens, by SHA-256. A family is every
//! token minted from one authorization code (Task 6 rotates within it).

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

use crate::error::OkfError;

/// An access or a refresh token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenKind {
    /// `okfat_`: presented on `/mcp` and `/api/v1`.
    Access,
    /// `okfrt_`: exchanged at `/oauth/token` for a new pair.
    Refresh,
}

impl TokenKind {
    /// The spelling stored in `oauth_tokens.kind`.
    ///
    /// # Returns
    /// `"access"` or `"refresh"`.
    pub fn as_str(self) -> &'static str {
        match self {
            TokenKind::Access => "access",
            TokenKind::Refresh => "refresh",
        }
    }

    /// Parses the stored spelling.
    ///
    /// # Arguments
    /// * `text` - an `oauth_tokens.kind` value.
    ///
    /// # Returns
    /// The kind, or `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "access" => Some(TokenKind::Access),
            "refresh" => Some(TokenKind::Refresh),
            _ => None,
        }
    }
}

/// A token to store. It holds the hash only, never the token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewToken<'a> {
    /// `keys::hash` of the token.
    pub sha256: [u8; 32],
    /// The grant (`api_keys.id`) it acts for.
    pub grant_id: &'a str,
    /// Access or refresh.
    pub kind: TokenKind,
    /// Its family (one per authorization code).
    pub family_id: &'a str,
    /// The resource it was issued for (the audience).
    pub resource: &'a str,
    /// When it stops being accepted.
    pub expires_at: DateTime<Utc>,
}

/// A live access token's facts, as the bearer check needs them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAccessToken {
    /// The grant it acts for.
    pub grant_id: String,
    /// The resource it was issued for.
    pub resource: String,
    /// When it stops being accepted.
    pub expires_at: DateTime<Utc>,
}

/// A token [`revoke`] found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevokedToken {
    /// Its family.
    pub family_id: String,
    /// Access or refresh (revoking a refresh token revokes its family, Task 6).
    pub kind: TokenKind,
}

/// Stores a token.
///
/// # Arguments
/// * `conn` - a connection or an open transaction (minting stores an access
///   and a refresh token together).
/// * `new` - the token.
/// * `now` - its creation time (server clock).
///
/// # Errors
/// [`OkfError::Database`], including a hash collision (a duplicate primary key).
pub async fn insert(
    conn: &mut PgConnection,
    new: &NewToken<'_>,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    sqlx::query(
        "INSERT INTO oauth_tokens \
         (sha256, grant_id, kind, family_id, resource, created_at, expires_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7)",
    )
    .bind(new.sha256.as_slice())
    .bind(new.grant_id)
    .bind(new.kind.as_str())
    .bind(new.family_id)
    .bind(new.resource)
    .bind(now)
    .bind(new.expires_at)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// A live access token by hash: not revoked, not expired. Whether its grant
/// is still live is the key cache's to say (`Keys::find_grant`).
///
/// # Arguments
/// * `pool` - the database.
/// * `sha256` - `keys::hash` of the presented token.
/// * `now` - the request's time (server clock).
///
/// # Returns
/// Its facts, or `None`.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn find_access(
    pool: &PgPool,
    sha256: &[u8; 32],
    now: DateTime<Utc>,
) -> Result<Option<LiveAccessToken>, OkfError> {
    let row: Option<(String, String, DateTime<Utc>)> = sqlx::query_as(
        "SELECT grant_id, resource, expires_at FROM oauth_tokens \
         WHERE sha256 = $1 AND kind = 'access' AND revoked_at IS NULL AND expires_at > $2",
    )
    .bind(sha256.as_slice())
    .bind(now)
    .fetch_optional(pool)
    .await?;
    // `Option::map` with a closure that destructures the tuple.
    Ok(row.map(|(grant_id, resource, expires_at)| LiveAccessToken {
        grant_id,
        resource,
        expires_at,
    }))
}

/// Revokes one token (RFC 7009). Revoking an already revoked token keeps
/// its first revocation time and still reports it.
///
/// # Arguments
/// * `pool` - the database.
/// * `sha256` - `keys::hash` of the token.
/// * `now` - the revocation time (server clock).
///
/// # Returns
/// Its family and kind, or `None` for an unknown token.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn revoke(
    pool: &PgPool,
    sha256: &[u8; 32],
    now: DateTime<Utc>,
) -> Result<Option<RevokedToken>, OkfError> {
    let row: Option<(String, String)> = sqlx::query_as(
        "UPDATE oauth_tokens SET revoked_at = COALESCE(revoked_at, $2) \
         WHERE sha256 = $1 RETURNING family_id, kind",
    )
    .bind(sha256.as_slice())
    .bind(now)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(|(family_id, kind)| RevokedToken {
        family_id,
        kind: TokenKind::parse(&kind).expect("the schema checks oauth_tokens.kind"),
    }))
}

/// The first key of the token-family locks (`okff` in ASCII): its own
/// namespace, apart from the catalog locks (`okf`) and the DCR registration
/// lock (`okfo`), so a family's hash can never collide with them.
const FAMILY_LOCK_NAMESPACE: i32 = 0x6f6b_6666;

/// Takes the family's lock for the rest of the transaction.
///
/// Every transaction that adds tokens to a family (minting from a code,
/// rotating a refresh token) or revokes it ([`revoke_family`]) takes this
/// lock first, so they run one at a time. That is what makes "reuse revokes
/// the whole family" hold under read committed: a statement's snapshot is
/// taken when the statement starts, so a revocation's `UPDATE` cannot see
/// successors inserted by a rotation that has not committed yet (PR #262
/// review). With the lock, either the revocation runs after the rotation
/// commits, and its `UPDATE`, started after the lock was granted, sees and
/// revokes the successors; or it runs first, and the rotation, reading its
/// token only after the lock was granted, finds it revoked and mints nothing.
///
/// Two families whose ids hash alike share a lock: harmless, they only wait
/// for each other.
///
/// # Arguments
/// * `conn` - the transaction's connection (pass `&mut tx`; outside a
///   transaction the lock would end with the statement).
/// * `family_id` - the family.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn lock_family(conn: &mut PgConnection, family_id: &str) -> Result<(), OkfError> {
    // `hashtext` maps the text id onto the lock's `int4` second key.
    // `pg_advisory_xact_lock` returns `void`, so run it with `execute`.
    sqlx::query("SELECT pg_advisory_xact_lock($1, hashtext($2))")
        .bind(FAMILY_LOCK_NAMESPACE)
        .bind(family_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// What presenting a refresh token found ([`use_refresh`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RefreshUse {
    /// Live and unused: now marked used. Mint its successor in the same
    /// transaction.
    Rotated {
        /// Its grant.
        grant_id: String,
        /// Its family, which the successor joins.
        family_id: String,
        /// The resource it was issued for.
        resource: String,
    },
    /// Already used once: a stolen copy, or the client replaying it. The
    /// caller revokes the family.
    Reused {
        /// The family to revoke.
        family_id: String,
    },
    /// Unknown, not a refresh token, expired, revoked, or its grant revoked.
    Unknown,
}

/// The row [`use_refresh`] reads: the token's binding, its state, and its
/// grant's revocation time.
type RefreshRow = (
    String,
    String,
    String,
    Option<DateTime<Utc>>,
    Option<DateTime<Utc>>,
    DateTime<Utc>,
    Option<DateTime<Utc>>,
);

/// Presents a refresh token: takes its family's lock ([`lock_family`]),
/// locks its row and, if it is live and unused, marks it used.
///
/// Reuse is checked first: a used token is `Reused` even after its family
/// was revoked, so replaying it again stays harmless (revoking twice).
///
/// # Arguments
/// * `tx` - a read-committed transaction, the one the successor is minted
///   in (commit it after; dropping it uncommitted rolls the mark back and
///   releases the family lock).
/// * `sha256` - the presented token's hash.
/// * `now` - the server clock.
///
/// # Returns
/// What was found.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn use_refresh(
    tx: &mut PgConnection,
    sha256: &[u8; 32],
    now: DateTime<Utc>,
) -> Result<RefreshUse, OkfError> {
    // The family first: a token's family never changes, so reading it
    // before the lock is safe; the token's state is read only after it.
    let family: Option<String> = sqlx::query_scalar(
        "SELECT family_id FROM oauth_tokens WHERE sha256 = $1 AND kind = 'refresh'",
    )
    .bind(sha256.as_slice())
    .fetch_optional(&mut *tx)
    .await?;
    let Some(family) = family else {
        return Ok(RefreshUse::Unknown);
    };
    lock_family(&mut *tx, &family).await?;
    // A new statement, so (read committed) a new snapshot: it sees whatever
    // the family's previous lock holder committed, a revocation included.
    // `FOR UPDATE OF t` also locks the token row.
    let row: Option<RefreshRow> = sqlx::query_as(
        "SELECT t.grant_id, t.family_id, t.resource, t.used_at, t.revoked_at, t.expires_at, \
                k.revoked_at \
         FROM oauth_tokens t JOIN api_keys k ON k.id = t.grant_id \
         WHERE t.sha256 = $1 AND t.kind = 'refresh' \
         FOR UPDATE OF t",
    )
    .bind(sha256.as_slice())
    .fetch_optional(&mut *tx)
    .await?;
    // `let ... else`: return early when there is no row.
    let Some((grant_id, family_id, resource, used_at, revoked_at, expires_at, grant_revoked_at)) =
        row
    else {
        return Ok(RefreshUse::Unknown);
    };
    if used_at.is_some() {
        return Ok(RefreshUse::Reused { family_id });
    }
    if revoked_at.is_some() || grant_revoked_at.is_some() || expires_at <= now {
        return Ok(RefreshUse::Unknown);
    }
    sqlx::query("UPDATE oauth_tokens SET used_at = $2 WHERE sha256 = $1")
        .bind(sha256.as_slice())
        .bind(now)
        .execute(&mut *tx)
        .await?;
    Ok(RefreshUse::Rotated {
        grant_id,
        family_id,
        resource,
    })
}

/// Revokes every live token of a family, under its lock ([`lock_family`]),
/// so a rotation or a mint in flight either commits first and is revoked
/// with the rest, or runs after and finds its parent revoked.
///
/// # Arguments
/// * `pool` - the database.
/// * `family_id` - the family.
/// * `now` - the server clock.
///
/// # Returns
/// The hashes it revoked (none if it was already revoked), for
/// `AccessTokens::forget`.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn revoke_family(
    pool: &PgPool,
    family_id: &str,
    now: DateTime<Utc>,
) -> Result<Vec<[u8; 32]>, OkfError> {
    let mut tx = pool.begin().await?;
    lock_family(&mut tx, family_id).await?;
    // Started after the lock was granted, so its snapshot holds every token
    // the family's previous lock holder committed.
    let rows: Vec<Vec<u8>> = sqlx::query_scalar(
        "UPDATE oauth_tokens SET revoked_at = $2 \
         WHERE family_id = $1 AND revoked_at IS NULL RETURNING sha256",
    )
    .bind(family_id)
    .bind(now)
    .fetch_all(&mut *tx)
    .await?;
    tx.commit().await?;
    // The schema checks `length(sha256) = 32`, so the conversion to a fixed
    // array cannot fail on a row the server wrote; `filter_map` skips any
    // other.
    Ok(rows
        .into_iter()
        .filter_map(|hash| <[u8; 32]>::try_from(hash.as_slice()).ok())
        .collect())
}

/// Deletes every token that expired before `expired_before`.
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
    let deleted = sqlx::query("DELETE FROM oauth_tokens WHERE expires_at < $1")
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

    /// A database with one grant; returns it with its id.
    async fn with_grant() -> (crate::db::pg_support::TestDb, String) {
        let db = test_db().await;
        catalogs::create(&db.pool, "main", "", now()).await.unwrap();
        let client = insert_client(
            &db.pool,
            "https://client.example.com/client.json",
            "Example",
            now(),
        )
        .await;
        let grant = insert_grant(&db.pool, &client, Access::Write, &["main"], now()).await;
        (db, grant)
    }

    async fn store(db: &crate::db::pg_support::TestDb, new: &NewToken<'_>) {
        let mut conn = db.pool.acquire().await.unwrap();
        insert(&mut conn, new, now()).await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn find_access_returns_live_access_tokens_only() {
        let (db, grant) = with_grant().await;
        let token = |byte: u8, kind: TokenKind, expires_at: DateTime<Utc>| NewToken {
            sha256: [byte; 32],
            grant_id: &grant,
            kind,
            family_id: "family-1",
            resource: "https://okf.example.com/mcp",
            expires_at,
        };
        let later = now() + TimeDelta::hours(1);
        store(&db, &token(1, TokenKind::Access, later)).await;
        store(&db, &token(2, TokenKind::Refresh, later)).await;
        store(
            &db,
            &token(3, TokenKind::Access, now() + TimeDelta::seconds(1)),
        )
        .await;
        store(&db, &token(4, TokenKind::Access, later)).await;
        revoke(&db.pool, &[4; 32], now()).await.unwrap();

        let live = find_access(&db.pool, &[1; 32], now()).await.unwrap();
        let refresh = find_access(&db.pool, &[2; 32], now()).await.unwrap();
        let expired = find_access(&db.pool, &[3; 32], now() + TimeDelta::seconds(1))
            .await
            .unwrap();
        let revoked = find_access(&db.pool, &[4; 32], now()).await.unwrap();
        let unknown = find_access(&db.pool, &[5; 32], now()).await.unwrap();

        assert_eq!(
            live,
            Some(LiveAccessToken {
                grant_id: grant.clone(),
                resource: "https://okf.example.com/mcp".to_string(),
                expires_at: later,
            })
        );
        assert_eq!(
            (refresh, expired, revoked, unknown),
            (None, None, None, None)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoke_reports_the_family_and_kind_and_is_idempotent() {
        let (db, grant) = with_grant().await;
        store(
            &db,
            &NewToken {
                sha256: [6; 32],
                grant_id: &grant,
                kind: TokenKind::Refresh,
                family_id: "family-2",
                resource: "https://okf.example.com/mcp",
                expires_at: now() + TimeDelta::days(30),
            },
        )
        .await;
        let expected = Some(RevokedToken {
            family_id: "family-2".to_string(),
            kind: TokenKind::Refresh,
        });

        let first = revoke(&db.pool, &[6; 32], now()).await.unwrap();
        let second = revoke(&db.pool, &[6; 32], now() + TimeDelta::hours(1))
            .await
            .unwrap();
        let unknown = revoke(&db.pool, &[7; 32], now()).await.unwrap();

        assert_eq!(first, expected);
        assert_eq!(second, expected);
        assert_eq!(unknown, None);
        let revoked_at: DateTime<Utc> =
            sqlx::query_scalar("SELECT revoked_at FROM oauth_tokens WHERE sha256 = $1")
                .bind([6u8; 32].as_slice())
                .fetch_one(&db.pool)
                .await
                .unwrap();
        assert_eq!(revoked_at, now(), "the first revocation time stays");
    }

    /// A token of `grant` in `family`, live for 30 days.
    fn token<'a>(
        grant: &'a str,
        kind: TokenKind,
        family: &'a str,
        sha256: [u8; 32],
    ) -> NewToken<'a> {
        NewToken {
            sha256,
            grant_id: grant,
            kind,
            family_id: family,
            resource: "https://okf.example.com/mcp",
            expires_at: now() + TimeDelta::days(30),
        }
    }

    /// `use_refresh` in its own committed transaction.
    async fn present(db: &crate::db::pg_support::TestDb, sha256: &[u8; 32]) -> RefreshUse {
        let mut tx = db.pool.begin().await.unwrap();
        let found = use_refresh(&mut tx, sha256, now()).await.unwrap();
        tx.commit().await.unwrap();
        found
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_refresh_token_rotates_once_then_counts_as_reused() {
        let (db, grant) = with_grant().await;
        store(&db, &token(&grant, TokenKind::Refresh, "family-a", [8; 32])).await;

        let first = present(&db, &[8; 32]).await;
        let second = present(&db, &[8; 32]).await;

        assert_eq!(
            first,
            RefreshUse::Rotated {
                grant_id: grant.clone(),
                family_id: "family-a".to_string(),
                resource: "https://okf.example.com/mcp".to_string(),
            }
        );
        assert_eq!(
            second,
            RefreshUse::Reused {
                family_id: "family-a".to_string()
            }
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_rolled_back_rotation_leaves_the_token_unused() {
        let (db, grant) = with_grant().await;
        store(&db, &token(&grant, TokenKind::Refresh, "family-b", [9; 32])).await;

        let mut tx = db.pool.begin().await.unwrap();
        let first = use_refresh(&mut tx, &[9; 32], now()).await.unwrap();
        // Dropped without `commit`: sqlx rolls the transaction back.
        drop(tx);

        assert!(matches!(first, RefreshUse::Rotated { .. }));
        assert!(matches!(
            present(&db, &[9; 32]).await,
            RefreshUse::Rotated { .. }
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn unknown_access_expired_revoked_and_revoked_grant_tokens_are_unknown() {
        let (db, grant) = with_grant().await;
        store(&db, &token(&grant, TokenKind::Access, "f", [10; 32])).await;
        store(
            &db,
            &NewToken {
                expires_at: now() + TimeDelta::seconds(1),
                ..token(&grant, TokenKind::Refresh, "f", [11; 32])
            },
        )
        .await;
        store(&db, &token(&grant, TokenKind::Refresh, "f", [12; 32])).await;
        revoke(&db.pool, &[12; 32], now()).await.unwrap();
        store(&db, &token(&grant, TokenKind::Refresh, "g", [13; 32])).await;
        let mut tx = db.pool.begin().await.unwrap();
        // One second on, the short-lived token has expired.
        let expired = use_refresh(&mut tx, &[11; 32], now() + TimeDelta::seconds(1))
            .await
            .unwrap();
        drop(tx);
        crate::store::keys::revoke_by_id(&db.pool, &grant, now())
            .await
            .unwrap();

        assert_eq!(expired, RefreshUse::Unknown, "expired");
        for (hash, what) in [
            ([14u8; 32], "unknown"),
            ([10; 32], "an access token"),
            ([12; 32], "revoked"),
            ([13; 32], "its grant revoked"),
        ] {
            assert_eq!(present(&db, &hash).await, RefreshUse::Unknown, "{what}");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoke_family_revokes_every_live_member_and_returns_their_hashes() {
        let (db, grant) = with_grant().await;
        store(&db, &token(&grant, TokenKind::Access, "fam", [15; 32])).await;
        store(&db, &token(&grant, TokenKind::Refresh, "fam", [16; 32])).await;
        store(&db, &token(&grant, TokenKind::Access, "else", [17; 32])).await;

        let mut revoked = revoke_family(&db.pool, "fam", now()).await.unwrap();
        revoked.sort();
        let again = revoke_family(&db.pool, "fam", now()).await.unwrap();

        assert_eq!(revoked, vec![[15; 32], [16; 32]]);
        assert!(again.is_empty(), "already revoked");
        assert!(find_access(&db.pool, &[15; 32], now())
            .await
            .unwrap()
            .is_none());
        assert!(find_access(&db.pool, &[17; 32], now())
            .await
            .unwrap()
            .is_some());
    }

    /// Waits until another backend on this test's database is blocked on a
    /// lock (an advisory lock or a row lock): the step that makes the race
    /// below deterministic instead of a matter of scheduling.
    async fn until_another_backend_waits_on_a_lock(db: &crate::db::pg_support::TestDb) {
        for _ in 0..500 {
            let waiting: i64 = sqlx::query_scalar(
                "SELECT count(*) FROM pg_stat_activity \
                 WHERE datname = current_database() AND pid <> pg_backend_pid() \
                 AND wait_event_type = 'Lock'",
            )
            .fetch_one(&db.pool)
            .await
            .unwrap();
            if waiting > 0 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        panic!("the revocation never waited on a lock");
    }

    /// What revokes the family in [`race_a_revocation_with_a_rotation`].
    #[derive(Debug, Clone, Copy)]
    enum Trigger {
        /// R0 presented again at the token endpoint.
        Reuse,
        /// R0 revoked at `/oauth/revoke`.
        Revoke,
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_reuse_racing_a_rotation_revokes_the_successors_too() {
        race_a_revocation_with_a_rotation(Trigger::Reuse).await;
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_revoke_racing_a_rotation_revokes_the_successors_too() {
        race_a_revocation_with_a_rotation(Trigger::Revoke).await;
    }

    /// PR #262 review: R0 has rotated to R1. A rotation of R1 inserts its
    /// successors (A2, R2) and stays open; meanwhile `trigger` on R0 revokes
    /// the family. When the rotation commits, its successors must be revoked
    /// too, not left live outside the revocation's snapshot.
    async fn race_a_revocation_with_a_rotation(trigger: Trigger) {
        let (db, grant) = with_grant().await;
        let family = "family-race";
        let (r0, a1, r1, a2, r2) = ([20u8; 32], [21; 32], [22; 32], [23; 32], [24; 32]);
        store(&db, &token(&grant, TokenKind::Refresh, family, r0)).await;
        // R0 -> (A1, R1), committed.
        let mut tx = db.pool.begin().await.unwrap();
        assert!(matches!(
            use_refresh(&mut tx, &r0, now()).await.unwrap(),
            RefreshUse::Rotated { .. }
        ));
        insert(
            &mut tx,
            &token(&grant, TokenKind::Access, family, a1),
            now(),
        )
        .await
        .unwrap();
        insert(
            &mut tx,
            &token(&grant, TokenKind::Refresh, family, r1),
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        // R1 -> (A2, R2), held open after inserting the successors.
        let mut rotation = db.pool.begin().await.unwrap();
        assert!(matches!(
            use_refresh(&mut rotation, &r1, now()).await.unwrap(),
            RefreshUse::Rotated { .. }
        ));
        insert(
            &mut rotation,
            &token(&grant, TokenKind::Access, family, a2),
            now(),
        )
        .await
        .unwrap();
        insert(
            &mut rotation,
            &token(&grant, TokenKind::Refresh, family, r2),
            now(),
        )
        .await
        .unwrap();

        // On another connection, as the endpoints do it: the token endpoint
        // finds R0 reused, or `/oauth/revoke` revokes R0; then the family.
        let pool = db.pool.clone();
        let reuse = tokio::spawn(async move {
            let family_id = match trigger {
                Trigger::Reuse => {
                    let mut tx = pool.begin().await.unwrap();
                    let found = use_refresh(&mut tx, &r0, now()).await.unwrap();
                    drop(tx);
                    let RefreshUse::Reused { family_id } = found else {
                        panic!("R0 is a reuse, found {found:?}");
                    };
                    family_id
                }
                Trigger::Revoke => {
                    revoke(&pool, &r0, now())
                        .await
                        .unwrap()
                        .expect("R0")
                        .family_id
                }
            };
            revoke_family(&pool, &family_id, now()).await.unwrap()
        });
        until_another_backend_waits_on_a_lock(&db).await;
        rotation.commit().await.unwrap();
        let mut revoked = reuse.await.unwrap();
        revoked.sort();

        assert!(
            find_access(&db.pool, &a2, now()).await.unwrap().is_none(),
            "the successor access token is revoked"
        );
        assert_eq!(present(&db, &r2).await, RefreshUse::Unknown, "so is R2");
        assert!(
            revoked.contains(&a2) && revoked.contains(&r2),
            "{revoked:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prune_deletes_only_tokens_expired_before_the_cut_off() {
        let (db, grant) = with_grant().await;
        for (byte, expires_at) in [
            (1u8, now() - TimeDelta::days(8)),
            (2, now() - TimeDelta::days(6)),
            (3, now() + TimeDelta::hours(1)),
        ] {
            sqlx::query(
                "INSERT INTO oauth_tokens \
                 (sha256, grant_id, kind, family_id, resource, created_at, expires_at) \
                 VALUES ($1, $2, 'access', 'f', 'https://okf.example.com/mcp', $3, $4)",
            )
            .bind([byte; 32].as_slice())
            .bind(&grant)
            .bind(expires_at - TimeDelta::hours(1))
            .bind(expires_at)
            .execute(&db.pool)
            .await
            .unwrap();
        }

        let pruned = prune(&db.pool, now() - TimeDelta::days(7)).await.unwrap();

        assert_eq!(pruned, 1);
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM oauth_tokens")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(left, 2);
    }
}

//! Catalog rows, the per-catalog lock, and the administrative generation.

use chrono::{DateTime, Utc};
// `PgExecutor` is "anything a query can run on": a pool, a connection or a
// transaction. Functions generic over it work inside or outside a transaction.
use sqlx::{PgConnection, PgExecutor, PgPool};

use crate::error::OkfError;

/// The first key of every catalog lock (`okf` in ASCII), separating this
/// crate's advisory locks from anyone else's on a shared database.
const CATALOG_LOCK_NAMESPACE: i32 = 0x006f_6b66;

/// A catalog row.
// `derive(sqlx::FromRow)` maps a result row's columns onto the fields by
// name, like building a dataclass from a `dict` row.
#[derive(Debug, Clone, PartialEq, Eq, sqlx::FromRow)]
pub struct Catalog {
    /// Database id.
    pub id: i64,
    /// Unique name, `[a-z0-9-]+`.
    pub name: String,
    /// Free text.
    pub description: String,
    /// Bumped by every publish and import.
    pub generation: i64,
    /// When it was created (server clock).
    pub created_at: DateTime<Utc>,
}

/// Is `name` a valid catalog name (non-empty, ASCII lowercase letters, digits, `-`)?
///
/// # Arguments
/// * `name` - the proposed name.
///
/// # Returns
/// `true` if the schema's `CHECK (name ~ '^[a-z0-9-]+$')` would accept it.
pub fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-')
}

/// Creates an empty catalog and bumps the administrative generation, in one
/// transaction.
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the new catalog's name.
/// * `description` - free text.
/// * `now` - the creation time (server clock).
///
/// # Returns
/// The new row.
///
/// # Errors
/// [`OkfError::Refused`] for an invalid or taken name; [`OkfError::Database`]
/// otherwise.
pub async fn create(
    pool: &PgPool,
    name: &str,
    description: &str,
    now: DateTime<Utc>,
) -> Result<Catalog, OkfError> {
    if !is_valid_name(name) {
        return Err(OkfError::Refused {
            reason: format!("catalog name {name:?} must be lowercase letters, digits and '-'"),
        });
    }
    // Dropping `tx` without `commit()` (an early `return` or `?`) rolls back.
    let mut tx = pool.begin().await?;
    let inserted = sqlx::query_as::<_, Catalog>(
        "INSERT INTO catalogs (name, description, created_at) VALUES ($1, $2, $3) \
         RETURNING id, name, description, generation, created_at",
    )
    .bind(name)
    .bind(description)
    .bind(now)
    // `&mut *tx` borrows the transaction's connection: the query runs inside it.
    .fetch_one(&mut *tx)
    .await;
    let catalog = match inserted {
        Ok(catalog) => catalog,
        Err(error) if is_unique_violation(&error) => {
            return Err(OkfError::Refused {
                reason: format!("catalog {name:?} already exists"),
            })
        }
        Err(error) => return Err(error.into()),
    };
    // `&mut tx` is a `&mut Transaction`, which Rust converts to the
    // `&mut PgConnection` the helper takes (deref coercion).
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(catalog)
}

/// Every catalog, by name.
///
/// # Arguments
/// * `pool` - the database.
///
/// # Returns
/// The rows, ordered by name.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn list(pool: &PgPool) -> Result<Vec<Catalog>, OkfError> {
    let catalogs = sqlx::query_as::<_, Catalog>(
        "SELECT id, name, description, generation, created_at FROM catalogs ORDER BY name",
    )
    .fetch_all(pool)
    .await?;
    Ok(catalogs)
}

/// The catalog called `name`.
///
/// # Arguments
/// * `executor` - a pool, connection or transaction (`&mut *tx`).
/// * `name` - the catalog's name.
///
/// # Returns
/// Its row.
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] if there is none; [`OkfError::Database`].
// `'e` is a lifetime parameter: the executor is borrowed for the query only.
pub async fn find<'e>(executor: impl PgExecutor<'e>, name: &str) -> Result<Catalog, OkfError> {
    sqlx::query_as::<_, Catalog>(
        "SELECT id, name, description, generation, created_at FROM catalogs WHERE name = $1",
    )
    .bind(name)
    .fetch_optional(executor)
    .await?
    .ok_or_else(|| OkfError::CatalogNotSelected {
        catalog: name.to_string(),
    })
}

/// Deletes a catalog with its documents, history, changesets and grants
/// (cascading foreign keys), under the catalog's lock, and bumps the
/// administrative generation, in one transaction.
///
/// After the lock it locks the catalog row and its open changesets
/// (`FOR UPDATE`) before looking for unpublished work. A foreign-key check
/// takes `FOR KEY SHARE` on the row it references, which conflicts with
/// `FOR UPDATE`, so a changeset or entry being written concurrently is either
/// committed first (and seen) or held off until the delete ends (and then
/// fails its foreign-key check): the refusal cannot be raced. A writer that
/// holds an uncommitted entry and then needs the catalog row can deadlock
/// with this (Postgres aborts one side); see the plan's note for M5.
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the catalog's name.
/// * `force` - delete even with unpublished changes in an open changeset.
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] if there is no such catalog;
/// [`OkfError::Refused`] if it has unpublished changes and `force` is false;
/// [`OkfError::Database`] otherwise.
pub async fn delete(pool: &PgPool, name: &str, force: bool) -> Result<(), OkfError> {
    let mut tx = pool.begin().await?;
    let catalog = find(&mut *tx, name).await?;
    lock(&mut tx, catalog.id).await?;
    // A delete that waited on the lock may find the row already gone.
    let still_there: Option<i64> =
        sqlx::query_scalar("SELECT id FROM catalogs WHERE id = $1 FOR UPDATE")
            .bind(catalog.id)
            .fetch_optional(&mut *tx)
            .await?;
    if still_there.is_none() {
        return Err(OkfError::CatalogNotSelected {
            catalog: name.to_string(),
        });
    }
    sqlx::query("SELECT id FROM changesets WHERE catalog_id = $1 AND state = 'open' FOR UPDATE")
        .bind(catalog.id)
        .execute(&mut *tx)
        .await?;
    // An open changeset with no entries holds no work, so it does not block.
    let unpublished: bool = sqlx::query_scalar(
        "SELECT EXISTS (SELECT 1 FROM changesets c \
         JOIN changeset_entries e ON e.changeset_id = c.id \
         WHERE c.catalog_id = $1 AND c.state = 'open')",
    )
    .bind(catalog.id)
    .fetch_one(&mut *tx)
    .await?;
    if unpublished && !force {
        return Err(OkfError::Refused {
            reason: format!(
                "catalog {name:?} has unpublished changes; pass --force to delete them too"
            ),
        });
    }
    sqlx::query("DELETE FROM catalogs WHERE id = $1")
        .bind(catalog.id)
        .execute(&mut *tx)
        .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Takes the catalog's publish lock for the rest of the transaction (design
/// "Publish"): publishes, imports and deletes of one catalog run one at a time.
///
/// # Arguments
/// * `conn` - the transaction's connection (pass `&mut tx`).
/// * `catalog_id` - the catalog's id.
///
/// # Errors
/// [`OkfError::Database`], including for an id above `i32::MAX` (the
/// two-key form takes `int4` keys; the server's cast reports "integer out of
/// range" rather than this process panicking).
pub(crate) async fn lock(conn: &mut PgConnection, catalog_id: i64) -> Result<(), OkfError> {
    // The two-key form, under this crate's namespace, so the lock cannot
    // collide with another application's advisory locks on a shared database.
    // `pg_advisory_xact_lock` returns `void`, so run it with `execute`.
    sqlx::query("SELECT pg_advisory_xact_lock($1, $2::int4)")
        .bind(CATALOG_LOCK_NAMESPACE)
        .bind(catalog_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// Bumps `server_state.admin_generation` inside the caller's transaction.
///
/// # Arguments
/// * `conn` - the transaction's connection (pass `&mut tx`).
///
/// # Errors
/// [`OkfError::Database`].
pub(crate) async fn bump_admin_generation(conn: &mut PgConnection) -> Result<(), OkfError> {
    sqlx::query("UPDATE server_state SET admin_generation = admin_generation + 1")
        .execute(&mut *conn)
        .await?;
    Ok(())
}

/// The current administrative generation (design "Noticing changes made
/// outside the server").
///
/// # Arguments
/// * `executor` - a pool, connection or transaction.
///
/// # Returns
/// The counter every `catalog` and `key` change bumps.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn admin_generation<'e>(executor: impl PgExecutor<'e>) -> Result<i64, OkfError> {
    let generation = sqlx::query_scalar("SELECT admin_generation FROM server_state")
        .fetch_one(executor)
        .await?;
    Ok(generation)
}

/// Is this a unique-constraint violation (SQLSTATE 23505)?
///
/// # Arguments
/// * `error` - a driver error.
///
/// # Returns
/// `true` only for a database error with code 23505.
pub(crate) fn is_unique_violation(error: &sqlx::Error) -> bool {
    error
        .as_database_error()
        .is_some_and(|db_error| db_error.is_unique_violation())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_list_and_find() {
        let db = test_db().await;

        let pg = create(&db.pool, "pg", "pgrace's catalog", now())
            .await
            .unwrap();
        create(&db.pool, "aa-work", "", now()).await.unwrap();

        assert_eq!(
            (pg.name.as_str(), pg.generation, pg.created_at),
            ("pg", 0, now())
        );
        let names: Vec<String> = list(&db.pool)
            .await
            .unwrap()
            .into_iter()
            .map(|c| c.name)
            .collect();
        assert_eq!(names, ["aa-work", "pg"]);
        assert_eq!(find(&db.pool, "pg").await.unwrap(), pg);
        assert!(matches!(
            find(&db.pool, "nope").await,
            Err(OkfError::CatalogNotSelected { catalog }) if catalog == "nope"
        ));
        assert_eq!(admin_generation(&db.pool).await.unwrap(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bad_and_duplicate_names_are_refused() {
        let db = test_db().await;
        create(&db.pool, "pg", "", now()).await.unwrap();

        for name in ["", "Bad Name", "a/b", "ünï"] {
            assert!(
                matches!(
                    create(&db.pool, name, "", now()).await,
                    Err(OkfError::Refused { .. })
                ),
                "{name}"
            );
        }
        let duplicate = create(&db.pool, "pg", "", now()).await;

        assert!(
            matches!(duplicate, Err(OkfError::Refused { reason }) if reason.contains("already exists"))
        );
        assert_eq!(admin_generation(&db.pool).await.unwrap(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_refuses_unpublished_work_unless_forced() {
        let db = test_db().await;
        let pg = create(&db.pool, "pg", "", now()).await.unwrap();
        let changeset: i64 = sqlx::query_scalar(
            "INSERT INTO changesets (catalog_id, session_id, actor, state, created_at, updated_at) \
             VALUES ($1, 's', 'a', 'open', $2, $2) RETURNING id",
        )
        .bind(pg.id)
        .bind(now())
        .fetch_one(&db.pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO changeset_entries (changeset_id, path, content) VALUES ($1, 'a.md', $2)",
        )
        .bind(changeset)
        .bind(b"x".to_vec())
        .execute(&db.pool)
        .await
        .unwrap();

        let refused = delete(&db.pool, "pg", false).await;
        assert!(matches!(refused, Err(OkfError::Refused { reason }) if reason.contains("--force")));

        delete(&db.pool, "pg", true).await.unwrap();

        assert!(list(&db.pool).await.unwrap().is_empty());
        let entries: i64 = sqlx::query_scalar("SELECT count(*) FROM changeset_entries")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(entries, 0);
        assert_eq!(admin_generation(&db.pool).await.unwrap(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_empty_open_changeset_does_not_block_delete() {
        let db = test_db().await;
        let pg = create(&db.pool, "pg", "", now()).await.unwrap();
        sqlx::query(
            "INSERT INTO changesets (catalog_id, session_id, actor, state, created_at, updated_at) \
             VALUES ($1, 's', 'a', 'open', $2, $2)",
        )
        .bind(pg.id)
        .bind(now())
        .execute(&db.pool)
        .await
        .unwrap();

        delete(&db.pool, "pg", false).await.unwrap();

        assert!(list(&db.pool).await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_lock_holds_other_transactions_off_until_commit() {
        let db = test_db().await;
        let pg = create(&db.pool, "pg", "", now()).await.unwrap();
        let mut holder = db.pool.begin().await.unwrap();

        lock(&mut holder, pg.id).await.unwrap();

        let try_lock = "SELECT pg_try_advisory_xact_lock($1, $2::int4)";
        let mut other = db.pool.begin().await.unwrap();
        let taken: bool = sqlx::query_scalar(try_lock)
            .bind(CATALOG_LOCK_NAMESPACE)
            .bind(pg.id)
            .fetch_one(&mut *other)
            .await
            .unwrap();
        assert!(!taken, "a second transaction got the held lock");
        holder.commit().await.unwrap();
        let taken: bool = sqlx::query_scalar(try_lock)
            .bind(CATALOG_LOCK_NAMESPACE)
            .bind(pg.id)
            .fetch_one(&mut *other)
            .await
            .unwrap();
        assert!(taken, "the lock outlived its transaction");
    }

    /// Waits until some backend is blocked on a lock, so a test knows the
    /// spawned delete has reached the point it should wait at.
    async fn until_a_lock_is_awaited(pool: &PgPool) {
        for _ in 0..200 {
            let waiting: i64 =
                sqlx::query_scalar("SELECT count(*) FROM pg_locks WHERE NOT granted")
                    .fetch_one(pool)
                    .await
                    .unwrap();
            if waiting > 0 {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("no backend ever waited on a lock");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_delete_that_waited_finds_the_catalog_gone() {
        let db = test_db().await;
        let pg = create(&db.pool, "pg", "", now()).await.unwrap();
        let mut first = db.pool.begin().await.unwrap();
        lock(&mut first, pg.id).await.unwrap();
        // `tokio::spawn` runs the second delete concurrently, like a thread;
        // it blocks on the lock `first` holds.
        let pool = db.pool.clone();
        let second = tokio::spawn(async move { delete(&pool, "pg", false).await });
        until_a_lock_is_awaited(&db.pool).await;

        sqlx::query("DELETE FROM catalogs WHERE id = $1")
            .bind(pg.id)
            .execute(&mut *first)
            .await
            .unwrap();
        first.commit().await.unwrap();
        let outcome = second.await.unwrap();

        assert!(matches!(
            outcome,
            Err(OkfError::CatalogNotSelected { catalog }) if catalog == "pg"
        ));
        assert_eq!(admin_generation(&db.pool).await.unwrap(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_entry_written_during_a_delete_is_refused_not_cascaded() {
        let db = test_db().await;
        let pg = create(&db.pool, "pg", "", now()).await.unwrap();
        let changeset: i64 = sqlx::query_scalar(
            "INSERT INTO changesets (catalog_id, session_id, actor, state, created_at, updated_at) \
             VALUES ($1, 's', 'a', 'open', $2, $2) RETURNING id",
        )
        .bind(pg.id)
        .bind(now())
        .fetch_one(&db.pool)
        .await
        .unwrap();
        // A session's write, not yet committed when the delete starts.
        let mut writer = db.pool.begin().await.unwrap();
        sqlx::query(
            "INSERT INTO changeset_entries (changeset_id, path, content) VALUES ($1, 'a.md', $2)",
        )
        .bind(changeset)
        .bind(b"x".to_vec())
        .execute(&mut *writer)
        .await
        .unwrap();
        let pool = db.pool.clone();
        let deleting = tokio::spawn(async move { delete(&pool, "pg", false).await });
        until_a_lock_is_awaited(&db.pool).await;

        writer.commit().await.unwrap();
        let outcome = deleting.await.unwrap();

        assert!(matches!(outcome, Err(OkfError::Refused { reason }) if reason.contains("--force")));
        assert_eq!(list(&db.pool).await.unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_id_beyond_int4_is_a_database_error() {
        let db = test_db().await;
        let mut tx = db.pool.begin().await.unwrap();

        let outcome = lock(&mut tx, i64::from(i32::MAX) + 1).await;

        assert!(matches!(outcome, Err(OkfError::Database { .. })));
    }
}

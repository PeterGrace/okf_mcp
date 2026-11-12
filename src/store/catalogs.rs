//! Catalog rows, the per-catalog lock, and the administrative generation.

use chrono::{DateTime, Utc};
// `PgExecutor` is "anything a query can run on": a pool, a connection or a
// transaction. Functions generic over it work inside or outside a transaction.
use sqlx::{PgConnection, PgExecutor, PgPool};

use crate::error::OkfError;
use crate::store::teams;

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
    /// The name of the team that owns it; `None` only for a catalog from
    /// before migration 0009 not yet assigned.
    pub owner_team: Option<String>,
}

// One SELECT shared by `list` and `find` (a macro so `concat!` can append
// each query's tail at compile time; see `select_users!` in users.rs).
macro_rules! select_catalogs {
    () => {
        "SELECT c.id, c.name, c.description, c.generation, c.created_at, \
                (SELECT t.name FROM teams t WHERE t.id = c.owner_team_id) AS owner_team \
         FROM catalogs c"
    };
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
/// * `owner_team` - the name of the team that owns it.
/// * `now` - the creation time (server clock).
///
/// # Returns
/// The new row.
///
/// # Errors
/// [`OkfError::Refused`] for an invalid or taken name;
/// [`OkfError::UnknownName`] for an unknown team; [`OkfError::Database`]
/// otherwise.
pub async fn create(
    pool: &PgPool,
    name: &str,
    description: &str,
    owner_team: &str,
    now: DateTime<Utc>,
) -> Result<Catalog, OkfError> {
    if !is_valid_name(name) {
        return Err(OkfError::Refused {
            reason: format!("catalog name {name:?} must be lowercase letters, digits and '-'"),
        });
    }
    // Dropping `tx` without `commit()` (an early `return` or `?`) rolls back.
    let mut tx = pool.begin().await?;
    // `&mut tx` is a `&mut Transaction`, which Rust converts to the
    // `&mut PgConnection` the helper takes (deref coercion).
    let team_id = teams::id_of(&mut tx, owner_team).await?;
    // `RETURNING` cannot run the owner subquery of `select_catalogs!` on the
    // new row, but the owner's name is the one just resolved: hand it back
    // as `$5`.
    let inserted = sqlx::query_as::<_, Catalog>(
        "INSERT INTO catalogs (name, description, created_at, owner_team_id) \
         VALUES ($1, $2, $3, $4) \
         RETURNING id, name, description, generation, created_at, $5::text AS owner_team",
    )
    .bind(name)
    .bind(description)
    .bind(now)
    .bind(&team_id)
    .bind(owner_team)
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
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(catalog)
}

/// Makes `team` the owner of catalog `name` (moving it, or assigning a
/// catalog from before migration 0009), and bumps the administrative
/// generation, in one transaction.
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the catalog's name.
/// * `team` - the new owner's team name.
///
/// # Returns
/// The previous owner's name (`None` for a catalog that had none).
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] for an unknown catalog;
/// [`OkfError::UnknownName`] for an unknown team; [`OkfError::Database`].
pub async fn set_owner(pool: &PgPool, name: &str, team: &str) -> Result<Option<String>, OkfError> {
    let mut tx = pool.begin().await?;
    let team_id = teams::id_of(&mut tx, team).await?;
    // As in `update_description`: the subquery reads the row as it was
    // before the `UPDATE`, under its row lock, so this returns the owner it
    // replaced even with two changes racing. `Option<Option<String>>`: the
    // outer `None` is "no such catalog", the inner one "it had no owner".
    let previous: Option<Option<String>> = sqlx::query_scalar(
        "UPDATE catalogs c SET owner_team_id = $2 \
         FROM (SELECT id, owner_team_id FROM catalogs WHERE name = $1 FOR UPDATE) old \
         WHERE c.id = old.id \
         RETURNING (SELECT t.name FROM teams t WHERE t.id = old.owner_team_id)",
    )
    .bind(name)
    .bind(&team_id)
    .fetch_optional(&mut *tx)
    .await?;
    let previous = previous.ok_or_else(|| OkfError::CatalogNotSelected {
        catalog: name.to_string(),
    })?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(previous)
}

/// Replaces a catalog's description and bumps the administrative
/// generation, in one transaction. The catalog's `generation` is left alone:
/// its content did not change, so its snapshot is not rebuilt.
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the catalog's name.
/// * `description` - the new free text (empty is allowed, as for `create`).
///
/// # Returns
/// The description it had before.
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] if there is no such catalog;
/// [`OkfError::Database`] otherwise.
pub async fn update_description(
    pool: &PgPool,
    name: &str,
    description: &str,
) -> Result<String, OkfError> {
    let mut tx = pool.begin().await?;
    // A subquery reads the row as it was before the `UPDATE` (one
    // statement sees one snapshot), so this returns the old text; the row
    // lock the `UPDATE` takes keeps two updates from both reading the same.
    let previous: Option<String> = sqlx::query_scalar(
        "UPDATE catalogs c SET description = $2 \
         FROM (SELECT id, description FROM catalogs WHERE name = $1 FOR UPDATE) old \
         WHERE c.id = old.id RETURNING old.description",
    )
    .bind(name)
    .bind(description)
    .fetch_optional(&mut *tx)
    .await?;
    let previous = previous.ok_or_else(|| OkfError::CatalogNotSelected {
        catalog: name.to_string(),
    })?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(previous)
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
    let catalogs = sqlx::query_as::<_, Catalog>(concat!(select_catalogs!(), " ORDER BY c.name"))
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
    sqlx::query_as::<_, Catalog>(concat!(select_catalogs!(), " WHERE c.name = $1"))
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
/// fails its foreign-key check): the refusal cannot be raced. Writers take the
/// catalog row (`FOR KEY SHARE`, `changesets::lock_catalog`) before any
/// changeset or entry, the same order as here, so a write and a delete wait
/// for each other rather than deadlock: a write that waited finds the
/// catalog gone.
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
    // In id order, so two transactions locking several changesets cannot
    // take them in opposite orders.
    sqlx::query(
        "SELECT id FROM changesets WHERE catalog_id = $1 AND state = 'open' \
         ORDER BY id FOR UPDATE",
    )
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
// `pub(crate)` so other store modules' tests share the lock helper below.
pub(crate) mod tests {
    use super::*;
    use crate::db::{test_db, TEST_TEAM};
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_list_and_find() {
        let db = test_db().await;
        // Read first: the fixture user already bumped the generation.
        let start = admin_generation(&db.pool).await.unwrap();

        let pg = create(&db.pool, "pg", "pgrace's catalog", TEST_TEAM, now())
            .await
            .unwrap();
        create(&db.pool, "aa-work", "", TEST_TEAM, now())
            .await
            .unwrap();

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
        assert_eq!(admin_generation(&db.pool).await.unwrap(), start + 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_new_catalog_is_owned_by_the_named_team() {
        let db = test_db().await;

        let catalog = create(&db.pool, "pg", "", TEST_TEAM, now()).await.unwrap();

        assert_eq!(catalog.owner_team.as_deref(), Some(TEST_TEAM));
        assert_eq!(find(&db.pool, "pg").await.unwrap(), catalog);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_owner_is_unknown_name_and_creates_nothing() {
        let db = test_db().await;
        // Read first: the fixture user already bumped the generation.
        let start = admin_generation(&db.pool).await.unwrap();

        let made = create(&db.pool, "pg", "", "nobody", now()).await;

        assert!(
            matches!(&made, Err(OkfError::UnknownName { noun: "team", name }) if name == "nobody"),
            "{made:?}"
        );
        assert!(list(&db.pool).await.unwrap().is_empty());
        assert_eq!(admin_generation(&db.pool).await.unwrap(), start);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_owner_moves_a_catalog_and_returns_the_previous_owner() {
        let db = test_db().await;
        let alice = crate::store::users::NewUser {
            name: "alice",
            display_name: "",
        };
        crate::store::users::create(&db.pool, &alice, now())
            .await
            .unwrap();
        create(&db.pool, "pg", "", TEST_TEAM, now()).await.unwrap();
        let before = admin_generation(&db.pool).await.unwrap();

        let previous = set_owner(&db.pool, "pg", "alice").await.unwrap();

        assert_eq!(previous.as_deref(), Some(TEST_TEAM));
        assert_eq!(
            find(&db.pool, "pg").await.unwrap().owner_team.as_deref(),
            Some("alice")
        );
        assert_eq!(admin_generation(&db.pool).await.unwrap(), before + 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_owner_assigns_a_legacy_catalog() {
        let db = test_db().await;
        sqlx::query("INSERT INTO catalogs (name, created_at) VALUES ('legacy', now())")
            .execute(&db.pool)
            .await
            .unwrap();
        assert_eq!(find(&db.pool, "legacy").await.unwrap().owner_team, None);

        let previous = set_owner(&db.pool, "legacy", TEST_TEAM).await.unwrap();

        assert_eq!(previous, None);
        assert_eq!(
            find(&db.pool, "legacy")
                .await
                .unwrap()
                .owner_team
                .as_deref(),
            Some(TEST_TEAM)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_owner_refuses_an_unknown_catalog_or_team_and_changes_nothing() {
        let db = test_db().await;
        create(&db.pool, "pg", "", TEST_TEAM, now()).await.unwrap();
        let start = admin_generation(&db.pool).await.unwrap();

        let no_catalog = set_owner(&db.pool, "nope", TEST_TEAM).await;
        let no_team = set_owner(&db.pool, "pg", "nobody").await;

        assert!(
            matches!(&no_catalog, Err(OkfError::CatalogNotSelected { catalog }) if catalog == "nope"),
            "{no_catalog:?}"
        );
        assert!(
            matches!(&no_team, Err(OkfError::UnknownName { noun: "team", .. })),
            "{no_team:?}"
        );
        assert_eq!(
            find(&db.pool, "pg").await.unwrap().owner_team.as_deref(),
            Some(TEST_TEAM)
        );
        assert_eq!(admin_generation(&db.pool).await.unwrap(), start);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_description_replaces_it_and_returns_the_old_text() {
        let db = test_db().await;
        // Read first: the fixture user already bumped the generation.
        let start = admin_generation(&db.pool).await.unwrap();
        let pg = create(&db.pool, "pg", "copy", TEST_TEAM, now())
            .await
            .unwrap();

        let previous = update_description(&db.pool, "pg", "source of truth")
            .await
            .unwrap();
        let cleared = update_description(&db.pool, "pg", "").await.unwrap();

        assert_eq!(
            (previous.as_str(), cleared.as_str()),
            ("copy", "source of truth")
        );
        let row = find(&db.pool, "pg").await.unwrap();
        assert_eq!(
            (row.description.as_str(), row.generation),
            ("", pg.generation)
        );
        assert_eq!(admin_generation(&db.pool).await.unwrap(), start + 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn concurrent_updates_each_return_the_text_they_replaced() {
        let db = test_db().await;
        create(&db.pool, "pg", "start", TEST_TEAM, now())
            .await
            .unwrap();
        // Hold the row so both updates queue behind it and then race.
        let mut holder = db.pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM catalogs WHERE name = 'pg' FOR UPDATE")
            .execute(&mut *holder)
            .await
            .unwrap();
        let spawn_update = |text: &'static str| {
            let pool = db.pool.clone();
            tokio::spawn(async move { update_description(&pool, "pg", text).await })
        };
        let first = spawn_update("one");
        let second = spawn_update("two");
        until_locks_are_awaited(&db.pool, 2).await;

        holder.commit().await.unwrap();
        let mut previous = [
            first.await.unwrap().unwrap(),
            second.await.unwrap().unwrap(),
        ];
        let last = find(&db.pool, "pg").await.unwrap().description;

        // One update replaced "start"; the other replaced the first's text.
        // Without the row lock in the subquery, both could report "start".
        previous.sort();
        let winner_first = if last == "one" { "two" } else { "one" };
        let mut expected = ["start".to_string(), winner_first.to_string()];
        expected.sort();
        assert_eq!(previous, expected);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_description_refuses_an_unknown_catalog() {
        let db = test_db().await;
        // Read first: the fixture user already bumped the generation.
        let start = admin_generation(&db.pool).await.unwrap();

        let outcome = update_description(&db.pool, "nope", "x").await;

        assert!(matches!(
            outcome,
            Err(OkfError::CatalogNotSelected { catalog }) if catalog == "nope"
        ));
        assert_eq!(admin_generation(&db.pool).await.unwrap(), start);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bad_and_duplicate_names_are_refused() {
        let db = test_db().await;
        // Read first: the fixture user already bumped the generation.
        let start = admin_generation(&db.pool).await.unwrap();
        create(&db.pool, "pg", "", TEST_TEAM, now()).await.unwrap();

        for name in ["", "Bad Name", "a/b", "ünï"] {
            assert!(
                matches!(
                    create(&db.pool, name, "", TEST_TEAM, now()).await,
                    Err(OkfError::Refused { .. })
                ),
                "{name}"
            );
        }
        let duplicate = create(&db.pool, "pg", "", TEST_TEAM, now()).await;

        assert!(
            matches!(duplicate, Err(OkfError::Refused { reason }) if reason.contains("already exists"))
        );
        assert_eq!(admin_generation(&db.pool).await.unwrap(), start + 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_refuses_unpublished_work_unless_forced() {
        let db = test_db().await;
        // Read first: the fixture user already bumped the generation.
        let start = admin_generation(&db.pool).await.unwrap();
        let pg = create(&db.pool, "pg", "", TEST_TEAM, now()).await.unwrap();
        let changeset: i64 = sqlx::query_scalar(
            "INSERT INTO changesets (catalog_id, actor, state, created_at, updated_at) \
             VALUES ($1, 'a', 'open', $2, $2) RETURNING id",
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
        assert_eq!(admin_generation(&db.pool).await.unwrap(), start + 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_empty_open_changeset_does_not_block_delete() {
        let db = test_db().await;
        let pg = create(&db.pool, "pg", "", TEST_TEAM, now()).await.unwrap();
        sqlx::query(
            "INSERT INTO changesets (catalog_id, actor, state, created_at, updated_at) \
             VALUES ($1, 'a', 'open', $2, $2)",
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
        let pg = create(&db.pool, "pg", "", TEST_TEAM, now()).await.unwrap();
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
    /// spawned delete (or import) has reached the point it should wait at.
    pub(crate) async fn until_a_lock_is_awaited(pool: &PgPool) {
        until_locks_are_awaited(pool, 1).await;
    }

    /// Waits until at least `count` lock requests are blocked.
    async fn until_locks_are_awaited(pool: &PgPool, count: i64) {
        for _ in 0..200 {
            let waiting: i64 =
                sqlx::query_scalar("SELECT count(*) FROM pg_locks WHERE NOT granted")
                    .fetch_one(pool)
                    .await
                    .unwrap();
            if waiting >= count {
                return;
            }
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        }
        panic!("fewer than {count} lock requests were ever blocked");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_create_that_waited_on_a_team_delete_is_unknown_name() {
        // Arrange: a transaction deletes team `family` and holds its commit
        // while the create runs.
        let db = test_db().await;
        let family = crate::store::teams::NewTeam {
            name: "family",
            display_name: "",
            owner: crate::db::TEST_USER,
        };
        crate::store::teams::create(&db.pool, &family, now())
            .await
            .unwrap();
        let mut deleting = db.pool.begin().await.unwrap();
        sqlx::query("DELETE FROM teams WHERE name = 'family'")
            .execute(&mut *deleting)
            .await
            .unwrap();
        // `tokio::spawn` runs the create concurrently, like a thread; its
        // `FOR KEY SHARE` read of the team waits on the delete's row lock.
        let pool = db.pool.clone();
        let creating = tokio::spawn(async move { create(&pool, "pg", "", "family", now()).await });
        until_a_lock_is_awaited(&db.pool).await;

        deleting.commit().await.unwrap();
        let outcome = creating.await.unwrap();

        // Not a foreign-key violation (a 500): the team is simply unknown.
        assert!(
            matches!(&outcome, Err(OkfError::UnknownName { noun: "team", .. })),
            "{outcome:?}"
        );
        assert!(list(&db.pool).await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_delete_that_waited_finds_the_catalog_gone() {
        let db = test_db().await;
        // Read first: the fixture user already bumped the generation.
        let start = admin_generation(&db.pool).await.unwrap();
        let pg = create(&db.pool, "pg", "", TEST_TEAM, now()).await.unwrap();
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
        assert_eq!(admin_generation(&db.pool).await.unwrap(), start + 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_entry_written_during_a_delete_is_refused_not_cascaded() {
        let db = test_db().await;
        let pg = create(&db.pool, "pg", "", TEST_TEAM, now()).await.unwrap();
        let changeset: i64 = sqlx::query_scalar(
            "INSERT INTO changesets (catalog_id, actor, state, created_at, updated_at) \
             VALUES ($1, 'a', 'open', $2, $2) RETURNING id",
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

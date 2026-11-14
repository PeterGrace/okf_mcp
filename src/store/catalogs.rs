//! Catalog rows, the per-catalog lock, and the administrative generation.

use chrono::{DateTime, Utc};
// `PgExecutor` is "anything a query can run on": a pool, a connection or a
// transaction. Functions generic over it work inside or outside a transaction.
use sqlx::{PgConnection, PgExecutor, PgPool};

use crate::error::OkfError;
use crate::store::audit::{self, Action, AuditActor, Entry, Target};
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

/// A catalog to create: [`create`]'s input.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewCatalog<'a> {
    /// The new catalog's name, `[a-z0-9-]+`.
    pub name: &'a str,
    /// Free text.
    pub description: &'a str,
    /// The name of the team that owns it.
    pub owner_team: &'a str,
}

/// Creates an empty catalog, records `catalog.created`, and bumps the
/// administrative generation, in one transaction.
///
/// # Arguments
/// * `pool` - the database.
/// * `new` - its name, description and owner team.
/// * `actor` - who, for the audit log.
/// * `now` - the creation time (server clock).
///
/// # Returns
/// The new row.
///
/// # Errors
/// [`OkfError::Refused`] for an invalid or taken name;
/// [`OkfError::UnknownName`] for an unknown team; [`OkfError::Database`]
/// otherwise, a failed audit write included.
pub async fn create(
    pool: &PgPool,
    new: &NewCatalog<'_>,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<Catalog, OkfError> {
    let NewCatalog {
        name,
        description,
        owner_team,
    } = *new;
    if !is_valid_name(name) {
        return Err(OkfError::Refused {
            reason: format!("catalog name {name:?} must be lowercase letters, digits and '-'"),
        });
    }
    // Dropping `tx` without `commit()` (an early `return` or `?`) rolls back.
    let mut tx = pool.begin().await?;
    // `&mut tx` is a `&mut Transaction`, which Rust converts to the
    // `&mut PgConnection` the helper takes (deref coercion).
    audit::lock_actor(&mut tx, actor).await?;
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
    record_catalog(
        &mut tx,
        actor,
        CatalogEvent {
            action: Action::CatalogCreated,
            catalog_id: catalog.id,
            catalog: name,
            details: serde_json::json!({}),
        },
        now,
    )
    .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(catalog)
}

/// Makes `team` the owner of catalog `name` (moving it, or assigning a
/// catalog from before migration 0009), records `catalog.owner_changed`,
/// and bumps the administrative generation, in one transaction. Giving a
/// catalog to the team that owns it already changes nothing, so it writes
/// and records nothing.
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the catalog's name.
/// * `team` - the new owner's team name.
/// * `actor` - who, for the audit log.
/// * `now` - the server clock.
///
/// # Returns
/// The previous owner's name (`None` for a catalog that had none).
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] for an unknown (or malformed) catalog;
/// [`OkfError::UnknownName`] for an unknown team; [`OkfError::Database`],
/// a failed audit write included.
pub async fn set_owner(
    pool: &PgPool,
    name: &str,
    team: &str,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<Option<String>, OkfError> {
    check_name(name)?;
    let mut tx = pool.begin().await?;
    audit::lock_actor(&mut tx, actor).await?;
    let team_id = teams::id_of(&mut tx, team).await?;
    // As in `update_description`: the subquery reads the row as it was
    // before the `UPDATE`, under its row lock, so this returns the owner it
    // replaced even with two changes racing. The outer `Option` is "no such
    // catalog"; the old owner's id and name are `None` for a catalog that
    // had none.
    let previous: Option<(Option<String>, Option<String>)> = sqlx::query_as(
        "UPDATE catalogs c SET owner_team_id = $2 \
         FROM (SELECT id, owner_team_id FROM catalogs WHERE name = $1 FOR UPDATE) old \
         WHERE c.id = old.id \
         RETURNING old.owner_team_id, \
                   (SELECT t.name FROM teams t WHERE t.id = old.owner_team_id)",
    )
    .bind(name)
    .bind(&team_id)
    .fetch_optional(&mut *tx)
    .await?;
    let (previous_id, previous) = previous.ok_or_else(|| OkfError::CatalogNotSelected {
        catalog: name.to_string(),
    })?;
    if previous_id.as_deref() == Some(team_id.as_str()) {
        // Dropping `tx` rolls back the `UPDATE`, which wrote the same value.
        return Ok(previous);
    }
    // `audit_team_ref` of NULL is NULL: a catalog that had no owner.
    let from: Option<i64> = sqlx::query_scalar("SELECT audit_team_ref($1)")
        .bind(&previous_id)
        .fetch_one(&mut *tx)
        .await?;
    let to = audit::team_ref(&mut tx, team).await?;
    // `chain` appends `to` after `from` (if any): the old team, then the new.
    let teams: Vec<i64> = from.into_iter().chain([to]).collect();
    audit::record(
        &mut tx,
        actor,
        &Entry {
            action: Action::CatalogOwnerChanged,
            target: Target::Catalog(name),
            person: None,
            teams: &teams,
            details: serde_json::json!({"from": from, "to": to}),
        },
        now,
    )
    .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(previous)
}

/// Replaces a catalog's description, records
/// `catalog.description_changed`, and bumps the administrative generation,
/// in one transaction. The catalog's `generation` is left alone: its
/// content did not change, so its snapshot is not rebuilt. The same text
/// again changes nothing, so it writes and records nothing.
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the catalog's name.
/// * `description` - the new free text (empty is allowed, as for `create`).
/// * `actor` - who, for the audit log.
/// * `now` - the server clock.
///
/// # Returns
/// The description it had before.
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] if there is no such catalog (a
/// malformed name included); [`OkfError::Database`] otherwise, a failed
/// audit write included.
pub async fn update_description(
    pool: &PgPool,
    name: &str,
    description: &str,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<String, OkfError> {
    check_name(name)?;
    let mut tx = pool.begin().await?;
    audit::lock_actor(&mut tx, actor).await?;
    // A subquery reads the row as it was before the `UPDATE` (one
    // statement sees one snapshot), so this returns the old text; the row
    // lock the `UPDATE` takes keeps two updates from both reading the same.
    let previous: Option<(i64, String)> = sqlx::query_as(
        "UPDATE catalogs c SET description = $2 \
         FROM (SELECT id, description FROM catalogs WHERE name = $1 FOR UPDATE) old \
         WHERE c.id = old.id RETURNING c.id, old.description",
    )
    .bind(name)
    .bind(description)
    .fetch_optional(&mut *tx)
    .await?;
    let (catalog_id, previous) = previous.ok_or_else(|| OkfError::CatalogNotSelected {
        catalog: name.to_string(),
    })?;
    if previous == description {
        // Dropping `tx` rolls back the `UPDATE`, which wrote the same text.
        return Ok(previous);
    }
    record_catalog(
        &mut tx,
        actor,
        CatalogEvent {
            action: Action::CatalogDescriptionChanged,
            catalog_id,
            catalog: name,
            // The length in characters, never the text.
            details: serde_json::json!({"length": description.chars().count()}),
        },
        now,
    )
    .await?;
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
/// [`OkfError::CatalogNotSelected`] if there is none (a malformed name
/// included); [`OkfError::Database`].
// `'e` is a lifetime parameter: the executor is borrowed for the query only.
pub async fn find<'e>(executor: impl PgExecutor<'e>, name: &str) -> Result<Catalog, OkfError> {
    check_name(name)?;
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
/// It records `catalog.deleted` before deleting the row (the entry's teams
/// are read from it).
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the catalog's name.
/// * `force` - delete even with unpublished changes in an open changeset.
/// * `actor` - who, for the audit log.
/// * `now` - the server clock.
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] if there is no such catalog (a
/// malformed name included); [`OkfError::Refused`] if it has unpublished
/// changes and `force` is false; [`OkfError::Database`] otherwise, a failed
/// audit write included.
pub async fn delete(
    pool: &PgPool,
    name: &str,
    force: bool,
    actor: &AuditActor,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    let mut tx = pool.begin().await?;
    audit::lock_actor(&mut tx, actor).await?;
    let catalog = find(&mut *tx, name).await?;
    lock(&mut tx, catalog.id).await?;
    // A delete that waited on the lock may find the row already gone. The
    // generation is the one no publish can move any more.
    let still_there: Option<i64> =
        sqlx::query_scalar("SELECT generation FROM catalogs WHERE id = $1 FOR UPDATE")
            .bind(catalog.id)
            .fetch_optional(&mut *tx)
            .await?;
    let Some(generation) = still_there else {
        return Err(OkfError::CatalogNotSelected {
            catalog: name.to_string(),
        });
    };
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
    record_catalog(
        &mut tx,
        actor,
        CatalogEvent {
            action: Action::CatalogDeleted,
            catalog_id: catalog.id,
            catalog: name,
            // `forced`: unpublished changes went with it.
            details: serde_json::json!({"forced": unpublished, "generation": generation}),
        },
        now,
    )
    .await?;
    sqlx::query("DELETE FROM catalogs WHERE id = $1")
        .bind(catalog.id)
        .execute(&mut *tx)
        .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(())
}

/// Refuses a malformed catalog name as unknown before it reaches SQL: a NUL,
/// which Postgres refuses as a database error (a 500), names no catalog
/// (issue #339).
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] for a name [`is_valid_name`] refuses.
fn check_name(name: &str) -> Result<(), OkfError> {
    if is_valid_name(name) {
        Ok(())
    } else {
        Err(OkfError::CatalogNotSelected {
            catalog: name.to_string(),
        })
    }
}

/// One catalog event to record: [`record_catalog`]'s input.
pub(crate) struct CatalogEvent<'a> {
    /// What happened.
    pub action: Action,
    /// The catalog's id; its row must still exist (its owner team is read).
    pub catalog_id: i64,
    /// The catalog's name.
    pub catalog: &'a str,
    /// The entry's details.
    pub details: serde_json::Value,
}

/// Records an entry about a catalog in the change's transaction, belonging
/// to the catalog's owner team ([`audit::catalog_teams`]).
///
/// # Arguments
/// * `conn` - the change's transaction.
/// * `actor` - who.
/// * `event` - what.
/// * `now` - when (server clock).
///
/// # Errors
/// [`OkfError::Database`] (which fails the change).
pub(crate) async fn record_catalog(
    conn: &mut PgConnection,
    actor: &AuditActor,
    event: CatalogEvent<'_>,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    let teams = audit::catalog_teams(conn, event.catalog_id).await?;
    audit::record(
        conn,
        actor,
        &Entry {
            action: event.action,
            target: Target::Catalog(event.catalog),
            person: None,
            teams: &teams,
            details: event.details,
        },
        now,
    )
    .await?;
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

        let pg = create(
            &db.pool,
            &NewCatalog {
                name: "pg",
                description: "pgrace's catalog",
                owner_team: TEST_TEAM,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        create(
            &db.pool,
            &NewCatalog {
                name: "aa-work",
                description: "",
                owner_team: TEST_TEAM,
            },
            &AuditActor::cli(),
            now(),
        )
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

        let catalog = create(
            &db.pool,
            &NewCatalog {
                name: "pg",
                description: "",
                owner_team: TEST_TEAM,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();

        assert_eq!(catalog.owner_team.as_deref(), Some(TEST_TEAM));
        assert_eq!(find(&db.pool, "pg").await.unwrap(), catalog);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_unknown_owner_is_unknown_name_and_creates_nothing() {
        let db = test_db().await;
        // Read first: the fixture user already bumped the generation.
        let start = admin_generation(&db.pool).await.unwrap();

        let made = create(
            &db.pool,
            &NewCatalog {
                name: "pg",
                description: "",
                owner_team: "nobody",
            },
            &AuditActor::cli(),
            now(),
        )
        .await;

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
        create(
            &db.pool,
            &NewCatalog {
                name: "pg",
                description: "",
                owner_team: TEST_TEAM,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let before = admin_generation(&db.pool).await.unwrap();

        let previous = set_owner(&db.pool, "pg", "alice", &AuditActor::cli(), Utc::now())
            .await
            .unwrap();

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

        let previous = set_owner(
            &db.pool,
            "legacy",
            TEST_TEAM,
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();

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
        create(
            &db.pool,
            &NewCatalog {
                name: "pg",
                description: "",
                owner_team: TEST_TEAM,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let start = admin_generation(&db.pool).await.unwrap();

        let no_catalog =
            set_owner(&db.pool, "nope", TEST_TEAM, &AuditActor::cli(), Utc::now()).await;
        let no_team = set_owner(&db.pool, "pg", "nobody", &AuditActor::cli(), Utc::now()).await;

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
        let pg = create(
            &db.pool,
            &NewCatalog {
                name: "pg",
                description: "copy",
                owner_team: TEST_TEAM,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();

        let previous = update_description(
            &db.pool,
            "pg",
            "source of truth",
            &AuditActor::cli(),
            Utc::now(),
        )
        .await
        .unwrap();
        let cleared = update_description(&db.pool, "pg", "", &AuditActor::cli(), Utc::now())
            .await
            .unwrap();

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
        create(
            &db.pool,
            &NewCatalog {
                name: "pg",
                description: "start",
                owner_team: TEST_TEAM,
            },
            &AuditActor::cli(),
            now(),
        )
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
            tokio::spawn(async move {
                update_description(&pool, "pg", text, &AuditActor::cli(), Utc::now()).await
            })
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

        let outcome =
            update_description(&db.pool, "nope", "x", &AuditActor::cli(), Utc::now()).await;

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
        create(
            &db.pool,
            &NewCatalog {
                name: "pg",
                description: "",
                owner_team: TEST_TEAM,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();

        for name in ["", "Bad Name", "a/b", "ünï"] {
            assert!(
                matches!(
                    create(
                        &db.pool,
                        &NewCatalog {
                            name: name,
                            description: "",
                            owner_team: TEST_TEAM
                        },
                        &AuditActor::cli(),
                        now()
                    )
                    .await,
                    Err(OkfError::Refused { .. })
                ),
                "{name}"
            );
        }
        let duplicate = create(
            &db.pool,
            &NewCatalog {
                name: "pg",
                description: "",
                owner_team: TEST_TEAM,
            },
            &AuditActor::cli(),
            now(),
        )
        .await;

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
        let pg = create(
            &db.pool,
            &NewCatalog {
                name: "pg",
                description: "",
                owner_team: TEST_TEAM,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
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

        let refused = delete(&db.pool, "pg", false, &AuditActor::cli(), Utc::now()).await;
        assert!(matches!(refused, Err(OkfError::Refused { reason }) if reason.contains("--force")));

        delete(&db.pool, "pg", true, &AuditActor::cli(), Utc::now())
            .await
            .unwrap();

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
        let pg = create(
            &db.pool,
            &NewCatalog {
                name: "pg",
                description: "",
                owner_team: TEST_TEAM,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO changesets (catalog_id, actor, state, created_at, updated_at) \
             VALUES ($1, 'a', 'open', $2, $2)",
        )
        .bind(pg.id)
        .bind(now())
        .execute(&db.pool)
        .await
        .unwrap();

        delete(&db.pool, "pg", false, &AuditActor::cli(), Utc::now())
            .await
            .unwrap();

        assert!(list(&db.pool).await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_lock_holds_other_transactions_off_until_commit() {
        let db = test_db().await;
        let pg = create(
            &db.pool,
            &NewCatalog {
                name: "pg",
                description: "",
                owner_team: TEST_TEAM,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
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

    /// Counts the lock requests blocked in this test's own database: the
    /// test server may be shared by other tests, whose waits are not ours.
    pub(crate) const AWAITED_LOCKS: &str = "SELECT count(*) FROM pg_locks l \
         JOIN pg_stat_activity a ON a.pid = l.pid \
         WHERE NOT l.granted AND a.datname = current_database()";

    /// Waits until some backend is blocked on a lock, so a test knows the
    /// spawned delete (or import) has reached the point it should wait at.
    pub(crate) async fn until_a_lock_is_awaited(pool: &PgPool) {
        until_locks_are_awaited(pool, 1).await;
    }

    /// Waits until at least `count` lock requests are blocked.
    pub(crate) async fn until_locks_are_awaited(pool: &PgPool, count: i64) {
        for _ in 0..200 {
            let waiting: i64 = sqlx::query_scalar(AWAITED_LOCKS)
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
        crate::store::teams::create(&db.pool, &family, &AuditActor::cli(), now())
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
        let creating = tokio::spawn(async move {
            create(
                &pool,
                &NewCatalog {
                    name: "pg",
                    description: "",
                    owner_team: "family",
                },
                &AuditActor::cli(),
                now(),
            )
            .await
        });
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
        let pg = create(
            &db.pool,
            &NewCatalog {
                name: "pg",
                description: "",
                owner_team: TEST_TEAM,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let mut first = db.pool.begin().await.unwrap();
        lock(&mut first, pg.id).await.unwrap();
        // `tokio::spawn` runs the second delete concurrently, like a thread;
        // it blocks on the lock `first` holds.
        let pool = db.pool.clone();
        let second = tokio::spawn(async move {
            delete(&pool, "pg", false, &AuditActor::cli(), Utc::now()).await
        });
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
        let pg = create(
            &db.pool,
            &NewCatalog {
                name: "pg",
                description: "",
                owner_team: TEST_TEAM,
            },
            &AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
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
        let deleting = tokio::spawn(async move {
            delete(&pool, "pg", false, &AuditActor::cli(), Utc::now()).await
        });
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

    // ---- The audit log (M14 Task 6b) ----

    use crate::store::audit::testing::entries;
    use serde_json::json;

    /// Creates catalog `name`, owned by [`TEST_TEAM`].
    async fn seed(pool: &PgPool, name: &str) -> Catalog {
        let new = NewCatalog {
            name,
            description: "",
            owner_team: TEST_TEAM,
        };
        create(pool, &new, &AuditActor::cli(), now()).await.unwrap()
    }

    /// Creates the shared team `family`, owned by the fixture user.
    async fn family(pool: &PgPool) {
        let team = crate::store::teams::NewTeam {
            name: "family",
            display_name: "",
            owner: crate::db::TEST_USER,
        };
        crate::store::teams::create(pool, &team, &AuditActor::cli(), now())
            .await
            .unwrap();
    }

    /// Gives catalog `catalog_id` an open changeset with one entry.
    async fn unpublished_work(pool: &PgPool, catalog_id: i64) {
        let changeset: i64 = sqlx::query_scalar(
            "INSERT INTO changesets (catalog_id, actor, state, created_at, updated_at) \
             VALUES ($1, 'a', 'open', $2, $2) RETURNING id",
        )
        .bind(catalog_id)
        .bind(now())
        .fetch_one(pool)
        .await
        .unwrap();
        sqlx::query(
            "INSERT INTO changeset_entries (changeset_id, path, content) VALUES ($1, 'a.md', 'x')",
        )
        .bind(changeset)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_records_catalog_created_for_its_owner_team() {
        let db = test_db().await;

        seed(&db.pool, "pg").await;

        let created = entries(&db.pool, "catalog.created").await;
        assert_eq!(created.len(), 1, "{created:?}");
        assert_eq!(created[0].target_kind, "catalog");
        assert_eq!(created[0].target.as_deref(), Some("pg"));
        assert_eq!(created[0].person, None);
        // A personal team shows as its person.
        assert_eq!(created[0].teams, [crate::db::TEST_USER]);
        assert_eq!(created[0].details, json!({}));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_owner_records_both_teams_and_nothing_for_the_same_owner() {
        let db = test_db().await;
        family(&db.pool).await;
        seed(&db.pool, "pg").await;
        let before = admin_generation(&db.pool).await.unwrap();

        set_owner(&db.pool, "pg", "family", &AuditActor::cli(), now())
            .await
            .unwrap();
        let after = admin_generation(&db.pool).await.unwrap();
        let again = set_owner(&db.pool, "pg", "family", &AuditActor::cli(), now())
            .await
            .unwrap();

        assert_eq!(again.as_deref(), Some("family"));
        assert_eq!(after, before + 1);
        assert_eq!(
            admin_generation(&db.pool).await.unwrap(),
            after,
            "no change"
        );
        let moved = entries(&db.pool, "catalog.owner_changed").await;
        assert_eq!(moved.len(), 1, "{moved:?}");
        assert_eq!(moved[0].target.as_deref(), Some("pg"));
        assert_eq!(moved[0].teams, [crate::db::TEST_USER, "family"]);
        let (from, to) = (&moved[0].details["from"], &moved[0].details["to"]);
        assert!(
            from.is_i64() && to.is_i64() && from != to,
            "{:?}",
            moved[0].details
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_owner_of_a_legacy_catalog_records_no_old_team() {
        let db = test_db().await;
        sqlx::query("INSERT INTO catalogs (name, created_at) VALUES ('legacy', now())")
            .execute(&db.pool)
            .await
            .unwrap();

        set_owner(&db.pool, "legacy", TEST_TEAM, &AuditActor::cli(), now())
            .await
            .unwrap();

        let moved = entries(&db.pool, "catalog.owner_changed").await;
        assert_eq!(moved.len(), 1, "{moved:?}");
        assert_eq!(moved[0].teams, [crate::db::TEST_USER]);
        assert_eq!(moved[0].details["from"], serde_json::Value::Null);
        assert!(moved[0].details.get("from").is_some(), "the key is there");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn update_description_records_its_length_and_nothing_for_the_same_text() {
        let db = test_db().await;
        seed(&db.pool, "pg").await;

        update_description(&db.pool, "pg", "naïve", &AuditActor::cli(), now())
            .await
            .unwrap();
        let previous = update_description(&db.pool, "pg", "naïve", &AuditActor::cli(), now())
            .await
            .unwrap();

        assert_eq!(previous, "naïve");
        let changed = entries(&db.pool, "catalog.description_changed").await;
        assert_eq!(changed.len(), 1, "{changed:?}");
        assert_eq!(changed[0].target.as_deref(), Some("pg"));
        assert_eq!(changed[0].teams, [crate::db::TEST_USER]);
        // Characters, not bytes; never the text.
        assert_eq!(changed[0].details, json!({"length": 5}));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn delete_records_each_deletion_and_nothing_when_refused() {
        let db = test_db().await;
        seed(&db.pool, "plain").await;
        let busy = seed(&db.pool, "busy").await;
        unpublished_work(&db.pool, busy.id).await;
        let actor = AuditActor::cli();

        let refused = delete(&db.pool, "busy", false, &actor, now()).await;
        delete(&db.pool, "plain", false, &actor, now())
            .await
            .unwrap();
        delete(&db.pool, "busy", true, &actor, now()).await.unwrap();

        assert!(matches!(refused, Err(OkfError::Refused { .. })));
        let deleted = entries(&db.pool, "catalog.deleted").await;
        assert_eq!(deleted.len(), 2, "{deleted:?}");
        assert_eq!(deleted[0].target.as_deref(), Some("plain"));
        // Read before the row went.
        assert_eq!(deleted[0].teams, [crate::db::TEST_USER]);
        assert_eq!(
            deleted[0].details,
            json!({"forced": false, "generation": 0})
        );
        assert_eq!(deleted[1].target.as_deref(), Some("busy"));
        assert_eq!(deleted[1].details, json!({"forced": true, "generation": 0}));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_failed_audit_write_fails_the_catalog_change() {
        let db = test_db().await;
        seed(&db.pool, "pg").await;
        family(&db.pool).await;
        crate::http::api::testing::fail_on(&db.pool, "BEFORE INSERT ON audit_entries FOR EACH ROW")
            .await;
        let actor = AuditActor::cli();
        let new = NewCatalog {
            name: "other",
            description: "",
            owner_team: TEST_TEAM,
        };

        let created = create(&db.pool, &new, &actor, now()).await;
        let moved = set_owner(&db.pool, "pg", "family", &actor, now()).await;
        let described = update_description(&db.pool, "pg", "new", &actor, now()).await;
        let deleted = delete(&db.pool, "pg", false, &actor, now()).await;

        assert!(created.is_err() && moved.is_err() && described.is_err() && deleted.is_err());
        let pg = find(&db.pool, "pg").await.unwrap();
        assert_eq!(pg.owner_team.as_deref(), Some(TEST_TEAM));
        assert_eq!(pg.description, "");
        assert!(matches!(
            find(&db.pool, "other").await,
            Err(OkfError::CatalogNotSelected { .. })
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_malformed_catalog_name_is_unknown_and_never_reaches_sql() {
        let db = test_db().await;
        let actor = AuditActor::cli();
        let bad = "p\0g";

        let found = find(&db.pool, bad).await.map(|_| ());
        let moved = set_owner(&db.pool, bad, TEST_TEAM, &actor, now())
            .await
            .map(|_| ());
        let described = update_description(&db.pool, bad, "", &actor, now())
            .await
            .map(|_| ());
        let deleted = delete(&db.pool, bad, false, &actor, now()).await;

        // A NUL would be a database error (a 500) if it were bound.
        for outcome in [found, moved, described, deleted] {
            assert!(
                matches!(outcome, Err(OkfError::CatalogNotSelected { .. })),
                "{outcome:?}"
            );
        }
    }
}

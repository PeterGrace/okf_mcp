//! API key rows and their catalog grants. Only a key's SHA-256 is stored;
//! the key itself is returned once, by [`create`].

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

use crate::error::OkfError;
use crate::store::catalogs::{self, bump_admin_generation, is_unique_violation};

/// What a key may do. Each level includes the ones before it, so the
/// derived `Ord` (`Read < Write < Admin`, the declaration order) answers
/// "at least write?".
// `clap::ValueEnum` lets the command line parse `--access read|write|admin`
// straight into this enum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, clap::ValueEnum)]
pub enum Access {
    /// The read tools and export.
    Read,
    /// Read, plus the write tools and publish.
    Write,
    /// Write, plus import into granted catalogs and using any key's changeset.
    Admin,
}

impl Access {
    /// The spelling stored in `api_keys.access`.
    ///
    /// # Returns
    /// `"read"`, `"write"` or `"admin"`.
    pub fn as_str(self) -> &'static str {
        match self {
            Access::Read => "read",
            Access::Write => "write",
            Access::Admin => "admin",
        }
    }

    /// Parses the stored spelling.
    ///
    /// # Arguments
    /// * `text` - an `api_keys.access` value.
    ///
    /// # Returns
    /// The level, or `None` for anything other than [`Access::as_str`]'s spellings.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "read" => Some(Access::Read),
            "write" => Some(Access::Write),
            "admin" => Some(Access::Admin),
            _ => None,
        }
    }
}

/// Which catalogs a new key reaches.
// `'a` ties the borrowed slice to the caller's names: the enum holds a view of
// them, not a copy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Grants<'a> {
    /// Every catalog, present and future.
    All,
    /// These catalogs, by name.
    Catalogs(&'a [String]),
}

/// A key to create.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewKey<'a> {
    /// Grouping name, `[A-Za-z0-9._-]+`.
    pub id: &'a str,
    /// Its access level.
    pub access: Access,
    /// Its catalogs.
    pub grants: Grants<'a>,
    /// Its default catalog; must be granted.
    pub default_catalog: Option<&'a str>,
    /// The actor its writes are recorded as (`claude-code/claude-opus-5-5`).
    pub actor: &'a str,
}

/// A newly issued credential. Its bearer secret is returned only once.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedKey {
    pub id: String,
    pub key: String,
}

/// A stored key (never the key itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyRecord {
    /// Its id.
    pub id: String,
    /// Its grouping name.
    pub name: String,
    /// SHA-256 of the key.
    pub sha256: Vec<u8>,
    /// Its access level.
    pub access: Access,
    /// Granted every catalog.
    pub all_catalogs: bool,
    /// Granted catalogs by name, sorted (empty with `all_catalogs`).
    pub catalogs: Vec<String>,
    /// Its default catalog.
    pub default_catalog: Option<String>,
    /// The actor its writes are recorded as.
    pub actor: String,
    /// When it was created.
    pub created_at: DateTime<Utc>,
    /// When it was revoked; a revoked key authenticates nothing.
    pub revoked_at: Option<DateTime<Utc>>,
}

/// The row as selected, before `access` is parsed.
#[derive(sqlx::FromRow)]
struct KeyRow {
    id: String,
    name: String,
    sha256: Vec<u8>,
    access: String,
    all_catalogs: bool,
    catalogs: Vec<String>,
    default_catalog: Option<String>,
    actor: String,
    created_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
}

/// Is `id` a valid key id (the schema's `CHECK (id ~ '^[A-Za-z0-9._-]+$')`)?
fn is_valid_name(name: &str) -> bool {
    !name.is_empty()
        && name
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Creates a key and its grants, and bumps the administrative generation, in
/// one transaction.
///
/// # Arguments
/// * `pool` - the database.
/// * `new` - the key's id, access, grants, default catalog and actor.
/// * `now` - the creation time (server clock).
///
/// # Returns
/// The key itself: print it once; it is not stored and cannot be shown again.
///
/// # Errors
/// [`OkfError::Refused`] for an invalid or taken id, no catalogs, or a default
/// catalog that is not granted; [`OkfError::CatalogNotSelected`] for an unknown catalog
/// (granted or default); [`OkfError::Io`] if the OS random source fails;
/// [`OkfError::Database`] otherwise.
pub async fn create(
    pool: &PgPool,
    new: &NewKey<'_>,
    now: DateTime<Utc>,
) -> Result<String, OkfError> {
    Ok(create_with_id(pool, new, now).await?.key)
}

/// Creates a key and also returns its immutable generated id.
pub async fn create_with_id(
    pool: &PgPool,
    new: &NewKey<'_>,
    now: DateTime<Utc>,
) -> Result<CreatedKey, OkfError> {
    if !is_valid_name(new.id) {
        return Err(OkfError::Refused {
            reason: format!(
                "key name {:?} must be letters, digits, '.', '_' and '-'",
                new.id
            ),
        });
    }
    if matches!(new.grants, Grants::Catalogs([])) {
        // A slice pattern: `[]` matches an empty slice.
        return Err(OkfError::Refused {
            reason: format!(
                "key {:?} must be granted a catalog, or every catalog",
                new.id
            ),
        });
    }
    // Where the default sits among the granted names, checked before any query.
    let default_position = match (new.default_catalog, new.grants) {
        (Some(default), Grants::Catalogs(names)) => Some(
            names
                .iter()
                .position(|name| name == default)
                .ok_or_else(|| OkfError::Refused {
                    reason: format!("default catalog {default:?} is not granted to the key"),
                })?,
        ),
        (Some(_), Grants::All) | (None, _) => None,
    };
    let mut tx = pool.begin().await?;
    // Serialise issuance within one grouping name so concurrent requests get
    // the same clean duplicate-name refusal rather than racing the index.
    sqlx::query("SELECT pg_advisory_xact_lock(hashtextextended($1, 0))")
        .bind(new.id)
        .execute(&mut *tx)
        .await?;
    let granted = match new.grants {
        Grants::All => Vec::new(),
        Grants::Catalogs(names) => catalog_ids(&mut tx, names).await?,
    };
    let default_catalog_id = match (new.default_catalog, default_position) {
        (_, Some(position)) => Some(granted[position]),
        // An unknown catalog is `CatalogNotSelected`; any other error propagates.
        (Some(default), None) => Some(catalogs::find(&mut *tx, default).await?.id),
        (None, None) => None,
    };
    let key = crate::keys::generate()?;
    let inserted = sqlx::query_scalar::<_, String>(
        "INSERT INTO api_keys \
         (name, sha256, access, all_catalogs, default_catalog_id, actor, created_at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7) RETURNING id",
    )
    .bind(new.id)
    .bind(crate::keys::hash(&key).as_slice())
    .bind(new.access.as_str())
    .bind(matches!(new.grants, Grants::All))
    .bind(default_catalog_id)
    .bind(new.actor)
    .bind(now)
    .fetch_one(&mut *tx)
    .await;
    let id = match inserted {
        Ok(id) => id,
        Err(error)
            if is_unique_violation(&error)
                && error
                    .as_database_error()
                    .and_then(|db_error| db_error.constraint())
                    == Some("api_keys_live_name_actor") =>
        {
            return Err(OkfError::Refused {
                reason: format!(
                    "an active key named {:?} with actor {:?} already exists",
                    new.id, new.actor
                ),
            })
        }
        Err(error) => return Err(error.into()),
    };
    // One statement for every grant: `unnest` turns the bound array into rows.
    // A name given twice is one grant (`ON CONFLICT DO NOTHING`).
    sqlx::query(
        "INSERT INTO key_catalogs (key_id, catalog_id) \
         SELECT $1, unnest($2::bigint[]) ON CONFLICT DO NOTHING",
    )
    .bind(&id)
    .bind(&granted)
    .execute(&mut *tx)
    .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(CreatedKey { id, key })
}

/// The ids of the catalogs called `names`, in the same order, in one query.
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] for the first name with no catalog;
/// [`OkfError::Database`].
async fn catalog_ids(conn: &mut PgConnection, names: &[String]) -> Result<Vec<i64>, OkfError> {
    // `WITH ORDINALITY` numbers the array's elements, so the rows come back in
    // the caller's order with a NULL id for each unknown name.
    let rows: Vec<(String, Option<i64>)> = sqlx::query_as(
        "SELECT n.name, c.id FROM unnest($1::text[]) WITH ORDINALITY AS n (name, ord) \
         LEFT JOIN catalogs c ON c.name = n.name ORDER BY n.ord",
    )
    .bind(names)
    .fetch_all(&mut *conn)
    .await?;
    rows.into_iter()
        .map(|(name, id)| id.ok_or(OkfError::CatalogNotSelected { catalog: name }))
        // Collecting an iterator of `Result`s into one `Result<Vec<_>, _>`
        // stops at the first error, as a loop with `raise` would.
        .collect()
}

/// Every key, by id, with its granted catalogs.
///
/// # Arguments
/// * `pool` - the database.
///
/// # Returns
/// The records, ordered by id, revoked keys included.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn list(pool: &PgPool) -> Result<Vec<KeyRecord>, OkfError> {
    let rows = sqlx::query_as::<_, KeyRow>(
        "SELECT k.id, k.name, k.sha256, k.access, k.all_catalogs, d.name AS default_catalog, k.actor, \
                k.created_at, k.revoked_at, \
                COALESCE(array_agg(c.name ORDER BY c.name) FILTER (WHERE c.name IS NOT NULL), \
                         '{}'::text[]) AS catalogs \
         FROM api_keys k \
         LEFT JOIN catalogs d ON d.id = k.default_catalog_id \
         LEFT JOIN key_catalogs g ON g.key_id = k.id \
         LEFT JOIN catalogs c ON c.id = g.catalog_id \
         GROUP BY k.id, d.name ORDER BY k.name, k.actor, k.created_at, k.id",
    )
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|row| KeyRecord {
            id: row.id,
            name: row.name,
            sha256: row.sha256,
            access: Access::parse(&row.access).expect("the schema checks api_keys.access"),
            all_catalogs: row.all_catalogs,
            catalogs: row.catalogs,
            default_catalog: row.default_catalog,
            actor: row.actor,
            created_at: row.created_at,
            revoked_at: row.revoked_at,
        })
        .collect())
}

/// Revokes a key: it authenticates nothing from then on (M4 notices within
/// `poll_interval`), and bumps the administrative generation.
///
/// # Arguments
/// * `pool` - the database.
/// * `id` - the key's id.
/// * `now` - the revocation time (server clock).
///
/// # Errors
/// [`OkfError::Refused`] if there is no such key or it is already revoked;
/// [`OkfError::Database`].
pub async fn revoke_target(
    pool: &PgPool,
    target: &str,
    actor: Option<&str>,
    now: DateTime<Utc>,
) -> Result<String, OkfError> {
    let mut tx = pool.begin().await?;
    let id = resolve_active(&mut tx, target, actor).await?;
    let revoked =
        sqlx::query("UPDATE api_keys SET revoked_at = $2 WHERE id = $1 AND revoked_at IS NULL")
            .bind(&id)
            .bind(now)
            .execute(&mut *tx)
            .await?;
    if revoked.rows_affected() == 0 {
        return Err(OkfError::Refused {
            reason: format!("no active key {id:?}"),
        });
    }
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(id)
}

/// Revokes a key selected by immutable id or an unambiguous live name.
pub async fn revoke(pool: &PgPool, target: &str, now: DateTime<Utc>) -> Result<(), OkfError> {
    revoke_target(pool, target, None, now).await.map(|_| ())
}

/// Grants a key a catalog, and bumps the administrative generation. Already
/// granted is a no-op that leaves the generation alone, so a running server
/// does not reload for nothing.
///
/// # Arguments
/// * `pool` - the database.
/// * `id` - the key's id.
/// * `catalog` - the catalog's name.
///
/// # Errors
/// [`OkfError::Refused`] if there is no such key, it is revoked, or it already
/// has every catalog (`--all`); [`OkfError::CatalogNotSelected`] if there is no such
/// catalog; [`OkfError::Database`].
pub async fn grant_target(
    pool: &PgPool,
    target: &str,
    actor: Option<&str>,
    catalog: &str,
) -> Result<String, OkfError> {
    let mut tx = pool.begin().await?;
    let catalog = catalogs::find(&mut *tx, catalog).await?;
    let id = resolve_active(&mut tx, target, actor).await?;
    require_named_grants(&mut tx, &id).await?;
    let inserted = sqlx::query(
        "INSERT INTO key_catalogs (key_id, catalog_id) VALUES ($1, $2) ON CONFLICT DO NOTHING",
    )
    .bind(&id)
    .bind(catalog.id)
    .execute(&mut *tx)
    .await?;
    if inserted.rows_affected() == 0 {
        return Ok(id);
    }
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(id)
}

/// Grants a catalog to a key selected by immutable id or unambiguous name.
pub async fn grant(pool: &PgPool, target: &str, catalog: &str) -> Result<(), OkfError> {
    grant_target(pool, target, None, catalog).await.map(|_| ())
}

/// Removes a key's grant of a catalog; if it was the key's default catalog,
/// the default is cleared. Bumps the administrative generation; not granted
/// is a no-op that leaves the generation alone.
///
/// # Arguments
/// As [`grant`].
///
/// # Errors
/// As [`grant`].
pub async fn ungrant_target(
    pool: &PgPool,
    target: &str,
    actor: Option<&str>,
    catalog: &str,
) -> Result<String, OkfError> {
    let mut tx = pool.begin().await?;
    let catalog = catalogs::find(&mut *tx, catalog).await?;
    let id = resolve_active(&mut tx, target, actor).await?;
    require_named_grants(&mut tx, &id).await?;
    let deleted = sqlx::query("DELETE FROM key_catalogs WHERE key_id = $1 AND catalog_id = $2")
        .bind(&id)
        .bind(catalog.id)
        .execute(&mut *tx)
        .await?;
    let cleared = sqlx::query(
        "UPDATE api_keys SET default_catalog_id = NULL WHERE id = $1 AND default_catalog_id = $2",
    )
    .bind(&id)
    .bind(catalog.id)
    .execute(&mut *tx)
    .await?;
    if deleted.rows_affected() + cleared.rows_affected() == 0 {
        return Ok(id);
    }
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(id)
}

/// Removes a catalog grant from a key selected by id or unambiguous name.
pub async fn ungrant(pool: &PgPool, target: &str, catalog: &str) -> Result<(), OkfError> {
    ungrant_target(pool, target, None, catalog)
        .await
        .map(|_| ())
}

async fn resolve_active(
    conn: &mut PgConnection,
    target: &str,
    actor: Option<&str>,
) -> Result<String, OkfError> {
    if actor.is_some() {
        let is_id: bool = sqlx::query_scalar("SELECT EXISTS(SELECT 1 FROM api_keys WHERE id = $1)")
            .bind(target)
            .fetch_one(&mut *conn)
            .await?;
        if is_id {
            return Err(OkfError::Refused {
                reason: format!(
                    "--actor selects a key name and cannot be used with key id {target:?}"
                ),
            });
        }
    }
    if actor.is_none() {
        let exact: Option<String> = sqlx::query_scalar("SELECT id FROM api_keys WHERE id = $1")
            .bind(target)
            .fetch_optional(&mut *conn)
            .await?;
        if let Some(id) = exact {
            return Ok(id);
        }
    }
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM api_keys WHERE revoked_at IS NULL AND name = $1 \
         AND ($2::text IS NULL OR actor = $2) \
         ORDER BY actor, id",
    )
    .bind(target)
    .bind(actor)
    .fetch_all(&mut *conn)
    .await?;
    match ids.as_slice() {
        [id] => Ok(id.clone()),
        [] => Err(OkfError::Refused {
            reason: match actor {
                Some(actor) => format!("no active key named {target:?} with actor {actor:?}"),
                None => format!("no active key or key id {target:?}"),
            },
        }),
        _ => Err(OkfError::Refused {
            reason: format!(
                "key name {target:?} is ambiguous; use its id or pass --actor (matching ids: {})",
                ids.join(", ")
            ),
        }),
    }
}

/// Refuses an unknown key id, a revoked key (as [`revoke`] does: its grants
/// no longer matter), and a key granted every catalog: its grants are not
/// per catalog, so granting or removing one would change nothing it reaches.
async fn require_named_grants(conn: &mut PgConnection, id: &str) -> Result<(), OkfError> {
    // `Option<(bool, bool)>`: `None` when there is no such row.
    let key: Option<(bool, bool)> =
        sqlx::query_as("SELECT all_catalogs, revoked_at IS NOT NULL FROM api_keys WHERE id = $1")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
    match key {
        Some((false, false)) => Ok(()),
        Some((_, true)) => Err(OkfError::Refused {
            reason: format!("key {id:?} is revoked"),
        }),
        Some((true, false)) => Err(OkfError::Refused {
            reason: format!("key {id:?} is granted every catalog (--all)"),
        }),
        None => Err(OkfError::Refused {
            reason: format!("no key {id:?}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;
    use chrono::TimeZone;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 2, 12, 0, 0).unwrap()
    }

    fn new_key<'a>(id: &'a str, grants: Grants<'a>, default: Option<&'a str>) -> NewKey<'a> {
        NewKey {
            id,
            access: Access::Write,
            grants,
            default_catalog: default,
            actor: "claude-code/test",
        }
    }

    #[test]
    fn access_round_trips_its_spelling_and_orders_by_level() {
        let levels = [Access::Read, Access::Write, Access::Admin];

        let parsed: Vec<Option<Access>> = levels
            .iter()
            .map(|level| Access::parse(level.as_str()))
            .collect();

        assert_eq!(parsed, levels.map(Some));
        assert_eq!(Access::parse("root"), None);
        assert!(Access::Read < Access::Write && Access::Write < Access::Admin);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_stores_only_the_hash_and_the_grants() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", now()).await.unwrap();
        catalogs::create(&db.pool, "work", "", now()).await.unwrap();
        // "pg" twice: one grant.
        let names = ["work".to_string(), "pg".to_string(), "pg".to_string()];

        let key = create(
            &db.pool,
            &new_key("laptop", Grants::Catalogs(&names), Some("pg")),
            now(),
        )
        .await
        .unwrap();

        let keys = list(&db.pool).await.unwrap();
        assert_eq!(keys.len(), 1);
        let record = &keys[0];
        assert_eq!(record.name, "laptop");
        assert_eq!(record.access, Access::Write);
        assert!(!record.all_catalogs);
        assert_eq!(record.catalogs, ["pg", "work"]);
        assert_eq!(record.default_catalog.as_deref(), Some("pg"));
        assert_eq!(record.actor, "claude-code/test");
        assert!(crate::keys::matches(&record.sha256, &key));
        assert_eq!(record.created_at, now());
        assert_eq!(record.revoked_at, None);
        assert_eq!(catalogs::admin_generation(&db.pool).await.unwrap(), 3);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bad_requests_are_refused() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", now()).await.unwrap();
        let pg = ["pg".to_string()];
        let missing = ["pg".to_string(), "nope".to_string()];
        create(&db.pool, &new_key("a", Grants::All, None), now())
            .await
            .unwrap();

        let unknown = create(
            &db.pool,
            &new_key("b", Grants::Catalogs(&missing), None),
            now(),
        )
        .await;
        let duplicate = create(&db.pool, &new_key("a", Grants::All, None), now()).await;
        let bad_id = create(&db.pool, &new_key("has space", Grants::All, None), now()).await;
        let no_catalogs = create(&db.pool, &new_key("d", Grants::Catalogs(&[]), None), now()).await;
        let bad_default = create(
            &db.pool,
            &new_key("c", Grants::Catalogs(&pg), Some("work")),
            now(),
        )
        .await;

        assert!(matches!(
            unknown,
            Err(OkfError::CatalogNotSelected { ref catalog }) if catalog == "nope"
        ));
        assert!(matches!(
            duplicate,
            Err(OkfError::Refused { reason }) if reason.contains("already exists")
        ));
        assert!(matches!(bad_id, Err(OkfError::Refused { .. })));
        assert!(matches!(no_catalogs, Err(OkfError::Refused { .. })));
        assert!(matches!(bad_default, Err(OkfError::Refused { .. })));
        // Only the first key exists, and only its creation bumped the generation.
        assert_eq!(list(&db.pool).await.unwrap().len(), 1);
        assert_eq!(catalogs::admin_generation(&db.pool).await.unwrap(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoke_grant_and_ungrant() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", now()).await.unwrap();
        catalogs::create(&db.pool, "work", "", now()).await.unwrap();
        let pg = ["pg".to_string()];
        create(
            &db.pool,
            &new_key("k", Grants::Catalogs(&pg), Some("pg")),
            now(),
        )
        .await
        .unwrap();
        create(&db.pool, &new_key("all", Grants::All, None), now())
            .await
            .unwrap();

        grant(&db.pool, "k", "work").await.unwrap();
        grant(&db.pool, "k", "work").await.unwrap(); // already granted: a no-op
        ungrant(&db.pool, "k", "pg").await.unwrap();
        ungrant(&db.pool, "k", "pg").await.unwrap(); // not granted: a no-op
        revoke(&db.pool, "k", now()).await.unwrap();

        let record = &list(&db.pool).await.unwrap()[1];
        assert_eq!(record.name, "k");
        assert_eq!(record.catalogs, ["work"]);
        // Losing the default catalog's grant clears the default.
        assert_eq!(record.default_catalog, None);
        assert_eq!(record.revoked_at, Some(now()));
        // Two catalogs, two keys, one grant, one ungrant, one revoke: the
        // no-ops bumped nothing.
        assert_eq!(catalogs::admin_generation(&db.pool).await.unwrap(), 7);
        assert!(matches!(
            revoke(&db.pool, "k", now()).await,
            Err(OkfError::Refused { .. })
        ));
        assert!(matches!(
            grant(&db.pool, "k", "pg").await,
            Err(OkfError::Refused { reason }) if reason.contains("no active key")
        ));
        assert!(matches!(
            ungrant(&db.pool, "k", "work").await,
            Err(OkfError::Refused { .. })
        ));
        assert!(matches!(
            grant(&db.pool, "nope", "pg").await,
            Err(OkfError::Refused { .. })
        ));
        assert!(matches!(
            grant(&db.pool, "k", "nope").await,
            Err(OkfError::CatalogNotSelected { .. })
        ));
        assert!(matches!(
            grant(&db.pool, "all", "pg").await,
            Err(OkfError::Refused { reason }) if reason.contains("every catalog")
        ));
        assert!(matches!(
            ungrant(&db.pool, "all", "pg").await,
            Err(OkfError::Refused { .. })
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn deleting_a_catalog_clears_it_as_a_default() {
        let db = test_db().await;
        catalogs::create(&db.pool, "pg", "", now()).await.unwrap();
        create(&db.pool, &new_key("k", Grants::All, Some("pg")), now())
            .await
            .unwrap();
        let unknown_default =
            create(&db.pool, &new_key("j", Grants::All, Some("nope")), now()).await;

        catalogs::delete(&db.pool, "pg", false).await.unwrap();
        catalogs::create(&db.pool, "pg", "", now()).await.unwrap();

        // A re-created catalog of the same name is not the old default.
        assert_eq!(list(&db.pool).await.unwrap()[0].default_catalog, None);
        assert!(matches!(
            unknown_default,
            Err(OkfError::CatalogNotSelected { .. })
        ));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn names_group_actors_and_revoked_pairs_can_be_reissued() {
        let db = test_db().await;
        let work_catalog = catalogs::create(&db.pool, "work", "", now()).await.unwrap();
        let terminal = NewKey {
            id: "claude",
            access: Access::Write,
            grants: Grants::All,
            default_catalog: None,
            actor: "claude-code-tui",
        };
        let web = NewKey {
            actor: "claude-ai-web",
            ..terminal
        };

        let first = create_with_id(&db.pool, &terminal, now()).await.unwrap();
        let second = create_with_id(&db.pool, &web, now()).await.unwrap();
        let mut tx = db.pool.begin().await.unwrap();
        let old_changeset = crate::store::changesets::open(
            &mut tx,
            work_catalog.id,
            &first.id,
            terminal.actor,
            now(),
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        sqlx::query(
            "INSERT INTO revisions \
             (catalog_id, path, revision, op, content, executable, actor, changeset_id, \
              message, created_at) \
             VALUES ($1, 'owned.md', 1, 'create', $2, false, $3, $4, 'original publish', $5)",
        )
        .bind(work_catalog.id)
        .bind(b"# Owned\n".as_slice())
        .bind(terminal.actor)
        .bind(old_changeset.id)
        .bind(now())
        .execute(&db.pool)
        .await
        .unwrap();
        assert_ne!(first.id, "claude");
        assert_ne!(second.id, first.id);
        assert!(matches!(
            create(&db.pool, &terminal, now()).await,
            Err(OkfError::Refused { reason }) if reason.contains("already exists")
        ));
        assert!(matches!(
            revoke_target(&db.pool, "claude", None, now()).await,
            Err(OkfError::Refused { reason }) if reason.contains("ambiguous")
        ));
        assert!(matches!(
            grant_target(&db.pool, "claude", None, "work").await,
            Err(OkfError::Refused { reason }) if reason.contains("ambiguous")
        ));
        assert!(matches!(
            ungrant_target(&db.pool, "claude", None, "work").await,
            Err(OkfError::Refused { reason }) if reason.contains("ambiguous")
        ));

        let revoked_id = revoke_target(&db.pool, "claude", Some("claude-code-tui"), now())
            .await
            .unwrap();
        assert_eq!(revoked_id, first.id);
        let replacement = create_with_id(&db.pool, &terminal, now()).await.unwrap();
        assert_ne!(replacement.id, first.id);
        let historical = crate::store::changesets::list_open(&db.pool, &[work_catalog.id], None)
            .await
            .unwrap();
        let historical = historical
            .iter()
            .find(|row| row.changeset.handle == old_changeset.handle)
            .unwrap();
        assert_eq!(
            historical.changeset.key_id.as_deref(),
            Some(first.id.as_str())
        );
        assert_eq!(historical.key_name.as_deref(), Some("claude"));
        assert_eq!(historical.changeset.actor, terminal.actor);
        let revisions = crate::store::history::revisions(&db.pool, work_catalog.id, "owned.md", 20)
            .await
            .unwrap();
        assert_eq!(revisions.len(), 1);
        assert_eq!(revisions[0].actor, terminal.actor);
        assert_eq!(revisions[0].message, "original publish");

        let records = list(&db.pool).await.unwrap();
        assert_eq!(records.len(), 3);
        assert!(records.iter().all(|record| record.name == "claude"));
        assert_eq!(
            records
                .iter()
                .filter(|record| record.revoked_at.is_none())
                .count(),
            2
        );
        assert!(crate::keys::matches(
            &records
                .iter()
                .find(|record| record.id == replacement.id)
                .unwrap()
                .sha256,
            &replacement.key
        ));
        let cache = crate::auth::KeyCache::from_records(records);
        assert!(
            cache.find(&first.key).is_none(),
            "the revoked key is refused"
        );
        assert!(
            cache.find(&second.key).is_some(),
            "the other actor still works"
        );
        assert!(
            cache.find(&replacement.key).is_some(),
            "the reissued key works"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_exact_id_wins_over_a_colliding_group_name() {
        let db = test_db().await;
        catalogs::create(&db.pool, "work", "", now()).await.unwrap();
        let work = ["work".to_string()];
        let by_id = create_with_id(
            &db.pool,
            &NewKey {
                id: "first-name",
                access: Access::Write,
                grants: Grants::Catalogs(&work),
                default_catalog: None,
                actor: "first",
            },
            now(),
        )
        .await
        .unwrap();
        let collision = NewKey {
            id: &by_id.id,
            access: Access::Write,
            grants: Grants::Catalogs(&work),
            default_catalog: None,
            actor: "second",
        };
        create(&db.pool, &collision, now()).await.unwrap();

        assert!(matches!(
            grant_target(&db.pool, &by_id.id, Some("first"), "work").await,
            Err(OkfError::Refused { reason }) if reason.contains("cannot be used with key id")
        ));

        grant_target(&db.pool, &by_id.id, None, "work")
            .await
            .unwrap();
        ungrant_target(&db.pool, &by_id.id, None, "work")
            .await
            .unwrap();
        let revoked = revoke_target(&db.pool, &by_id.id, None, now())
            .await
            .unwrap();
        assert_eq!(revoked, by_id.id);
        assert!(list(&db.pool)
            .await
            .unwrap()
            .iter()
            .any(|record| record.name == collision.id && record.revoked_at.is_none()));
    }
}

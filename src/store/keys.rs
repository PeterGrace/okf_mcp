//! API key rows and their catalog grants. Only a key's SHA-256 is stored;
//! the key itself is returned once, by [`create`].

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgPool};

use crate::error::OkfError;
use crate::store::catalogs::{self, bump_admin_generation, is_unique_violation};
use crate::store::users;

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

/// What a key row is: an API key, or an OAuth grant (a key row with no
/// hash, reached through its tokens).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyKind {
    /// An `okf_` API key.
    Key,
    /// A grant made by redeeming a sign-in code.
    OAuth,
}

impl KeyKind {
    /// The spelling stored in `api_keys.kind`.
    ///
    /// # Returns
    /// `"key"` or `"oauth"`.
    pub fn as_str(self) -> &'static str {
        match self {
            KeyKind::Key => "key",
            KeyKind::OAuth => "oauth",
        }
    }

    /// Parses the stored spelling.
    ///
    /// # Arguments
    /// * `text` - an `api_keys.kind` value.
    ///
    /// # Returns
    /// The kind, or `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "key" => Some(KeyKind::Key),
            "oauth" => Some(KeyKind::OAuth),
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
    pub name: &'a str,
    /// Its access level.
    pub access: Access,
    /// Whether this key may submit maintenance reports.
    pub maintenance_report: bool,
    /// Its catalogs.
    pub grants: Grants<'a>,
    /// Its default catalog; must be granted.
    pub default_catalog: Option<&'a str>,
    /// The actor its writes are recorded as (`claude-code/claude-opus-5-5`).
    pub actor: &'a str,
    /// The user it belongs to (must exist).
    pub user: &'a str,
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
    /// The key's SHA-256; `None` for an OAuth grant, which has no secret of its own.
    pub sha256: Option<Vec<u8>>,
    /// Its access level.
    pub access: Access,
    /// Whether this key may submit maintenance reports.
    pub maintenance_report: bool,
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
    /// An API key or an OAuth grant.
    pub kind: KeyKind,
    /// A grant's OAuth client (`None` for a key, or once the client is gone).
    pub client_id: Option<String>,
    /// That client's display name, if it gave one.
    pub client_name: Option<String>,
    /// The user it belongs to; `None` for a row from before migration 0009
    /// that no admin has assigned yet.
    pub user: Option<String>,
}

/// The row as selected, before `access` is parsed.
#[derive(sqlx::FromRow)]
struct KeyRow {
    id: String,
    name: String,
    sha256: Option<Vec<u8>>,
    access: String,
    maintenance_report: bool,
    all_catalogs: bool,
    catalogs: Vec<String>,
    default_catalog: Option<String>,
    actor: String,
    created_at: DateTime<Utc>,
    revoked_at: Option<DateTime<Utc>>,
    kind: String,
    client_id: Option<String>,
    client_name: Option<String>,
    user: Option<String>,
}

/// Is `name` valid under the schema's `api_keys_name_check` constraint? Sign-in
/// codes use the same rule for their names (`store::oauth::codes`).
///
/// # Arguments
/// * `name` - a key or sign-in code name.
///
/// # Returns
/// `true` for a non-empty name of letters, digits, `.`, `_` and `-`.
pub(crate) fn is_valid_name(name: &str) -> bool {
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
/// * `new` - the key's name, access, grants, default catalog and actor.
/// * `now` - the creation time (server clock).
///
/// # Returns
/// The key itself: print it once; it is not stored and cannot be shown again.
///
/// # Errors
/// [`OkfError::Refused`] for an invalid or taken id, no catalogs, or a default
/// catalog that is not granted; [`OkfError::CatalogNotSelected`] for an unknown catalog
/// (granted or default); [`OkfError::UnknownName`] for an unknown user;
/// [`OkfError::Io`] if the OS random source fails; [`OkfError::Database`]
/// otherwise.
pub async fn create(
    pool: &PgPool,
    new: &NewKey<'_>,
    now: DateTime<Utc>,
) -> Result<String, OkfError> {
    Ok(create_with_id(pool, new, now).await?.key)
}

/// Creates a key and also returns its immutable generated id.
///
/// # Arguments
/// As [`create`].
///
/// # Returns
/// The key's id and the key itself (shown once).
///
/// # Errors
/// As [`create`].
pub async fn create_with_id(
    pool: &PgPool,
    new: &NewKey<'_>,
    now: DateTime<Utc>,
) -> Result<CreatedKey, OkfError> {
    if !is_valid_name(new.name) {
        return Err(OkfError::Refused {
            reason: format!(
                "key name {:?} must be letters, digits, '.', '_' and '-'",
                new.name
            ),
        });
    }
    let mut tx = pool.begin().await?;
    let (granted, default_catalog_id) =
        resolve_grants(&mut tx, "key", new.name, new.grants, new.default_catalog).await?;
    // Held `FOR KEY SHARE` until commit, so a concurrent `users::delete`
    // either waits and then counts this key, or wins and leaves an unknown name.
    let user_id = users::id_of(&mut tx, new.user).await?;
    let key = crate::keys::generate()?;
    let inserted = sqlx::query_scalar::<_, String>(
        "INSERT INTO api_keys \
         (name, sha256, access, maintenance_report, all_catalogs, default_catalog_id, actor, \
          created_at, user_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(new.name)
    .bind(crate::keys::hash(&key).as_slice())
    .bind(new.access.as_str())
    .bind(new.maintenance_report)
    .bind(matches!(new.grants, Grants::All))
    .bind(default_catalog_id)
    .bind(new.actor)
    .bind(now)
    .bind(&user_id)
    .fetch_one(&mut *tx)
    .await;
    // Only the live name/actor constraint is a user-facing duplicate. A
    // `sha256` UNIQUE violation (a credential-hash collision) remains a
    // database error rather than being mislabeled as a taken name.
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
                    new.name, new.actor
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

/// Inserts a test-only legacy row whose immutable id is also its name.
#[cfg(test)]
pub(crate) async fn insert_legacy_test_key(
    pool: &PgPool,
    id: &str,
    access: Access,
    actor: &str,
    now: DateTime<Utc>,
) -> String {
    let key = crate::keys::generate().expect("generate test key");
    sqlx::query(
        "INSERT INTO api_keys \
         (id, name, sha256, access, all_catalogs, actor, created_at) \
         VALUES ($1, $1, $2, $3, true, $4, $5)",
    )
    .bind(id)
    .bind(crate::keys::hash(&key).as_slice())
    .bind(access.as_str())
    .bind(actor)
    .bind(now)
    .execute(pool)
    .await
    .expect("insert legacy test key");
    key
}

/// The ids of the catalogs called `names`, in the same order, in one query.
/// Shared with the sign-in codes (`store::oauth::codes`).
///
/// # Arguments
/// * `conn` - a connection or open transaction.
/// * `names` - catalog names.
///
/// # Returns
/// Their ids, in `names`' order.
///
/// # Errors
/// [`OkfError::CatalogNotSelected`] for the first name with no catalog;
/// [`OkfError::Database`].
pub(crate) async fn catalog_ids(
    conn: &mut PgConnection,
    names: &[String],
) -> Result<Vec<i64>, OkfError> {
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

/// Checks a new credential's grants and default catalog, then looks up their
/// ids: the rules `okf-mcp key create` and `okf-mcp oauth code create` share.
///
/// # Arguments
/// * `conn` - the creating transaction's connection.
/// * `noun` - `"key"` or `"sign-in code"`, for the messages.
/// * `name` - the credential's name, for the messages.
/// * `grants` - the catalogs it reaches.
/// * `default_catalog` - its default catalog, if any.
///
/// # Returns
/// The granted catalog ids, in the caller's order (empty for [`Grants::All`]),
/// and the default catalog's id.
///
/// # Errors
/// [`OkfError::Refused`] for no catalogs or an ungranted default (checked
/// before any query); [`OkfError::CatalogNotSelected`] for an unknown catalog;
/// [`OkfError::Database`].
pub(crate) async fn resolve_grants(
    conn: &mut PgConnection,
    noun: &str,
    name: &str,
    grants: Grants<'_>,
    default_catalog: Option<&str>,
) -> Result<(Vec<i64>, Option<i64>), OkfError> {
    if matches!(grants, Grants::Catalogs([])) {
        // A slice pattern: `[]` matches an empty slice.
        return Err(OkfError::Refused {
            reason: format!("{noun} {name:?} must be granted a catalog, or every catalog"),
        });
    }
    // Where the default sits among the granted names, checked before any query.
    let default_position = match (default_catalog, grants) {
        (Some(default), Grants::Catalogs(names)) => Some(
            names
                .iter()
                .position(|granted| granted == default)
                .ok_or_else(|| OkfError::Refused {
                    reason: format!("default catalog {default:?} is not granted to the {noun}"),
                })?,
        ),
        (Some(_), Grants::All) | (None, _) => None,
    };
    let granted = match grants {
        Grants::All => Vec::new(),
        Grants::Catalogs(names) => catalog_ids(conn, names).await?,
    };
    let default_id = match (default_catalog, default_position) {
        // `catalog_ids` returns one id per name, so `position` is in range.
        (_, Some(position)) => Some(granted[position]),
        // An unknown catalog is `CatalogNotSelected`; any other error propagates.
        (Some(default), None) => Some(catalogs::find(&mut *conn, default).await?.id),
        (None, None) => None,
    };
    Ok((granted, default_id))
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
        "SELECT k.id, k.name, k.sha256, k.access, k.maintenance_report, k.all_catalogs, \
                d.name AS default_catalog, k.actor, k.created_at, k.revoked_at, \
                k.kind, k.client_id, oc.client_name, u.name AS user, \
                COALESCE(array_agg(c.name ORDER BY c.name) FILTER (WHERE c.name IS NOT NULL), \
                         '{}'::text[]) AS catalogs \
         FROM api_keys k \
         LEFT JOIN catalogs d ON d.id = k.default_catalog_id \
         LEFT JOIN oauth_clients oc ON oc.client_id = k.client_id \
         LEFT JOIN users u ON u.id = k.user_id \
         LEFT JOIN key_catalogs g ON g.key_id = k.id \
         LEFT JOIN catalogs c ON c.id = g.catalog_id \
         GROUP BY k.id, d.name, oc.client_name, u.name ORDER BY k.name, k.actor, k.created_at, k.id",
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
            maintenance_report: row.maintenance_report,
            all_catalogs: row.all_catalogs,
            catalogs: row.catalogs,
            default_catalog: row.default_catalog,
            actor: row.actor,
            created_at: row.created_at,
            revoked_at: row.revoked_at,
            kind: KeyKind::parse(&row.kind).expect("the schema checks api_keys.kind"),
            client_id: row.client_id,
            client_name: row.client_name,
            user: row.user,
        })
        .collect())
}

/// Sets the independent maintenance-report capability on an active key.
/// Resolves an exact immutable id before an unambiguous active name; an actor
/// disambiguates a name, not an id. An unchanged value does not reload caches.
///
/// # Errors
/// [`OkfError::Refused`] for a revoked, absent or ambiguous key;
/// [`OkfError::Database`] for storage failures.
pub async fn set_maintenance_report_target(
    pool: &PgPool,
    target: &str,
    actor: Option<&str>,
    enabled: bool,
) -> Result<String, OkfError> {
    set_maintenance_report_with(pool, Selector::Resolve { target, actor }, enabled).await
}

/// As [`set_maintenance_report_target`], but for exactly the key with this immutable id, never a
/// name: the row is locked in the change's own transaction, so a key
/// replaced since the caller looked it up cannot be changed instead. The
/// web API's key routes use this.
///
/// # Errors
/// As [`set_maintenance_report_target`]; [`OkfError::Refused`] if there is no key with this id.
pub async fn set_maintenance_report_by_id(
    pool: &PgPool,
    id: &str,
    enabled: bool,
) -> Result<String, OkfError> {
    set_maintenance_report_with(pool, Selector::Exact(id), enabled).await
}

/// The body of [`set_maintenance_report_target`] and [`set_maintenance_report_by_id`].
async fn set_maintenance_report_with(
    pool: &PgPool,
    selector: Selector<'_>,
    enabled: bool,
) -> Result<String, OkfError> {
    let mut tx = pool.begin().await?;
    let id = select(&mut tx, selector).await?;
    if enabled {
        // An OAuth grant never reports (the schema's `api_keys_oauth_caps`):
        // refuse it here as a 409 instead of letting the CHECK fail as a 500.
        let kind: Option<String> = sqlx::query_scalar("SELECT kind FROM api_keys WHERE id = $1")
            .bind(&id)
            .fetch_optional(&mut *tx)
            .await?;
        if kind.as_deref() == Some(KeyKind::OAuth.as_str()) {
            return Err(OkfError::Refused {
                reason: format!(
                    "key {id:?} is an OAuth grant, which cannot submit maintenance reports"
                ),
            });
        }
    }
    let updated = sqlx::query(
        "UPDATE api_keys SET maintenance_report = $2 \
         WHERE id = $1 AND revoked_at IS NULL AND maintenance_report IS DISTINCT FROM $2",
    )
    .bind(&id)
    .bind(enabled)
    .execute(&mut *tx)
    .await?;
    if updated.rows_affected() != 0 {
        bump_admin_generation(&mut tx).await?;
    } else {
        // `resolve_active` accepts an exact generated id even if revoked.
        let active: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM api_keys WHERE id = $1 AND revoked_at IS NULL)",
        )
        .bind(&id)
        .fetch_one(&mut *tx)
        .await?;
        if !active {
            return Err(OkfError::Refused {
                reason: format!("key {id:?} is revoked"),
            });
        }
    }
    tx.commit().await?;
    Ok(id)
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
    revoke_with(pool, Selector::Resolve { target, actor }, now).await
}

/// As [`revoke_target`], but for exactly the key with this immutable id, never a
/// name: the row is locked in the change's own transaction, so a key
/// replaced since the caller looked it up cannot be changed instead. The
/// web API's key routes use this.
///
/// # Errors
/// As [`revoke_target`]; [`OkfError::Refused`] if there is no key with this id.
pub async fn revoke_by_id(pool: &PgPool, id: &str, now: DateTime<Utc>) -> Result<String, OkfError> {
    revoke_with(pool, Selector::Exact(id), now).await
}

/// The body of [`revoke_target`] and [`revoke_by_id`].
async fn revoke_with(
    pool: &PgPool,
    selector: Selector<'_>,
    now: DateTime<Utc>,
) -> Result<String, OkfError> {
    let mut tx = pool.begin().await?;
    let id = select(&mut tx, selector).await?;
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
    grant_with(pool, Selector::Resolve { target, actor }, catalog).await
}

/// As [`grant_target`], but for exactly the key with this immutable id, never a
/// name: the row is locked in the change's own transaction, so a key
/// replaced since the caller looked it up cannot be changed instead. The
/// web API's key routes use this.
///
/// # Errors
/// As [`grant_target`]; [`OkfError::Refused`] if there is no key with this id.
pub async fn grant_by_id(pool: &PgPool, id: &str, catalog: &str) -> Result<String, OkfError> {
    grant_with(pool, Selector::Exact(id), catalog).await
}

/// The body of [`grant_target`] and [`grant_by_id`].
async fn grant_with(
    pool: &PgPool,
    selector: Selector<'_>,
    catalog: &str,
) -> Result<String, OkfError> {
    let mut tx = pool.begin().await?;
    let catalog = catalogs::find(&mut *tx, catalog).await?;
    let id = select(&mut tx, selector).await?;
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
    ungrant_with(pool, Selector::Resolve { target, actor }, catalog).await
}

/// As [`ungrant_target`], but for exactly the key with this immutable id, never a
/// name: the row is locked in the change's own transaction, so a key
/// replaced since the caller looked it up cannot be changed instead. The
/// web API's key routes use this.
///
/// # Errors
/// As [`ungrant_target`]; [`OkfError::Refused`] if there is no key with this id.
pub async fn ungrant_by_id(pool: &PgPool, id: &str, catalog: &str) -> Result<String, OkfError> {
    ungrant_with(pool, Selector::Exact(id), catalog).await
}

/// The body of [`ungrant_target`] and [`ungrant_by_id`].
async fn ungrant_with(
    pool: &PgPool,
    selector: Selector<'_>,
    catalog: &str,
) -> Result<String, OkfError> {
    let mut tx = pool.begin().await?;
    let catalog = catalogs::find(&mut *tx, catalog).await?;
    let id = select(&mut tx, selector).await?;
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

/// Gives a key or OAuth grant a user, revoked rows included (their history
/// is attributed too), and bumps the administrative generation. Its access
/// is unchanged.
///
/// # Arguments
/// * `pool` - the database.
/// * `id` - exactly the key's or grant's immutable id, never a name.
/// * `user` - the user it belongs to from now on.
///
/// # Returns
/// The key's or grant's name.
///
/// # Errors
/// [`OkfError::UnknownName`] for an unknown user; [`OkfError::NotFound`]
/// (`keys/<id>`) when no row has this id; [`OkfError::Database`].
pub async fn assign_user_by_id(pool: &PgPool, id: &str, user: &str) -> Result<String, OkfError> {
    let mut tx = pool.begin().await?;
    let name = set_user(&mut tx, id, user)
        .await?
        .ok_or_else(|| OkfError::NotFound {
            path: format!("keys/{id}"),
        })?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(name)
}

/// As [`assign_user_by_id`], for an `okf-mcp key assign` target: an exact
/// generated id, else the one active key or grant with that name (and actor,
/// if given). Unlike the other `key` commands, grants are included.
///
/// # Arguments
/// * `pool` - the database.
/// * `target` - the id or name.
/// * `actor` - narrows a name to one actor.
/// * `user` - the user it belongs to from now on.
///
/// # Returns
/// The id it resolved to.
///
/// # Errors
/// [`OkfError::Refused`] for an unknown, revoked (by name) or ambiguous
/// target, or an actor given with an id; [`OkfError::UnknownName`] for an
/// unknown user; [`OkfError::Database`].
pub async fn assign_user_target(
    pool: &PgPool,
    target: &str,
    actor: Option<&str>,
    user: &str,
) -> Result<String, OkfError> {
    let mut tx = pool.begin().await?;
    let id = select(&mut tx, Selector::ResolveAnyKind { target, actor }).await?;
    // `select` found the row in this transaction, but without a lock it can
    // be gone by the update; that is the same "no such key" answer.
    set_user(&mut tx, &id, user)
        .await?
        .ok_or_else(|| OkfError::Refused {
            reason: format!("no key {id:?}"),
        })?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(id)
}

/// Points the row `id` at `user`, inside the caller's transaction.
///
/// # Returns
/// The row's name, or `None` when there is no row with this id.
///
/// # Errors
/// [`OkfError::UnknownName`] for an unknown user; [`OkfError::Database`].
async fn set_user(
    conn: &mut PgConnection,
    id: &str,
    user: &str,
) -> Result<Option<String>, OkfError> {
    // The user is read `FOR KEY SHARE` (held until commit) before the update,
    // so a concurrent `users::delete` either waits and then counts this row,
    // or wins first and this is `UnknownName` rather than a foreign-key error.
    let user_id = users::id_of(conn, user).await?;
    Ok(
        sqlx::query_scalar("UPDATE api_keys SET user_id = $2 WHERE id = $1 RETURNING name")
            .bind(id)
            .bind(&user_id)
            .fetch_optional(&mut *conn)
            .await?,
    )
}

/// How a key change names its key.
#[derive(Debug, Clone, Copy)]
enum Selector<'a> {
    /// An exact generated id, else an unambiguous active name, of an API key
    /// only (the CLI's rules, [`resolve_active`]).
    Resolve {
        /// The id or name.
        target: &'a str,
        /// Narrows a name to one actor.
        actor: Option<&'a str>,
    },
    /// As `Resolve`, but OAuth grants count too (`key assign`, Decision 4).
    ResolveAnyKind {
        /// The id or name.
        target: &'a str,
        /// Narrows a name to one actor.
        actor: Option<&'a str>,
    },
    /// Exactly this id, whatever its shape, an OAuth grant's included.
    Exact(&'a str),
}

/// The id `selector` names, inside the change's transaction.
async fn select(conn: &mut PgConnection, selector: Selector<'_>) -> Result<String, OkfError> {
    match selector {
        Selector::Resolve { target, actor } => resolve_active(conn, target, actor, false).await,
        Selector::ResolveAnyKind { target, actor } => {
            resolve_active(conn, target, actor, true).await
        }
        Selector::Exact(id) => {
            // `FOR UPDATE` locks the row until the transaction ends, so a
            // concurrent revoke or grant of it waits for this change.
            let found: Option<String> =
                sqlx::query_scalar("SELECT id FROM api_keys WHERE id = $1 FOR UPDATE")
                    .bind(id)
                    .fetch_optional(&mut *conn)
                    .await?;
            found.ok_or_else(|| OkfError::Refused {
                reason: format!("no key {id:?}"),
            })
        }
    }
}

/// The id an `okf-mcp key ...` target names: an exact generated id, else the
/// one active key with that name (and actor, if given). Only API keys
/// (`kind = 'key'`) are considered, by id or by name: an OAuth grant may share
/// a key's name (the live name/actor index covers keys only), and a key
/// command must never act on, or be made ambiguous by, a grant. Grants are
/// revoked by exact id (`okf-mcp oauth grant revoke`, `/api/v1/keys`).
/// `include_grants` lifts that rule for `okf-mcp key assign`, which reaches
/// grants too; a grant sharing a key's name then makes the name ambiguous.
///
/// # Errors
/// [`OkfError::Refused`] for an unknown, revoked or ambiguous target, or an
/// actor given with an id; [`OkfError::Database`].
async fn resolve_active(
    conn: &mut PgConnection,
    target: &str,
    actor: Option<&str>,
    include_grants: bool,
) -> Result<String, OkfError> {
    let exact: Option<(String, String)> =
        sqlx::query_as("SELECT id, name FROM api_keys WHERE id = $1 AND ($2 OR kind = 'key')")
            .bind(target)
            .bind(include_grants)
            .fetch_optional(&mut *conn)
            .await?;
    if let Some((id, name)) = exact {
        // Migration 0005 is the boundary: older rows have `id == name`,
        // while every later id is generated by the database and callers can
        // set only `name`. Legacy fixtures intentionally insert `id == name`.
        // Those rows must use active-name resolution so a revoked credential
        // cannot shadow its replacement and multiple actors stay ambiguous.
        if id != name {
            if actor.is_none() {
                return Ok(id);
            }
            return Err(OkfError::Refused {
                reason: format!(
                    "--actor selects a key name and cannot be used with key id {target:?}"
                ),
            });
        }
    }
    let ids: Vec<String> = sqlx::query_scalar(
        "SELECT id FROM api_keys WHERE revoked_at IS NULL AND name = $1 \
         AND ($3 OR kind = 'key') AND ($2::text IS NULL OR actor = $2) \
         ORDER BY actor, id",
    )
    .bind(target)
    .bind(actor)
    .bind(include_grants)
    .fetch_all(&mut *conn)
    .await?;
    match ids.as_slice() {
        [id] => Ok(id.clone()),
        [] => {
            // Only diagnose revoked names after looking for live replacements.
            let revoked: bool = sqlx::query_scalar(
                "SELECT EXISTS(SELECT 1 FROM api_keys WHERE name = $1 \
                 AND ($3 OR kind = 'key') AND revoked_at IS NOT NULL \
                 AND ($2::text IS NULL OR actor = $2))",
            )
            .bind(target)
            .bind(actor)
            .bind(include_grants)
            .fetch_one(&mut *conn)
            .await?;
            Err(OkfError::Refused {
                reason: match (revoked, actor) {
                    (true, Some(actor)) => {
                        format!("key named {target:?} with actor {actor:?} is revoked")
                    }
                    (true, None) => format!("key {target:?} is revoked"),
                    (false, Some(actor)) => {
                        format!("no active key named {target:?} with actor {actor:?}")
                    }
                    (false, None) => format!("no active key or key id {target:?}"),
                },
            })
        }
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

    fn new_key<'a>(name: &'a str, grants: Grants<'a>, default: Option<&'a str>) -> NewKey<'a> {
        NewKey {
            name,
            access: Access::Write,
            maintenance_report: false,
            grants,
            default_catalog: default,
            actor: "claude-code/test",
            user: crate::db::TEST_USER,
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

    #[test]
    fn key_kind_round_trips_its_spelling() {
        for kind in [KeyKind::Key, KeyKind::OAuth] {
            assert_eq!(KeyKind::parse(kind.as_str()), Some(kind));
        }
        assert_eq!(KeyKind::parse("token"), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn list_shows_a_grant_with_its_kind_and_client_and_no_hash() {
        let db = test_db().await;
        catalogs::create(&db.pool, "main", "", crate::db::TEST_TEAM, now())
            .await
            .unwrap();
        let names = ["main".to_string()];
        let key = create(
            &db.pool,
            &new_key("laptop", Grants::Catalogs(&names), None),
            now(),
        )
        .await
        .unwrap();
        let client = crate::store::oauth::testing::insert_client(
            &db.pool,
            "https://client.example.com/client.json",
            "Example Client",
            now(),
        )
        .await;
        let grant = crate::store::oauth::testing::insert_grant(
            &db.pool,
            &client,
            Access::Read,
            &["main"],
            now(),
        )
        .await;

        let records = list(&db.pool).await.unwrap();

        let key_row = records
            .iter()
            .find(|record| record.name == "laptop" && record.kind == KeyKind::Key)
            .unwrap();
        assert!(crate::keys::matches(
            key_row.sha256.as_deref().unwrap(),
            &key
        ));
        assert_eq!(key_row.client_id, None);
        assert_eq!(key_row.client_name, None);
        let grant_row = records.iter().find(|record| record.id == grant).unwrap();
        assert_eq!(grant_row.kind, KeyKind::OAuth);
        assert_eq!(grant_row.sha256, None);
        assert_eq!(grant_row.client_id.as_deref(), Some(client.as_str()));
        assert_eq!(grant_row.client_name.as_deref(), Some("Example Client"));
        assert_eq!(grant_row.catalogs, ["main"]);
        assert_eq!(grant_row.access, Access::Read);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_stores_only_the_hash_and_the_grants() {
        let db = test_db().await;
        // Read first: the fixture user already bumped the generation.
        let start = catalogs::admin_generation(&db.pool).await.unwrap();
        catalogs::create(&db.pool, "pg", "", crate::db::TEST_TEAM, now())
            .await
            .unwrap();
        catalogs::create(&db.pool, "work", "", crate::db::TEST_TEAM, now())
            .await
            .unwrap();
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
        assert!(crate::keys::matches(
            record.sha256.as_deref().expect("a key row has a hash"),
            &key
        ));
        assert_eq!(record.kind, KeyKind::Key);
        assert_eq!(record.created_at, now());
        assert_eq!(record.revoked_at, None);
        assert_eq!(
            catalogs::admin_generation(&db.pool).await.unwrap(),
            start + 3
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn bad_requests_are_refused() {
        let db = test_db().await;
        // Read first: the fixture user already bumped the generation.
        let start = catalogs::admin_generation(&db.pool).await.unwrap();
        catalogs::create(&db.pool, "pg", "", crate::db::TEST_TEAM, now())
            .await
            .unwrap();
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
        assert_eq!(
            catalogs::admin_generation(&db.pool).await.unwrap(),
            start + 2
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoke_grant_and_ungrant() {
        let db = test_db().await;
        // Read first: the fixture user already bumped the generation.
        let start = catalogs::admin_generation(&db.pool).await.unwrap();
        catalogs::create(&db.pool, "pg", "", crate::db::TEST_TEAM, now())
            .await
            .unwrap();
        catalogs::create(&db.pool, "work", "", crate::db::TEST_TEAM, now())
            .await
            .unwrap();
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
        assert_eq!(
            catalogs::admin_generation(&db.pool).await.unwrap(),
            start + 7
        );
        assert!(matches!(
            revoke(&db.pool, "k", now()).await,
            Err(OkfError::Refused { .. })
        ));
        assert!(matches!(
            grant(&db.pool, "k", "pg").await,
            Err(OkfError::Refused { reason }) if reason.contains("is revoked")
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
        catalogs::create(&db.pool, "pg", "", crate::db::TEST_TEAM, now())
            .await
            .unwrap();
        create(&db.pool, &new_key("k", Grants::All, Some("pg")), now())
            .await
            .unwrap();
        let unknown_default =
            create(&db.pool, &new_key("j", Grants::All, Some("nope")), now()).await;

        catalogs::delete(&db.pool, "pg", false).await.unwrap();
        catalogs::create(&db.pool, "pg", "", crate::db::TEST_TEAM, now())
            .await
            .unwrap();

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
        let work_catalog = catalogs::create(&db.pool, "work", "", crate::db::TEST_TEAM, now())
            .await
            .unwrap();
        let terminal = NewKey {
            name: "claude",
            access: Access::Write,
            maintenance_report: false,
            grants: Grants::All,
            default_catalog: None,
            actor: "claude-code-tui",
            user: crate::db::TEST_USER,
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
            records
                .iter()
                .find(|record| record.id == replacement.id)
                .unwrap()
                .sha256
                .as_deref()
                .expect("a key row has a hash"),
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
        catalogs::create(&db.pool, "work", "", crate::db::TEST_TEAM, now())
            .await
            .unwrap();
        let work = ["work".to_string()];
        let by_id = create_with_id(
            &db.pool,
            &NewKey {
                name: "first-name",
                access: Access::Write,
                maintenance_report: false,
                grants: Grants::Catalogs(&work),
                default_catalog: None,
                actor: "first",
                user: crate::db::TEST_USER,
            },
            now(),
        )
        .await
        .unwrap();
        let collision = NewKey {
            name: &by_id.id,
            access: Access::Write,
            maintenance_report: false,
            grants: Grants::Catalogs(&work),
            default_catalog: None,
            actor: "second",
            user: crate::db::TEST_USER,
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
            .any(|record| record.name == collision.name && record.revoked_at.is_none()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn by_id_changes_never_reach_a_legacy_keys_replacement() {
        // The web API's interleaving: it reads a legacy key (id == name) and
        // checks its reach, another admin rotates that key, then the change
        // runs with the id it read. The rotation's replacement shares the name
        // and actor, so name resolution would pick it; by id nothing may.
        let db = test_db().await;
        for name in ["work", "extra"] {
            catalogs::create(&db.pool, name, "", crate::db::TEST_TEAM, now())
                .await
                .unwrap();
        }
        insert_legacy_test_key(&db.pool, "legacy", Access::Write, "terminal", now()).await;
        let snapshot = list(&db.pool)
            .await
            .unwrap()
            .into_iter()
            .find(|record| record.id == "legacy")
            .expect("the legacy key");

        revoke_by_id(&db.pool, &snapshot.id, now()).await.unwrap();
        let work = ["work".to_string()];
        let replacement = create_with_id(
            &db.pool,
            &NewKey {
                actor: "terminal",
                ..new_key("legacy", Grants::Catalogs(&work), None)
            },
            now(),
        )
        .await
        .unwrap();

        let refused = |result: Result<String, OkfError>| {
            assert!(
                matches!(result, Err(OkfError::Refused { .. })),
                "{result:?}"
            );
        };
        refused(revoke_by_id(&db.pool, &snapshot.id, now()).await);
        refused(set_maintenance_report_by_id(&db.pool, &snapshot.id, true).await);
        refused(grant_by_id(&db.pool, &snapshot.id, "extra").await);
        refused(ungrant_by_id(&db.pool, &snapshot.id, "work").await);
        refused(revoke_by_id(&db.pool, "no-such-id", now()).await);

        let after = list(&db.pool)
            .await
            .unwrap()
            .into_iter()
            .find(|record| record.id == replacement.id)
            .expect("the replacement");
        assert_eq!(after.revoked_at, None);
        assert!(!after.maintenance_report);
        assert_eq!(after.catalogs, vec!["work".to_string()]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_legacy_id_uses_active_name_resolution_after_rotation() {
        let db = test_db().await;
        catalogs::create(&db.pool, "work", "", crate::db::TEST_TEAM, now())
            .await
            .unwrap();
        insert_legacy_test_key(&db.pool, "legacy", Access::Write, "terminal", now()).await;

        let work = ["work".to_string()];
        let web = create_with_id(
            &db.pool,
            &NewKey {
                name: "legacy",
                access: Access::Write,
                maintenance_report: false,
                grants: Grants::Catalogs(&work),
                default_catalog: None,
                actor: "web",
                user: crate::db::TEST_USER,
            },
            now(),
        )
        .await
        .unwrap();
        assert!(matches!(
            revoke_target(&db.pool, "legacy", None, now()).await,
            Err(OkfError::Refused { reason }) if reason.contains("ambiguous")
        ));

        revoke_target(&db.pool, &web.id, None, now()).await.unwrap();
        revoke_target(&db.pool, "legacy", Some("terminal"), now())
            .await
            .unwrap();
        for actor in [None, Some("terminal")] {
            assert!(matches!(
                revoke_target(&db.pool, "legacy", actor, now()).await,
                Err(OkfError::Refused { reason }) if reason.contains("is revoked")
            ));
        }
        assert!(matches!(
            revoke_target(&db.pool, "legacy", Some("unknown"), now()).await,
            Err(OkfError::Refused { reason }) if reason.contains("no active key")
        ));
        let replacement = create_with_id(
            &db.pool,
            &NewKey {
                name: "legacy",
                access: Access::Write,
                maintenance_report: false,
                grants: Grants::Catalogs(&work),
                default_catalog: None,
                actor: "terminal",
                user: crate::db::TEST_USER,
            },
            now(),
        )
        .await
        .unwrap();

        assert_eq!(
            ungrant_target(&db.pool, "legacy", None, "work")
                .await
                .unwrap(),
            replacement.id
        );
        assert_eq!(
            grant_target(&db.pool, "legacy", None, "work")
                .await
                .unwrap(),
            replacement.id
        );
        assert_eq!(
            revoke_target(&db.pool, "legacy", None, now())
                .await
                .unwrap(),
            replacement.id
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn key_commands_by_name_act_on_the_key_and_never_on_a_grant_of_that_name() {
        let db = test_db().await;
        catalogs::create(&db.pool, "main", "", crate::db::TEST_TEAM, now())
            .await
            .unwrap();
        catalogs::create(&db.pool, "other", "", crate::db::TEST_TEAM, now())
            .await
            .unwrap();
        let client = crate::store::oauth::testing::insert_client(
            &db.pool,
            "https://client.example.com/client.json",
            "Example Client",
            now(),
        )
        .await;
        // `insert_grant` names its grant `laptop`, actor `example-actor`.
        let grant = crate::store::oauth::testing::insert_grant(
            &db.pool,
            &client,
            Access::Read,
            &["main"],
            now(),
        )
        .await;
        let main = ["main".to_string()];
        let same_name = NewKey {
            actor: "example-actor",
            ..new_key("laptop", Grants::Catalogs(&main), None)
        };
        // The live name/actor rule covers keys only: the grant does not collide.
        let key = create_with_id(&db.pool, &same_name, now())
            .await
            .unwrap()
            .id;
        let row = |id: String| {
            let pool = db.pool.clone();
            async move {
                list(&pool)
                    .await
                    .unwrap()
                    .into_iter()
                    .find(|record| record.id == id)
                    .expect("the row exists")
            }
        };

        // Each by-name command finds exactly one candidate (not "ambiguous").
        let reporting = set_maintenance_report_target(&db.pool, "laptop", None, true).await;
        let granted = grant_target(&db.pool, "laptop", Some("example-actor"), "other").await;
        let ungranted = ungrant_target(&db.pool, "laptop", None, "main").await;
        let revoked = revoke_target(&db.pool, "laptop", None, now()).await;

        assert_eq!(reporting.unwrap(), key);
        assert_eq!(granted.unwrap(), key);
        assert_eq!(ungranted.unwrap(), key);
        assert_eq!(revoked.unwrap(), key);
        let key_row = row(key.clone()).await;
        assert!(key_row.maintenance_report && key_row.revoked_at.is_some());
        assert_eq!(key_row.catalogs, ["other"]);
        let grant_row = row(grant.clone()).await;
        assert_eq!(grant_row.kind, KeyKind::OAuth);
        assert!(!grant_row.maintenance_report);
        assert_eq!(grant_row.catalogs, ["main"]);
        assert_eq!(grant_row.revoked_at, None, "the grant is untouched");
        // With the key revoked, the name reports the key, not the live grant.
        assert!(matches!(
            revoke_target(&db.pool, "laptop", None, now()).await,
            Err(OkfError::Refused { reason }) if reason.contains("is revoked")
        ));
        // A grant's id is no key id to `okf-mcp key ...`.
        for result in [
            revoke_target(&db.pool, &grant, None, now()).await,
            grant_target(&db.pool, &grant, None, "other").await,
            set_maintenance_report_target(&db.pool, &grant, None, false).await,
        ] {
            assert!(
                matches!(&result, Err(OkfError::Refused { reason }) if reason.contains("no active key")),
                "{result:?}"
            );
        }
        assert_eq!(row(grant).await.revoked_at, None);
    }

    /// The stored record for `id`.
    async fn record(pool: &PgPool, id: &str) -> KeyRecord {
        list(pool)
            .await
            .unwrap()
            .into_iter()
            .find(|record| record.id == id)
            .expect("the row exists")
    }

    /// Creates user `alice` (with her personal team).
    async fn alice(pool: &PgPool) {
        let new = users::NewUser {
            name: "alice",
            display_name: "",
        };
        users::create(pool, &new, now()).await.unwrap();
    }

    /// Opens a transaction that deletes `alice` (her personal team first)
    /// and holds its commit, as a concurrent `users::delete` would.
    async fn deleting_alice(pool: &PgPool) -> sqlx::Transaction<'static, sqlx::Postgres> {
        let mut tx = pool.begin().await.unwrap();
        sqlx::query(
            "DELETE FROM teams WHERE personal_of = (SELECT id FROM users WHERE name = 'alice')",
        )
        .execute(&mut *tx)
        .await
        .unwrap();
        sqlx::query("DELETE FROM users WHERE name = 'alice'")
            .execute(&mut *tx)
            .await
            .unwrap();
        tx
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_key_belongs_to_its_user() {
        let db = test_db().await;
        let created = create_with_id(&db.pool, &new_key("k", Grants::All, None), now())
            .await
            .unwrap();

        let found = record(&db.pool, &created.id).await;

        assert_eq!(found.user.as_deref(), Some(crate::db::TEST_USER));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_key_for_an_unknown_user_is_unknown_name() {
        let db = test_db().await;
        let new = NewKey {
            user: "nobody",
            ..new_key("k", Grants::All, None)
        };
        let generation = catalogs::admin_generation(&db.pool).await.unwrap();

        let made = create_with_id(&db.pool, &new, now()).await;

        assert!(
            matches!(&made, Err(OkfError::UnknownName { noun: "user", name }) if name == "nobody"),
            "{made:?}"
        );
        assert!(list(&db.pool).await.unwrap().is_empty());
        assert_eq!(
            catalogs::admin_generation(&db.pool).await.unwrap(),
            generation
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn assigning_a_legacy_key_sets_its_user_and_bumps_the_generation() {
        let db = test_db().await;
        insert_legacy_test_key(&db.pool, "legacy", Access::Read, "legacy", now()).await;
        assert_eq!(record(&db.pool, "legacy").await.user, None);
        let before = catalogs::admin_generation(&db.pool).await.unwrap();

        let name = assign_user_by_id(&db.pool, "legacy", crate::db::TEST_USER)
            .await
            .unwrap();

        assert_eq!(name, "legacy");
        let found = record(&db.pool, "legacy").await;
        assert_eq!(found.user.as_deref(), Some(crate::db::TEST_USER));
        assert!(catalogs::admin_generation(&db.pool).await.unwrap() > before);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn assigning_reassigns_a_revoked_key_and_refuses_unknown_names() {
        // Arrange: a revoked key belonging to the fixture user, and alice.
        let db = test_db().await;
        alice(&db.pool).await;
        let created = create_with_id(&db.pool, &new_key("k", Grants::All, None), now())
            .await
            .unwrap();
        revoke_by_id(&db.pool, &created.id, now()).await.unwrap();

        let moved = assign_user_by_id(&db.pool, &created.id, "alice").await;
        let unknown_user = assign_user_by_id(&db.pool, &created.id, "nobody").await;
        let unknown_key = assign_user_by_id(&db.pool, "no-such-id", "alice").await;

        assert_eq!(moved.unwrap(), "k", "revoked rows are attributed too");
        assert!(
            matches!(
                &unknown_user,
                Err(OkfError::UnknownName { noun: "user", .. })
            ),
            "{unknown_user:?}"
        );
        assert!(
            matches!(&unknown_key, Err(OkfError::NotFound { path }) if path == "keys/no-such-id"),
            "{unknown_key:?}"
        );
        assert_eq!(
            record(&db.pool, &created.id).await.user.as_deref(),
            Some("alice")
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn assign_by_name_reaches_grants_and_a_shared_name_is_ambiguous() {
        // Arrange: a grant named `laptop`, then a key of the same name.
        let db = test_db().await;
        catalogs::create(&db.pool, "main", "", crate::db::TEST_TEAM, now())
            .await
            .unwrap();
        let client = crate::store::oauth::testing::insert_client(
            &db.pool,
            "https://client.example.com/client.json",
            "Example Client",
            now(),
        )
        .await;
        let grant = crate::store::oauth::testing::insert_grant(
            &db.pool,
            &client,
            Access::Read,
            &["main"],
            now(),
        )
        .await;

        let by_name = assign_user_target(&db.pool, "laptop", None, crate::db::TEST_USER).await;
        assert_eq!(by_name.unwrap(), grant);
        assert_eq!(
            record(&db.pool, &grant).await.user.as_deref(),
            Some(crate::db::TEST_USER)
        );
        let key = create_with_id(&db.pool, &new_key("laptop", Grants::All, None), now())
            .await
            .unwrap()
            .id;
        let ambiguous = assign_user_target(&db.pool, "laptop", None, crate::db::TEST_USER).await;
        let by_actor = assign_user_target(
            &db.pool,
            "laptop",
            Some("claude-code/test"),
            crate::db::TEST_USER,
        )
        .await;

        assert!(
            matches!(&ambiguous, Err(OkfError::Refused { reason }) if reason.contains("ambiguous")),
            "{ambiguous:?}"
        );
        assert_eq!(by_actor.unwrap(), key);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_create_that_waited_on_a_user_delete_is_unknown_name() {
        // Arrange: a transaction deletes alice and holds its commit while the
        // create runs.
        let db = test_db().await;
        alice(&db.pool).await;
        let deleting = deleting_alice(&db.pool).await;
        // `tokio::spawn` runs the create concurrently, like a thread; its
        // `FOR KEY SHARE` read of the user waits on the delete's row lock.
        let pool = db.pool.clone();
        let creating = tokio::spawn(async move {
            let new = NewKey {
                user: "alice",
                ..new_key("k", Grants::All, None)
            };
            create_with_id(&pool, &new, now()).await
        });
        crate::store::catalogs::tests::until_a_lock_is_awaited(&db.pool).await;

        deleting.commit().await.unwrap();
        let outcome = creating.await.unwrap();

        // Not a foreign-key violation (a 500): the user is simply unknown.
        assert!(
            matches!(&outcome, Err(OkfError::UnknownName { noun: "user", .. })),
            "{outcome:?}"
        );
        assert!(list(&db.pool).await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_assign_that_waited_on_a_user_delete_is_unknown_name() {
        let db = test_db().await;
        alice(&db.pool).await;
        insert_legacy_test_key(&db.pool, "legacy", Access::Read, "legacy", now()).await;
        let deleting = deleting_alice(&db.pool).await;
        let pool = db.pool.clone();
        let assigning =
            tokio::spawn(async move { assign_user_by_id(&pool, "legacy", "alice").await });
        crate::store::catalogs::tests::until_a_lock_is_awaited(&db.pool).await;

        deleting.commit().await.unwrap();
        let outcome = assigning.await.unwrap();

        assert!(
            matches!(&outcome, Err(OkfError::UnknownName { noun: "user", .. })),
            "{outcome:?}"
        );
        assert_eq!(record(&db.pool, "legacy").await.user, None);
    }
}

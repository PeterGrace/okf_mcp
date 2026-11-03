//! Changesets and their entries (design "Storage", "Session lifecycle"):
//! one open changeset per (catalog, key, label) holds a connection's
//! unpublished edits until `publish` or `discard` closes it.
//!
//! # Lock order (Decision 3)
//!
//! Every transaction that writes a changeset or its entries takes:
//! 1. the catalog row, `FOR KEY SHARE` ([`lock_catalog`]), first;
//! 2. the changeset row (the `UPDATE` that bumps `version`);
//! 3. the entry rows.
//!
//! `catalogs::delete` takes the catalog row `FOR UPDATE` before the open
//! changesets, so a write and a delete wait for each other in the same order
//! and cannot deadlock: a write that waited sees the catalog gone. Every
//! function here that writes takes its caller's transaction, after
//! [`lock_catalog`]; only [`prune_empty`] runs on its own, without the
//! catalog lock, skipping rows another transaction holds (`SKIP LOCKED`), so
//! it never waits for anyone. A deadlock Postgres still detects is
//! [`OkfError::Busy`], through `From<sqlx::Error>`.

use chrono::{DateTime, Utc};
use sqlx::{PgConnection, PgExecutor, PgPool};

use crate::error::OkfError;
use crate::store::catalogs::is_unique_violation;

/// What [`lock_catalog`] reports when the catalog row is gone: it was
/// deleted while the write waited for the delete to finish.
pub const CATALOG_GONE: &str =
    "the catalog was deleted while this request waited; nothing was written";

/// How often [`get_or_create`] tries again when the changeset it found
/// closes before it can read it back.
const GET_OR_CREATE_TRIES: usize = 3;

/// The columns of a changeset row, in [`RawChangeset`]'s order.
// A macro rather than a `const`: `concat!` (below) joins string literals at
// compile time, and it expands a macro call among its arguments into one,
// while it cannot read a `const`. So every query stays one literal string.
macro_rules! changeset_columns {
    () => {
        "id, catalog_id, label, key_id, actor, state, version, created_at, updated_at"
    };
}

/// Where a changeset is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChangesetState {
    /// Receiving edits.
    Open,
    /// Its entries became documents and revisions.
    Published,
    /// Dropped by `discard` or by the prune.
    Discarded,
}

impl ChangesetState {
    /// The spelling stored in `changesets.state`.
    ///
    /// # Returns
    /// `"open"`, `"published"` or `"discarded"`.
    pub fn as_str(self) -> &'static str {
        match self {
            ChangesetState::Open => "open",
            ChangesetState::Published => "published",
            ChangesetState::Discarded => "discarded",
        }
    }

    /// Parses the stored spelling.
    ///
    /// # Arguments
    /// * `text` - a `changesets.state` value.
    ///
    /// # Returns
    /// The state, or `None` for anything [`ChangesetState::as_str`] does not
    /// produce.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "open" => Some(ChangesetState::Open),
            "published" => Some(ChangesetState::Published),
            "discarded" => Some(ChangesetState::Discarded),
            _ => None,
        }
    }
}

/// A changeset row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChangesetRow {
    /// Database id (what `adopt_changeset` takes).
    pub id: i64,
    /// Its catalog.
    pub catalog_id: i64,
    /// The connection's label (Decision 1).
    pub label: String,
    /// The key it belongs to; `None` once that key's row is deleted.
    pub key_id: Option<String>,
    /// The actor of the key that opened it.
    pub actor: String,
    /// Open, published or discarded.
    pub state: ChangesetState,
    /// Bumped by every change to it or its entries.
    pub version: i64,
    /// When it was opened (server clock).
    pub created_at: DateTime<Utc>,
    /// Its last change (server clock).
    pub updated_at: DateTime<Utc>,
}

/// One changed path of a changeset.
// `FromRow`: the columns map onto the fields by name. `replay` is `jsonb`,
// which sqlx's `json` feature decodes into `serde_json::Value`.
#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct EntryRow {
    /// Catalog-relative path.
    pub path: String,
    /// The published revision the edit was based on; `None` when the path
    /// had no live document.
    pub base_revision: Option<i32>,
    /// The new bytes; `None` deletes the path.
    pub content: Option<Vec<u8>>,
    /// The new file's execute bit.
    pub executable: bool,
    /// The replay operations (Task 3's `ReplayOp`s as JSON) while every
    /// change to this entry came from one; `None` otherwise.
    pub replay: Option<serde_json::Value>,
}

/// The identity of an open changeset (Decision 1).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ChangesetKey<'a> {
    /// The catalog.
    pub catalog_id: i64,
    /// The key's id.
    pub key_id: &'a str,
    /// The connection's label.
    pub label: &'a str,
}

/// An open, non-empty changeset idle past a cutoff (`stranded_sessions`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StrandedRow {
    /// The changeset.
    pub changeset: ChangesetRow,
    /// Its catalog's name.
    pub catalog: String,
    /// Its changed paths, sorted.
    pub paths: Vec<String>,
}

/// A changeset row as selected, before `state` is parsed.
#[derive(sqlx::FromRow)]
struct RawChangeset {
    id: i64,
    catalog_id: i64,
    label: String,
    key_id: Option<String>,
    actor: String,
    state: String,
    version: i64,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
}

impl From<RawChangeset> for ChangesetRow {
    fn from(raw: RawChangeset) -> Self {
        Self {
            id: raw.id,
            catalog_id: raw.catalog_id,
            label: raw.label,
            key_id: raw.key_id,
            actor: raw.actor,
            // The column's CHECK constraint allows only the three spellings.
            state: ChangesetState::parse(&raw.state).expect("the schema checks changesets.state"),
            version: raw.version,
            created_at: raw.created_at,
            updated_at: raw.updated_at,
        }
    }
}

/// A [`StrandedRow`] as selected.
#[derive(sqlx::FromRow)]
struct RawStranded {
    // `flatten`: the changeset's columns sit beside `catalog` and `paths` in
    // the same result row.
    #[sqlx(flatten)]
    changeset: RawChangeset,
    catalog: String,
    paths: Vec<String>,
}

/// The error for a changeset that stopped being open under a write.
fn no_longer_open(changeset_id: i64) -> OkfError {
    OkfError::Busy {
        reason: format!("changeset {changeset_id} was published or discarded meanwhile"),
    }
}

/// Takes the catalog row `FOR KEY SHARE` for the rest of the transaction:
/// the first lock of every write to changesets or entries (Decision 3).
///
/// `FOR KEY SHARE` lets other writers and publishes (whose `UPDATE` of
/// `generation` takes `FOR NO KEY UPDATE`) through, and holds off only
/// `catalogs::delete`'s `FOR UPDATE`.
///
/// # Arguments
/// * `conn` - the transaction's connection (pass `&mut tx`).
/// * `catalog_id` - the catalog.
///
/// # Errors
/// [`OkfError::Refused`] with [`CATALOG_GONE`] if the row is gone (a delete
/// committed while this waited); [`OkfError::Database`].
pub async fn lock_catalog(conn: &mut PgConnection, catalog_id: i64) -> Result<(), OkfError> {
    let locked: Option<i64> =
        sqlx::query_scalar("SELECT id FROM catalogs WHERE id = $1 FOR KEY SHARE")
            .bind(catalog_id)
            .fetch_optional(&mut *conn)
            .await?;
    if locked.is_none() {
        return Err(OkfError::Refused {
            reason: CATALOG_GONE.to_string(),
        });
    }
    Ok(())
}

/// The open changeset of an identity.
///
/// # Arguments
/// * `executor` - a pool, connection or transaction (`&mut *tx`).
/// * `key` - catalog, key and label.
///
/// # Returns
/// The changeset, or `None` if that identity has none open. One lookup in
/// the partial unique index.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn find_open<'e>(
    executor: impl PgExecutor<'e>,
    key: &ChangesetKey<'_>,
) -> Result<Option<ChangesetRow>, OkfError> {
    let row = sqlx::query_as::<_, RawChangeset>(concat!(
        "SELECT ",
        changeset_columns!(),
        " FROM changesets WHERE catalog_id = $1 AND key_id = $2 AND label = $3 \
         AND state = 'open'"
    ))
    .bind(key.catalog_id)
    .bind(key.key_id)
    .bind(key.label)
    .fetch_optional(executor)
    .await?;
    // `Option::map` with a `From` function: `Some(raw)` becomes `Some(row)`.
    Ok(row.map(ChangesetRow::from))
}

/// The open changeset of an identity, opening one if there is none.
///
/// The insert skips a conflicting open row (`ON CONFLICT DO NOTHING`, which
/// waits for a racing insert to commit or roll back), then reads the row
/// that won `FOR UPDATE`. Either way the transaction holds the changeset's
/// row lock until it ends. Call it after [`lock_catalog`], in the write's
/// transaction.
///
/// # Arguments
/// * `conn` - the transaction's connection.
/// * `key` - catalog, key and label.
/// * `actor` - the key's actor, recorded on a new changeset.
/// * `now` - the time (server clock), for a new changeset's timestamps.
///
/// # Returns
/// The open changeset.
///
/// # Errors
/// [`OkfError::Busy`] if the changeset kept closing between the insert and
/// the read; [`OkfError::Database`].
pub async fn get_or_create(
    conn: &mut PgConnection,
    key: &ChangesetKey<'_>,
    actor: &str,
    now: DateTime<Utc>,
) -> Result<ChangesetRow, OkfError> {
    for _ in 0..GET_OR_CREATE_TRIES {
        let inserted = sqlx::query_as::<_, RawChangeset>(concat!(
            "INSERT INTO changesets (catalog_id, key_id, label, actor, state, created_at, \
             updated_at) VALUES ($1, $2, $3, $4, 'open', $5, $5) \
             ON CONFLICT (catalog_id, key_id, label) WHERE state = 'open' DO NOTHING \
             RETURNING ",
            changeset_columns!()
        ))
        .bind(key.catalog_id)
        .bind(key.key_id)
        .bind(key.label)
        .bind(actor)
        .bind(now)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(row) = inserted {
            return Ok(row.into());
        }
        // Read committed: this statement sees the row that won the race.
        // `FOR UPDATE` takes that row's lock too, as the insert above does
        // for a new row, so from here on the write holds its changeset and
        // the prune (`SKIP LOCKED`) passes it by.
        let found = sqlx::query_as::<_, RawChangeset>(concat!(
            "SELECT ",
            changeset_columns!(),
            " FROM changesets WHERE catalog_id = $1 AND key_id = $2 AND label = $3 \
             AND state = 'open' FOR UPDATE"
        ))
        .bind(key.catalog_id)
        .bind(key.key_id)
        .bind(key.label)
        .fetch_optional(&mut *conn)
        .await?;
        if let Some(row) = found {
            return Ok(row.into());
        }
    }
    Err(OkfError::Busy {
        reason: "the changeset closed while this write opened it".to_string(),
    })
}

/// A changeset by id, in any state.
///
/// # Arguments
/// * `executor` - a pool, connection or transaction.
/// * `id` - the changeset's id.
///
/// # Returns
/// The row, or `None` if there is none.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn by_id<'e>(
    executor: impl PgExecutor<'e>,
    id: i64,
) -> Result<Option<ChangesetRow>, OkfError> {
    let row = sqlx::query_as::<_, RawChangeset>(concat!(
        "SELECT ",
        changeset_columns!(),
        " FROM changesets WHERE id = $1"
    ))
    .bind(id)
    .fetch_optional(executor)
    .await?;
    Ok(row.map(ChangesetRow::from))
}

/// A changeset's entries.
///
/// # Arguments
/// * `executor` - a pool, connection or transaction.
/// * `changeset_id` - the changeset.
///
/// # Returns
/// The entries, by path (the primary key's order, so no sort).
///
/// # Errors
/// [`OkfError::Database`].
pub async fn entries<'e>(
    executor: impl PgExecutor<'e>,
    changeset_id: i64,
) -> Result<Vec<EntryRow>, OkfError> {
    let rows = sqlx::query_as::<_, EntryRow>(
        "SELECT path, base_revision, content, executable, replay FROM changeset_entries \
         WHERE changeset_id = $1 ORDER BY path",
    )
    .bind(changeset_id)
    .fetch_all(executor)
    .await?;
    Ok(rows)
}

/// Bumps an open changeset's `version` and `updated_at`, taking its row
/// lock: step 2 of the lock order, before any entry is written.
///
/// # Errors
/// [`OkfError::Busy`] if it is not open (any more); [`OkfError::Database`].
async fn touch(
    conn: &mut PgConnection,
    changeset_id: i64,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    let touched = sqlx::query(
        "UPDATE changesets SET version = version + 1, updated_at = $2 \
         WHERE id = $1 AND state = 'open'",
    )
    .bind(changeset_id)
    .bind(now)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if touched == 0 {
        return Err(no_longer_open(changeset_id));
    }
    Ok(())
}

/// Stores an entry, replacing the changeset's entry for the same path, and
/// bumps the changeset's `version`.
///
/// # Arguments
/// * `conn` - the transaction's connection, after [`lock_catalog`].
/// * `changeset_id` - an open changeset.
/// * `entry` - the path, base revision, content, execute bit and replay.
/// * `now` - the time (server clock).
///
/// # Errors
/// [`OkfError::Busy`] if the changeset is no longer open (a concurrent
/// publish, discard or prune closed it; a retry opens a new one);
/// [`OkfError::Database`].
pub async fn upsert_entry(
    conn: &mut PgConnection,
    changeset_id: i64,
    entry: &EntryRow,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    touch(conn, changeset_id, now).await?;
    sqlx::query(
        "INSERT INTO changeset_entries \
         (changeset_id, path, base_revision, content, executable, replay) \
         VALUES ($1, $2, $3, $4, $5, $6) \
         ON CONFLICT (changeset_id, path) DO UPDATE SET \
         base_revision = EXCLUDED.base_revision, content = EXCLUDED.content, \
         executable = EXCLUDED.executable, replay = EXCLUDED.replay",
    )
    .bind(changeset_id)
    .bind(&entry.path)
    .bind(entry.base_revision)
    // `as_deref`: `&Option<Vec<u8>>` to `Option<&[u8]>`, binding without a copy.
    .bind(entry.content.as_deref())
    .bind(entry.executable)
    .bind(&entry.replay)
    .execute(&mut *conn)
    .await?;
    Ok(())
}

/// Drops entries and bumps the changeset's `version`.
///
/// # Arguments
/// * `conn` - the transaction's connection, after [`lock_catalog`].
/// * `changeset_id` - an open changeset.
/// * `paths` - the paths to drop; `None` drops every entry.
/// * `now` - the time (server clock).
///
/// # Returns
/// How many entries were dropped (a named path with no entry counts 0).
///
/// # Errors
/// [`OkfError::Busy`] if the changeset is no longer open;
/// [`OkfError::Database`].
pub async fn remove_entries(
    conn: &mut PgConnection,
    changeset_id: i64,
    paths: Option<&[String]>,
    now: DateTime<Utc>,
) -> Result<u64, OkfError> {
    touch(conn, changeset_id, now).await?;
    // `$2::text[] IS NULL`: a NULL array (`None`) matches every path.
    let removed = sqlx::query(
        "DELETE FROM changeset_entries WHERE changeset_id = $1 \
         AND ($2::text[] IS NULL OR path = ANY($2))",
    )
    .bind(changeset_id)
    .bind(paths)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    Ok(removed)
}

/// Closes an open changeset (publish or discard), bumping its `version`.
///
/// # Arguments
/// * `conn` - the transaction's connection, after [`lock_catalog`].
/// * `changeset_id` - the changeset.
/// * `state` - the new state.
/// * `now` - the time (server clock).
///
/// # Errors
/// [`OkfError::Refused`] if it is not open; [`OkfError::Database`].
pub async fn set_state(
    conn: &mut PgConnection,
    changeset_id: i64,
    state: ChangesetState,
    now: DateTime<Utc>,
) -> Result<(), OkfError> {
    let changed = sqlx::query(
        "UPDATE changesets SET state = $2, version = version + 1, updated_at = $3 \
         WHERE id = $1 AND state = 'open'",
    )
    .bind(changeset_id)
    .bind(state.as_str())
    .bind(now)
    .execute(&mut *conn)
    .await?
    .rows_affected();
    if changed == 0 {
        return Err(OkfError::Refused {
            reason: format!("changeset {changeset_id} is not open"),
        });
    }
    Ok(())
}

/// Every open changeset with entries whose last change is before
/// `idle_since`, across catalogs: what `stranded_sessions` filters by key.
///
/// # Arguments
/// * `pool` - the database.
/// * `idle_since` - the cutoff (now less `session_idle_timeout`).
///
/// # Returns
/// The changesets with their catalog's name and sorted paths, by catalog
/// name, then oldest change first.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn stranded(
    pool: &PgPool,
    idle_since: DateTime<Utc>,
) -> Result<Vec<StrandedRow>, OkfError> {
    // The inner join to the entries keeps only non-empty changesets;
    // grouping by the primary key `c.id` lets the other `c` columns be
    // selected without listing them in `GROUP BY`.
    let rows = sqlx::query_as::<_, RawStranded>(
        "SELECT c.id, c.catalog_id, c.label, c.key_id, c.actor, c.state, c.version, \
         c.created_at, c.updated_at, k.name AS catalog, \
         array_agg(e.path ORDER BY e.path) AS paths \
         FROM changesets c \
         JOIN catalogs k ON k.id = c.catalog_id \
         JOIN changeset_entries e ON e.changeset_id = c.id \
         WHERE c.state = 'open' AND c.updated_at < $1 \
         GROUP BY c.id, k.name \
         ORDER BY k.name, c.updated_at, c.id",
    )
    .bind(idle_since)
    .fetch_all(pool)
    .await?;
    Ok(rows
        .into_iter()
        .map(|raw| StrandedRow {
            changeset: raw.changeset.into(),
            catalog: raw.catalog,
            paths: raw.paths,
        })
        .collect())
}

/// Gives an open changeset a new key and label (`adopt_changeset`); the
/// caller has checked the calling key may.
///
/// # Arguments
/// * `conn` - the transaction's connection, after [`lock_catalog`].
/// * `id` - the changeset.
/// * `key_id` - the adopting key.
/// * `label` - the adopting connection's label.
/// * `now` - the time (server clock).
///
/// # Returns
/// The changeset as adopted (its `version` bumped).
///
/// # Errors
/// [`OkfError::Refused`] if it is not open, or if that key and label already
/// have an open changeset in the catalog (the transaction is then aborted:
/// drop it); [`OkfError::Database`].
pub async fn adopt(
    conn: &mut PgConnection,
    id: i64,
    key_id: &str,
    label: &str,
    now: DateTime<Utc>,
) -> Result<ChangesetRow, OkfError> {
    let adopted = sqlx::query_as::<_, RawChangeset>(concat!(
        "UPDATE changesets SET key_id = $2, label = $3, version = version + 1, \
         updated_at = $4 WHERE id = $1 AND state = 'open' RETURNING ",
        changeset_columns!()
    ))
    .bind(id)
    .bind(key_id)
    .bind(label)
    .bind(now)
    .fetch_optional(&mut *conn)
    .await;
    match adopted {
        Ok(Some(row)) => Ok(row.into()),
        Ok(None) => Err(OkfError::Refused {
            reason: format!("changeset {id} is not open"),
        }),
        Err(error) if is_unique_violation(&error) => Err(OkfError::Refused {
            reason: "this connection already has an open changeset in that catalog; publish \
                     or discard it first, or connect with another X-OKF-Changeset label"
                .to_string(),
        }),
        Err(error) => Err(error.into()),
    }
}

/// Discards every open changeset without entries whose last change is
/// before `older_than` (design "Session lifecycle": the 24-hour prune).
///
/// Changesets another transaction holds are skipped (`SKIP LOCKED`): a
/// writer holds its changeset's row from [`get_or_create`] on (the insert
/// locks a new row, the fallback read locks an existing one `FOR UPDATE`),
/// so a changeset is never discarded under a write, and the prune never
/// waits on a lock.
///
/// # Arguments
/// * `pool` - the database.
/// * `older_than` - the cutoff.
/// * `now` - the time (server clock), as their `updated_at`.
///
/// # Returns
/// How many were discarded.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn prune_empty(
    pool: &PgPool,
    older_than: DateTime<Utc>,
    now: DateTime<Utc>,
) -> Result<u64, OkfError> {
    let pruned = sqlx::query(
        "UPDATE changesets SET state = 'discarded', version = version + 1, updated_at = $2 \
         WHERE state = 'open' AND id IN (\
             SELECT c.id FROM changesets c \
             WHERE c.state = 'open' AND c.updated_at < $1 \
             AND NOT EXISTS (SELECT 1 FROM changeset_entries e WHERE e.changeset_id = c.id) \
             FOR UPDATE OF c SKIP LOCKED)",
    )
    .bind(older_than)
    .bind(now)
    .execute(pool)
    .await?
    .rows_affected();
    Ok(pruned)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::test_db;
    use crate::store::catalogs;
    use crate::store::catalogs::tests::until_a_lock_is_awaited;
    use crate::store::keys::{self as key_rows, Access, Grants, NewKey};
    use chrono::{TimeDelta, TimeZone};
    use std::time::Duration;

    /// Generous: the tests share a loaded machine.
    const PATIENCE: Duration = Duration::from_secs(30);

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 3, 12, 0, 0).unwrap()
    }

    /// Creates catalog `pg` and the keys `alice` and `bob` (the foreign key
    /// `changesets.key_id` needs them), returning the catalog's id.
    async fn setup(pool: &PgPool) -> i64 {
        let pg = catalogs::create(pool, "pg", "", now()).await.unwrap();
        for id in ["alice", "bob"] {
            let new = NewKey {
                id,
                access: Access::Write,
                grants: Grants::All,
                default_catalog: None,
                actor: id,
            };
            key_rows::create(pool, &new, now()).await.unwrap();
        }
        pg.id
    }

    fn key<'a>(catalog_id: i64, key_id: &'a str, label: &'a str) -> ChangesetKey<'a> {
        ChangesetKey {
            catalog_id,
            key_id,
            label,
        }
    }

    /// Gets or creates the changeset in its own transaction, in the lock order.
    async fn open(pool: &PgPool, key: &ChangesetKey<'_>, at: DateTime<Utc>) -> ChangesetRow {
        let mut tx = pool.begin().await.unwrap();
        lock_catalog(&mut tx, key.catalog_id).await.unwrap();
        let changeset = get_or_create(&mut tx, key, key.key_id, at).await.unwrap();
        tx.commit().await.unwrap();
        changeset
    }

    fn entry(path: &str, text: Option<&str>) -> EntryRow {
        EntryRow {
            path: path.to_string(),
            base_revision: None,
            content: text.map(|text| text.as_bytes().to_vec()),
            executable: false,
            replay: None,
        }
    }

    /// Stores one entry in its own transaction, in the lock order.
    async fn put(
        pool: &PgPool,
        changeset: &ChangesetRow,
        entry: &EntryRow,
        at: DateTime<Utc>,
    ) -> Result<(), OkfError> {
        let mut tx = pool.begin().await?;
        lock_catalog(&mut tx, changeset.catalog_id).await?;
        upsert_entry(&mut tx, changeset.id, entry, at).await?;
        tx.commit().await?;
        Ok(())
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn get_or_create_returns_the_open_changeset_it_made() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        let alice = key(pg, "alice", "default");

        let first = open(&db.pool, &alice, now()).await;
        let again = open(&db.pool, &alice, now() + TimeDelta::minutes(5)).await;
        let found = find_open(&db.pool, &alice).await.unwrap();

        assert_eq!(again, first);
        assert_eq!(found, Some(first.clone()));
        assert_eq!(
            (
                first.label.as_str(),
                first.key_id.as_deref(),
                first.actor.as_str()
            ),
            ("default", Some("alice"), "alice")
        );
        assert_eq!((first.state, first.version), (ChangesetState::Open, 0));
        assert_eq!((first.created_at, first.updated_at), (now(), now()));
        assert_eq!(by_id(&db.pool, first.id).await.unwrap(), Some(first));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn each_label_and_each_key_get_their_own_changeset() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;

        let alice = open(&db.pool, &key(pg, "alice", "default"), now()).await;
        let alice_drafts = open(&db.pool, &key(pg, "alice", "drafts"), now()).await;
        let bob = open(&db.pool, &key(pg, "bob", "default"), now()).await;

        assert_ne!(alice.id, alice_drafts.id);
        assert_ne!(alice.id, bob.id);
        assert_eq!(bob.key_id.as_deref(), Some("bob"));
        assert_eq!(
            find_open(&db.pool, &key(pg, "bob", "drafts"))
                .await
                .unwrap(),
            None
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn upsert_replaces_the_entry_and_bumps_the_version() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        let changeset = open(&db.pool, &key(pg, "alice", "default"), now()).await;
        let later = now() + TimeDelta::minutes(1);
        let mut replayed = entry("log.md", Some("# Log\n"));
        replayed.replay = Some(serde_json::json!([{"op": "log", "date": "2026-11-03"}]));

        put(&db.pool, &changeset, &entry("a.md", Some("one\n")), now())
            .await
            .unwrap();
        put(&db.pool, &changeset, &entry("a.md", Some("two\n")), later)
            .await
            .unwrap();
        put(&db.pool, &changeset, &replayed, later).await.unwrap();

        let stored = entries(&db.pool, changeset.id).await.unwrap();
        assert_eq!(stored, [entry("a.md", Some("two\n")), replayed]);
        let after = by_id(&db.pool, changeset.id).await.unwrap().unwrap();
        assert_eq!((after.version, after.updated_at), (3, later));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn remove_entries_drops_the_named_paths_or_all_of_them() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        let changeset = open(&db.pool, &key(pg, "alice", "default"), now()).await;
        for path in ["a.md", "b.md", "c.md"] {
            put(&db.pool, &changeset, &entry(path, Some("x\n")), now())
                .await
                .unwrap();
        }
        let named = ["b.md".to_string(), "nope.md".to_string()];

        let mut tx = db.pool.begin().await.unwrap();
        lock_catalog(&mut tx, pg).await.unwrap();
        let one = remove_entries(&mut tx, changeset.id, Some(&named), now())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let left: Vec<String> = entries(&db.pool, changeset.id)
            .await
            .unwrap()
            .into_iter()
            .map(|entry| entry.path)
            .collect();
        let mut tx = db.pool.begin().await.unwrap();
        lock_catalog(&mut tx, pg).await.unwrap();
        let rest = remove_entries(&mut tx, changeset.id, None, now())
            .await
            .unwrap();
        tx.commit().await.unwrap();

        assert_eq!(one, 1);
        assert_eq!(left, ["a.md", "c.md"]);
        assert_eq!(rest, 2);
        assert!(entries(&db.pool, changeset.id).await.unwrap().is_empty());
        // Three puts and two removals.
        assert_eq!(
            by_id(&db.pool, changeset.id)
                .await
                .unwrap()
                .unwrap()
                .version,
            5
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn set_state_closes_a_changeset_and_a_new_one_opens_after_it() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        let alice = key(pg, "alice", "default");
        let changeset = open(&db.pool, &alice, now()).await;

        let mut tx = db.pool.begin().await.unwrap();
        lock_catalog(&mut tx, pg).await.unwrap();
        set_state(&mut tx, changeset.id, ChangesetState::Published, now())
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let mut tx = db.pool.begin().await.unwrap();
        let twice = set_state(&mut tx, changeset.id, ChangesetState::Discarded, now()).await;
        drop(tx);
        let late_write = put(&db.pool, &changeset, &entry("a.md", Some("x\n")), now()).await;
        let next = open(&db.pool, &alice, now()).await;

        let closed = by_id(&db.pool, changeset.id).await.unwrap().unwrap();
        assert_eq!(closed.state, ChangesetState::Published);
        assert!(matches!(twice, Err(OkfError::Refused { .. })));
        assert!(matches!(late_write, Err(OkfError::Busy { .. })));
        assert_ne!(next.id, changeset.id);
        assert_eq!(find_open(&db.pool, &alice).await.unwrap(), Some(next));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stranded_lists_only_open_non_empty_idle_changesets() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        let old = now() - TimeDelta::hours(3);
        let idle = open(&db.pool, &key(pg, "alice", "idle"), old).await;
        put(&db.pool, &idle, &entry("b.md", Some("b\n")), old)
            .await
            .unwrap();
        put(&db.pool, &idle, &entry("a.md", None), old)
            .await
            .unwrap();
        open(&db.pool, &key(pg, "alice", "empty"), old).await;
        let fresh = open(&db.pool, &key(pg, "alice", "fresh"), now()).await;
        put(&db.pool, &fresh, &entry("a.md", Some("a\n")), now())
            .await
            .unwrap();
        let published = open(&db.pool, &key(pg, "bob", "done"), old).await;
        put(&db.pool, &published, &entry("a.md", Some("a\n")), old)
            .await
            .unwrap();
        let mut tx = db.pool.begin().await.unwrap();
        lock_catalog(&mut tx, pg).await.unwrap();
        set_state(&mut tx, published.id, ChangesetState::Published, old)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let rows = stranded(&db.pool, now() - TimeDelta::hours(2))
            .await
            .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].changeset.id, idle.id);
        assert_eq!(rows[0].catalog, "pg");
        assert_eq!(rows[0].paths, ["a.md", "b.md"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn adopt_relabels_and_refuses_a_label_that_already_has_one() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        let stranded_one = open(&db.pool, &key(pg, "alice", "old-session"), now()).await;
        let taken = open(&db.pool, &key(pg, "bob", "default"), now()).await;
        let later = now() + TimeDelta::minutes(1);

        let mut tx = db.pool.begin().await.unwrap();
        lock_catalog(&mut tx, pg).await.unwrap();
        let refused = adopt(&mut tx, stranded_one.id, "bob", "default", later).await;
        drop(tx);
        let mut tx = db.pool.begin().await.unwrap();
        lock_catalog(&mut tx, pg).await.unwrap();
        let adopted = adopt(&mut tx, stranded_one.id, "bob", "work", later)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        assert!(matches!(refused, Err(OkfError::Refused { reason }) if reason.contains("already")));
        assert_eq!(adopted.id, stranded_one.id);
        assert_eq!(
            (adopted.key_id.as_deref(), adopted.label.as_str()),
            (Some("bob"), "work")
        );
        assert_eq!((adopted.version, adopted.updated_at), (1, later));
        assert_eq!(
            find_open(&db.pool, &key(pg, "alice", "old-session"))
                .await
                .unwrap(),
            None
        );
        assert_eq!(
            find_open(&db.pool, &key(pg, "bob", "default"))
                .await
                .unwrap(),
            Some(taken)
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prune_discards_only_empty_changesets_idle_past_the_cutoff() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        let old = now() - TimeDelta::hours(25);
        let empty_old = open(&db.pool, &key(pg, "alice", "a"), old).await;
        let empty_new = open(&db.pool, &key(pg, "alice", "b"), now()).await;
        let full_old = open(&db.pool, &key(pg, "alice", "c"), old).await;
        put(&db.pool, &full_old, &entry("a.md", Some("a\n")), old)
            .await
            .unwrap();

        let pruned = prune_empty(&db.pool, now() - TimeDelta::hours(24), now())
            .await
            .unwrap();

        let state = |row: Option<ChangesetRow>| row.unwrap().state;
        assert_eq!(pruned, 1);
        assert_eq!(
            state(by_id(&db.pool, empty_old.id).await.unwrap()),
            ChangesetState::Discarded
        );
        assert_eq!(
            state(by_id(&db.pool, empty_new.id).await.unwrap()),
            ChangesetState::Open
        );
        assert_eq!(
            state(by_id(&db.pool, full_old.id).await.unwrap()),
            ChangesetState::Open
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_prune_passes_by_a_changeset_get_or_create_found_and_holds() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        let alice = key(pg, "alice", "default");
        let existing = open(&db.pool, &alice, now()).await;
        let mut writer = db.pool.begin().await.unwrap();
        lock_catalog(&mut writer, pg).await.unwrap();
        // The changeset exists, so this takes the conflict path: the
        // `FOR UPDATE` read, which holds the row until `writer` ends.
        let found = get_or_create(&mut writer, &alice, "alice", now())
            .await
            .unwrap();
        let cutoff = now() + TimeDelta::hours(1);

        // `SKIP LOCKED`: the prune neither waits for `writer` nor discards
        // the changeset it holds, though it is empty and idle past the cutoff.
        let while_held = tokio::time::timeout(PATIENCE, prune_empty(&db.pool, cutoff, now()))
            .await
            .expect("the prune must skip the held row, not wait for it")
            .unwrap();
        let state_while_held = by_id(&db.pool, existing.id).await.unwrap().unwrap().state;
        writer.rollback().await.unwrap();
        let after = prune_empty(&db.pool, cutoff, now()).await.unwrap();

        assert_eq!(found.id, existing.id);
        assert_eq!((while_held, state_while_held), (0, ChangesetState::Open));
        assert_eq!(after, 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_deadlock_reported_by_postgres_is_busy() {
        let db = test_db().await;

        // `RAISE ... USING ERRCODE` makes Postgres fail with that SQLSTATE,
        // as if it had picked this statement as a deadlock victim.
        let error: OkfError =
            sqlx::query("DO $$ BEGIN RAISE EXCEPTION 'victim' USING ERRCODE = '40P01'; END $$")
                .execute(&db.pool)
                .await
                .unwrap_err()
                .into();

        assert_eq!(error.kind(), "Busy");
        assert!(!error.is_internal());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_delete_waits_for_a_write_then_refuses_its_work() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        // `oneshot` channels: one message each way, so the test decides when
        // the writer commits.
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel::<()>();
        let (commit_tx, commit_rx) = tokio::sync::oneshot::channel::<()>();
        let pool = db.pool.clone();
        let writer = tokio::spawn(async move {
            let mut tx = pool.begin().await?;
            lock_catalog(&mut tx, pg).await?;
            let changeset =
                get_or_create(&mut tx, &key(pg, "alice", "default"), "alice", now()).await?;
            upsert_entry(&mut tx, changeset.id, &entry("a.md", Some("x\n")), now()).await?;
            ready_tx.send(()).expect("the test waits");
            commit_rx.await.expect("the test signals");
            tx.commit().await?;
            Ok::<(), OkfError>(())
        });
        ready_rx.await.unwrap();
        let pool = db.pool.clone();
        let deleting = tokio::spawn(async move { catalogs::delete(&pool, "pg", false).await });
        until_a_lock_is_awaited(&db.pool).await;

        commit_tx.send(()).unwrap();
        let written = tokio::time::timeout(PATIENCE, writer).await;
        let deleted = tokio::time::timeout(PATIENCE, deleting).await;

        written.expect("the write finishes").unwrap().unwrap();
        let deleted = deleted.expect("the delete finishes").unwrap();
        assert!(matches!(deleted, Err(OkfError::Refused { reason }) if reason.contains("--force")));
        assert_eq!(catalogs::list(&db.pool).await.unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_write_waits_for_a_delete_then_sees_the_catalog_gone() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        // The first two steps of `catalogs::delete`, held open: the catalog
        // row `FOR UPDATE`, then its open changesets.
        let mut deleter = db.pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM catalogs WHERE id = $1 FOR UPDATE")
            .bind(pg)
            .execute(&mut *deleter)
            .await
            .unwrap();
        let pool = db.pool.clone();
        let writer = tokio::spawn(async move {
            let mut tx = pool.begin().await?;
            lock_catalog(&mut tx, pg).await?;
            get_or_create(&mut tx, &key(pg, "alice", "default"), "alice", now()).await?;
            tx.commit().await?;
            Ok::<(), OkfError>(())
        });
        until_a_lock_is_awaited(&db.pool).await;

        sqlx::query("DELETE FROM catalogs WHERE id = $1")
            .bind(pg)
            .execute(&mut *deleter)
            .await
            .unwrap();
        deleter.commit().await.unwrap();
        let written = tokio::time::timeout(PATIENCE, writer)
            .await
            .expect("the write finishes")
            .unwrap();

        assert!(matches!(written, Err(OkfError::Refused { reason }) if reason == CATALOG_GONE));
        let left: i64 = sqlx::query_scalar("SELECT count(*) FROM changesets")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert_eq!(left, 0);
    }
}

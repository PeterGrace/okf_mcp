//! Changesets and their entries (design "Storage", "Session lifecycle"):
//! a changeset, named by the handle [`open`] draws (M8 Decision 1), holds
//! one key's unpublished edits in one catalog until `publish` or `discard`
//! closes it. A key may have several open at once.
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

/// The columns of a changeset row, in [`RawChangeset`]'s order.
// A macro rather than a `const`: `concat!` (below) joins string literals at
// compile time, and it expands a macro call among its arguments into one,
// while it cannot read a `const`. So every query stays one literal string.
macro_rules! changeset_columns {
    () => {
        "id, handle, catalog_id, label, key_id, actor, state, version, created_at, updated_at"
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
    /// The handle the server issued (M8 Decision 1): a random UUID in text
    /// form, what `open_changeset` returns.
    pub handle: String,
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
    handle: String,
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
            handle: raw.handle,
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

/// Opens a new changeset for `key_id` in a catalog (M8 Decision 1). Several
/// may be open at once for one key and catalog. Until the `label` column goes
/// (M8 Task 5), the label is set to the handle, so the old one-open-per-label
/// index can never refuse it.
///
/// # Arguments
/// * `conn` - the transaction's connection, after [`lock_catalog`].
/// * `catalog_id` - the catalog.
/// * `key_id` - the opening key.
/// * `actor` - its actor.
/// * `now` - the time (server clock), for its timestamps.
///
/// # Returns
/// The new, open, empty changeset.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn open(
    conn: &mut PgConnection,
    catalog_id: i64,
    key_id: &str,
    actor: &str,
    now: DateTime<Utc>,
) -> Result<ChangesetRow, OkfError> {
    // One statement: the CTE (a named subquery, `WITH drawn AS ...`) draws the
    // handle once, and the insert writes it to both columns.
    let row = sqlx::query_as::<_, RawChangeset>(concat!(
        "WITH drawn AS (SELECT gen_random_uuid()::text AS handle) \
         INSERT INTO changesets (catalog_id, key_id, label, handle, actor, state, \
         created_at, updated_at) \
         SELECT $1, $2, drawn.handle, drawn.handle, $3, 'open', $4, $4 FROM drawn \
         RETURNING ",
        changeset_columns!()
    ))
    .bind(catalog_id)
    .bind(key_id)
    .bind(actor)
    .bind(now)
    .fetch_one(&mut *conn)
    .await?;
    Ok(row.into())
}

/// A changeset by its handle, in any state.
///
/// # Arguments
/// * `executor` - a pool, connection or transaction.
/// * `handle` - the handle, as a client sent it.
///
/// # Returns
/// The row, or `None` if no changeset has that handle. One lookup in the
/// handle's unique index.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn by_handle<'e>(
    executor: impl PgExecutor<'e>,
    handle: &str,
) -> Result<Option<ChangesetRow>, OkfError> {
    let row = sqlx::query_as::<_, RawChangeset>(concat!(
        "SELECT ",
        changeset_columns!(),
        " FROM changesets WHERE handle = $1"
    ))
    .bind(handle)
    .fetch_optional(executor)
    .await?;
    Ok(row.map(ChangesetRow::from))
}

/// Locks one changeset's row for the rest of the transaction, then reads it
/// again.
///
/// Every transaction that changes a changeset's entries or state takes this
/// lock after [`lock_catalog`], so writes to one changeset, its publish and
/// its discard run one at a time. The row is read *after* the lock is
/// granted: under Postgres' default isolation (read committed) each statement
/// sees everything committed before it started, so the `version` and
/// `state` returned are those the previous holder committed.
///
/// # Arguments
/// * `conn` - the transaction's connection (pass `&mut tx`).
/// * `id` - the changeset's id.
///
/// # Returns
/// The row as it is now, or `None` if it no longer exists (its catalog was
/// deleted).
///
/// # Errors
/// [`OkfError::Database`] (a deadlock maps to [`OkfError::Busy`], as for
/// every query here).
pub async fn lock(conn: &mut PgConnection, id: i64) -> Result<Option<ChangesetRow>, OkfError> {
    // `fetch_optional` gives `None` when no row matched, so a vanished
    // changeset is not an error here; the caller decides.
    let locked: Option<i64> =
        sqlx::query_scalar("SELECT id FROM changesets WHERE id = $1 FOR UPDATE")
            .bind(id)
            .fetch_optional(&mut *conn)
            .await?;
    if locked.is_none() {
        return Ok(None);
    }
    // `&mut *conn` reborrows the connection for one more query.
    by_id(&mut *conn, id).await
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
/// [`OkfError::Busy`] if the changeset is no longer open (callers take
/// [`lock`] first and refuse a closed one, so this is a backstop);
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
/// `idle_since`, optionally only one key's or one catalog's: what
/// `stranded_sessions` filters further by reach and connection.
///
/// # Arguments
/// * `pool` - the database.
/// * `idle_since` - the cutoff (now less `session_idle_timeout`).
/// * `key_id` - only this key's changesets (`None`: every key's, for an
///   `admin` caller), so other keys' paths never leave the database.
/// * `catalog_id` - only this catalog's (`None`: every catalog's).
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
    key_id: Option<&str>,
    catalog_id: Option<i64>,
) -> Result<Vec<StrandedRow>, OkfError> {
    // The inner join to the entries keeps only non-empty changesets;
    // grouping by the primary key `c.id` lets the other `c` columns be
    // selected without listing them in `GROUP BY`. A `NULL` parameter
    // (a Rust `None`) turns its filter off.
    let rows = sqlx::query_as::<_, RawStranded>(
        "SELECT c.id, c.handle, c.catalog_id, c.label, c.key_id, c.actor, c.state, c.version, \
         c.created_at, c.updated_at, k.name AS catalog, \
         array_agg(e.path ORDER BY e.path) AS paths \
         FROM changesets c \
         JOIN catalogs k ON k.id = c.catalog_id \
         JOIN changeset_entries e ON e.changeset_id = c.id \
         WHERE c.state = 'open' AND c.updated_at < $1 \
         AND ($2::text IS NULL OR c.key_id = $2) \
         AND ($3::bigint IS NULL OR c.catalog_id = $3) \
         GROUP BY c.id, k.name \
         ORDER BY k.name, c.updated_at, c.id",
    )
    .bind(idle_since)
    .bind(key_id)
    .bind(catalog_id)
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
/// Changesets another transaction holds are skipped (`SKIP LOCKED`), so the
/// prune never waits on a lock, and it never discards a changeset under a
/// write. A write resolves its handle without a lock, then takes the row
/// with [`lock`] and reads its state again:
/// - if the write took the row first, the prune skips it, and the write's
///   entry (and `updated_at`) keep it from being pruned later;
/// - if the prune holds the row, [`lock`] waits for it to commit, then reads
///   `discarded`, and the write is refused ("was published or discarded
///   while this write waited"), storing nothing.
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

    /// Opens a changeset for `key_id` in its own transaction, in the lock order.
    async fn opened(
        pool: &PgPool,
        catalog_id: i64,
        key_id: &str,
        at: DateTime<Utc>,
    ) -> ChangesetRow {
        let mut tx = pool.begin().await.unwrap();
        lock_catalog(&mut tx, catalog_id).await.unwrap();
        let changeset = open(&mut tx, catalog_id, key_id, key_id, at).await.unwrap();
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
    async fn open_returns_a_fresh_handle_each_time_and_by_handle_finds_it() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        let mut tx = db.pool.begin().await.unwrap();
        lock_catalog(&mut tx, pg).await.unwrap();

        let first = open(&mut tx, pg, "alice", "agent:a", now()).await.unwrap();
        let second = open(&mut tx, pg, "alice", "agent:a", now()).await.unwrap();
        tx.commit().await.unwrap();

        assert_ne!(first.handle, second.handle);
        assert_ne!(first.id, second.id);
        assert_eq!(first.label, first.handle);
        assert_eq!(
            (first.state, first.key_id.as_deref(), first.actor.as_str()),
            (ChangesetState::Open, Some("alice"), "agent:a")
        );
        assert_eq!(
            by_handle(&db.pool, &first.handle).await.unwrap(),
            Some(first)
        );
        assert_eq!(by_handle(&db.pool, "no-such-handle").await.unwrap(), None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn upsert_replaces_the_entry_and_bumps_the_version() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        let changeset = opened(&db.pool, pg, "alice", now()).await;
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
        let changeset = opened(&db.pool, pg, "alice", now()).await;
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
    async fn set_state_closes_a_changeset_once_and_refuses_a_late_write() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        let changeset = opened(&db.pool, pg, "alice", now()).await;

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

        let closed = by_id(&db.pool, changeset.id).await.unwrap().unwrap();
        assert_eq!(closed.state, ChangesetState::Published);
        assert!(matches!(twice, Err(OkfError::Refused { .. })));
        assert!(matches!(late_write, Err(OkfError::Busy { .. })));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn stranded_lists_only_open_non_empty_idle_changesets() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        let old = now() - TimeDelta::hours(3);
        let idle = opened(&db.pool, pg, "alice", old).await;
        put(&db.pool, &idle, &entry("b.md", Some("b\n")), old)
            .await
            .unwrap();
        put(&db.pool, &idle, &entry("a.md", None), old)
            .await
            .unwrap();
        opened(&db.pool, pg, "alice", old).await;
        let fresh = opened(&db.pool, pg, "alice", now()).await;
        put(&db.pool, &fresh, &entry("a.md", Some("a\n")), now())
            .await
            .unwrap();
        let published = opened(&db.pool, pg, "bob", old).await;
        put(&db.pool, &published, &entry("a.md", Some("a\n")), old)
            .await
            .unwrap();
        let mut tx = db.pool.begin().await.unwrap();
        lock_catalog(&mut tx, pg).await.unwrap();
        set_state(&mut tx, published.id, ChangesetState::Published, old)
            .await
            .unwrap();
        tx.commit().await.unwrap();

        let cutoff = now() - TimeDelta::hours(2);
        let rows = stranded(&db.pool, cutoff, None, None).await.unwrap();
        let owner = idle.key_id.as_deref();
        let by_owner = stranded(&db.pool, cutoff, owner, Some(pg)).await.unwrap();
        let by_other_key = stranded(&db.pool, cutoff, Some("nobody"), None)
            .await
            .unwrap();
        let by_other_catalog = stranded(&db.pool, cutoff, None, Some(pg + 1))
            .await
            .unwrap();

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].changeset.id, idle.id);
        assert_eq!(rows[0].catalog, "pg");
        assert_eq!(rows[0].paths, ["a.md", "b.md"]);
        assert_eq!(by_owner, rows);
        assert!(by_other_key.is_empty());
        assert!(by_other_catalog.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn adopt_relabels_and_refuses_a_label_that_already_has_one() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        let stranded_one = opened(&db.pool, pg, "alice", now()).await;
        let taken = opened(&db.pool, pg, "bob", now()).await;
        let later = now() + TimeDelta::minutes(1);

        let mut tx = db.pool.begin().await.unwrap();
        lock_catalog(&mut tx, pg).await.unwrap();
        // `taken`'s label: bob already has an open changeset with it.
        let refused = adopt(&mut tx, stranded_one.id, "bob", &taken.label, later).await;
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
            by_id(&db.pool, stranded_one.id).await.unwrap(),
            Some(adopted)
        );
        assert_eq!(by_id(&db.pool, taken.id).await.unwrap(), Some(taken));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prune_discards_only_empty_changesets_idle_past_the_cutoff() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        let old = now() - TimeDelta::hours(25);
        let empty_old = opened(&db.pool, pg, "alice", old).await;
        let empty_new = opened(&db.pool, pg, "alice", now()).await;
        let full_old = opened(&db.pool, pg, "alice", old).await;
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
    async fn the_prune_passes_by_a_changeset_a_write_holds() {
        let db = test_db().await;
        let pg = setup(&db.pool).await;
        let existing = opened(&db.pool, pg, "alice", now()).await;
        let mut writer = db.pool.begin().await.unwrap();
        lock_catalog(&mut writer, pg).await.unwrap();
        // As a write takes it: the row stays locked until `writer` ends.
        let found = lock(&mut writer, existing.id)
            .await
            .unwrap()
            .expect("the changeset exists");
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
            let changeset = open(&mut tx, pg, "alice", "alice", now()).await?;
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
            open(&mut tx, pg, "alice", "alice", now()).await?;
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

    #[tokio::test(flavor = "multi_thread")]
    async fn lock_waits_for_the_holder_and_reads_its_commit() {
        let db = test_db().await;
        let catalog = catalogs::create(&db.pool, "pg", "", now()).await.unwrap();
        // `changesets.key_id` is a foreign key: the key needs a row.
        let new_key = NewKey {
            id: "writer",
            access: Access::Write,
            grants: Grants::All,
            default_catalog: None,
            actor: "writer",
        };
        key_rows::create(&db.pool, &new_key, now()).await.unwrap();
        let row = opened(&db.pool, catalog.id, "writer", now()).await;
        let mut holder = db.pool.begin().await.unwrap();
        lock(&mut holder, row.id).await.unwrap();
        let pool = db.pool.clone();
        // The second locker blocks until `holder` commits.
        let waiting = tokio::spawn(async move {
            let mut tx = pool.begin().await.unwrap();
            let seen = lock(&mut tx, row.id).await.unwrap();
            tx.commit().await.unwrap();
            seen
        });
        until_a_lock_is_awaited(&db.pool).await;

        set_state(&mut holder, row.id, ChangesetState::Discarded, now())
            .await
            .unwrap();
        holder.commit().await.unwrap();
        let seen = tokio::time::timeout(PATIENCE, waiting)
            .await
            .expect("the second locker gets the lock")
            .unwrap()
            .expect("the row still exists");

        assert_eq!(seen.state, ChangesetState::Discarded);
        let mut tx = db.pool.begin().await.unwrap();
        assert!(lock(&mut tx, i64::MAX).await.unwrap().is_none());
    }
}

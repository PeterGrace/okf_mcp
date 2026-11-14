//! `oauth_clients`: dynamically registered clients (RFC 7591) and cached
//! Client ID Metadata Documents. Task 2 reads and prunes; Task 3 writes.

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::error::OkfError;

/// How a client got its id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClientKind {
    /// Registered at `POST /oauth/register`; its id is a UUID.
    Dcr,
    /// A Client ID Metadata Document; its id is the document's `https` URL.
    Cimd,
}

impl ClientKind {
    /// The spelling stored in `oauth_clients.kind`.
    ///
    /// # Returns
    /// `"dcr"` or `"cimd"`.
    pub fn as_str(self) -> &'static str {
        match self {
            ClientKind::Dcr => "dcr",
            ClientKind::Cimd => "cimd",
        }
    }

    /// Parses the stored spelling.
    ///
    /// # Arguments
    /// * `text` - an `oauth_clients.kind` value.
    ///
    /// # Returns
    /// The kind, or `None` for anything else.
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "dcr" => Some(ClientKind::Dcr),
            "cimd" => Some(ClientKind::Cimd),
            _ => None,
        }
    }
}

/// One client.
// `PartialEq` but not `Eq`: `serde_json::Value` holds floats.
#[derive(Debug, Clone, PartialEq)]
pub struct ClientRecord {
    /// Its id (a UUID, or the metadata document's URL).
    pub client_id: String,
    /// DCR or CIMD.
    pub kind: ClientKind,
    /// The name it gives itself (shown on the consent page, escaped).
    pub client_name: Option<String>,
    /// Its registered redirect URIs, matched exactly.
    pub redirect_uris: Vec<String>,
    /// Its whole metadata, as registered or fetched.
    pub metadata: serde_json::Value,
    /// When its document was last fetched (CIMD only).
    pub fetched_at: Option<DateTime<Utc>>,
    /// Until when the cached document may be used without a refetch (CIMD only).
    pub cache_expires_at: Option<DateTime<Utc>>,
    /// When it was first stored.
    pub created_at: DateTime<Utc>,
}

/// The select list every client query reads, in [`ClientRow`]'s order
/// (Task 3's writes `RETURNING` it too).
// A macro, not a `const`: sqlx 0.9 takes only a `&'static str` (or an
// audited `AssertSqlSafe`) as SQL, and `concat!("SELECT ", client_columns!(),
// ...)` builds that literal at compile time, where `format!` would build a
// `String` at run time.
macro_rules! client_columns {
    () => {
        "client_id, kind, client_name, redirect_uris, metadata, fetched_at, cache_expires_at, \
         created_at"
    };
}

/// One `oauth_clients` row as stored.
// `derive(sqlx::FromRow)` maps columns to fields by name, like a dataclass
// built from a row dict.
#[derive(sqlx::FromRow)]
struct ClientRow {
    client_id: String,
    kind: String,
    client_name: Option<String>,
    redirect_uris: Vec<String>,
    metadata: serde_json::Value,
    fetched_at: Option<DateTime<Utc>>,
    cache_expires_at: Option<DateTime<Utc>>,
    created_at: DateTime<Utc>,
}

impl From<ClientRow> for ClientRecord {
    fn from(row: ClientRow) -> Self {
        Self {
            client_id: row.client_id,
            kind: ClientKind::parse(&row.kind).expect("the schema checks oauth_clients.kind"),
            client_name: row.client_name,
            redirect_uris: row.redirect_uris,
            metadata: row.metadata,
            fetched_at: row.fetched_at,
            cache_expires_at: row.cache_expires_at,
            created_at: row.created_at,
        }
    }
}

/// A client by id.
///
/// # Arguments
/// * `pool` - the database.
/// * `client_id` - its id.
///
/// # Returns
/// The client, or `None`.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn find(pool: &PgPool, client_id: &str) -> Result<Option<ClientRecord>, OkfError> {
    let row = sqlx::query_as::<_, ClientRow>(concat!(
        "SELECT ",
        client_columns!(),
        " FROM oauth_clients WHERE client_id = $1"
    ))
    .bind(client_id)
    .fetch_optional(pool)
    .await?;
    // `Option::map(ClientRecord::from)` converts the row, if there is one.
    Ok(row.map(ClientRecord::from))
}

/// Deletes the dynamically registered clients created before
/// `created_before` that no grant names: registrations that never led to a
/// sign-in. Metadata-document clients are a cache and stay.
///
/// # Arguments
/// * `pool` - the database.
/// * `created_before` - the cut-off (the poll loop passes a day ago).
///
/// # Returns
/// How many were deleted.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn prune_unbound_dcr(
    pool: &PgPool,
    created_before: DateTime<Utc>,
) -> Result<u64, OkfError> {
    let deleted = sqlx::query(
        "DELETE FROM oauth_clients c WHERE c.kind = 'dcr' AND c.created_at < $1 \
         AND NOT EXISTS (SELECT 1 FROM api_keys k WHERE k.client_id = c.client_id)",
    )
    .bind(created_before)
    .execute(pool)
    .await?;
    Ok(deleted.rows_affected())
}

/// A client registering through RFC 7591, before it has an id.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NewDcrClient<'a> {
    /// Its `client_name`, if it sent one.
    pub client_name: Option<&'a str>,
    /// Its validated redirect URIs.
    pub redirect_uris: &'a [String],
    /// The registered metadata as the registration response echoes it
    /// (without `client_id`, which the database picks).
    pub metadata: &'a serde_json::Value,
}

/// The first key of the DCR registration lock (`okfo` in ASCII): a two-key
/// advisory lock in its own namespace, apart from the catalog locks
/// (`store::catalogs`, namespace `okf`).
const OAUTH_LOCK_NAMESPACE: i32 = 0x6f6b_666f;
/// The second key: the lock every DCR registration takes.
const DCR_REGISTRATION_LOCK: i32 = 1;

/// How many DCR clients no grant names: the subquery the cap and
/// [`count_unbound_dcr`] share.
// A macro, like `client_columns!`, so `concat!` can splice it into
// compile-time SQL.
macro_rules! unbound_dcr_count {
    () => {
        "(SELECT count(*) FROM oauth_clients c WHERE c.kind = 'dcr' \
          AND NOT EXISTS (SELECT 1 FROM api_keys k WHERE k.client_id = c.client_id))"
    };
}

/// Registers a DCR client under a fresh UUID, unless `max_unbound` DCR
/// clients already have no grant. The count and the insert run under one
/// transaction-scoped advisory lock, so concurrent registrations, from this
/// process or any other, take turns: each sees every insert committed
/// before it, and none can pass the cap.
///
/// # Arguments
/// * `pool` - the database.
/// * `new` - the validated registration.
/// * `now` - the registration time (server clock).
/// * `max_unbound` - the cap (spec "Abuse limits": 500).
///
/// # Returns
/// The stored client, or `None` when the cap is reached (nothing stored).
///
/// # Errors
/// [`OkfError::Database`].
pub async fn insert_dcr(
    pool: &PgPool,
    new: &NewDcrClient<'_>,
    now: DateTime<Utc>,
    max_unbound: i64,
) -> Result<Option<ClientRecord>, OkfError> {
    let mut tx = pool.begin().await?;
    // Held until the commit (or the rollback when `tx` is dropped on an
    // error). Read committed is enough under it: the previous holder has
    // committed before this one gets the lock, so the count sees its row.
    sqlx::query("SELECT pg_advisory_xact_lock($1, $2)")
        .bind(OAUTH_LOCK_NAMESPACE)
        .bind(DCR_REGISTRATION_LOCK)
        .execute(&mut *tx)
        .await?;
    // `concat!` splices the shared column list into the `RETURNING` clause
    // at compile time, so the row decodes through the same `ClientRow` as
    // `find`. `INSERT ... SELECT ... WHERE` inserts nothing (and returns no
    // row) when the cap is reached.
    let row = sqlx::query_as::<_, ClientRow>(concat!(
        "INSERT INTO oauth_clients \
         (client_id, kind, client_name, redirect_uris, metadata, created_at) \
         SELECT gen_random_uuid()::text, 'dcr', $1, $2, $3, $4 WHERE ",
        unbound_dcr_count!(),
        " < $5 RETURNING ",
        client_columns!()
    ))
    .bind(new.client_name)
    .bind(new.redirect_uris)
    .bind(new.metadata)
    .bind(now)
    .bind(max_unbound)
    .fetch_optional(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(row.map(ClientRecord::from))
}

/// Stores a freshly fetched Client ID Metadata Document, or refreshes the
/// cached one. The row's `created_at` (its first sighting) is kept. A DCR
/// row can never share the id, since DCR ids are UUIDs and CIMD ids are URLs,
/// but the `WHERE` makes sure a refresh never rewrites one.
///
/// # Arguments
/// * `pool` - the database.
/// * `client` - the validated document, with `kind` `Cimd`.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn upsert_cimd(pool: &PgPool, client: &ClientRecord) -> Result<(), OkfError> {
    sqlx::query(
        "INSERT INTO oauth_clients \
         (client_id, kind, client_name, redirect_uris, metadata, fetched_at, cache_expires_at, \
          created_at) \
         VALUES ($1, 'cimd', $2, $3, $4, $5, $6, $7) \
         ON CONFLICT (client_id) DO UPDATE SET \
           client_name = EXCLUDED.client_name, redirect_uris = EXCLUDED.redirect_uris, \
           metadata = EXCLUDED.metadata, fetched_at = EXCLUDED.fetched_at, \
           cache_expires_at = EXCLUDED.cache_expires_at \
         WHERE oauth_clients.kind = 'cimd'",
    )
    .bind(&client.client_id)
    .bind(&client.client_name)
    .bind(&client.redirect_uris)
    .bind(&client.metadata)
    .bind(client.fetched_at)
    .bind(client.cache_expires_at)
    .bind(client.created_at)
    .execute(pool)
    .await?;
    Ok(())
}

/// How many DCR clients no grant points at: the number the registration cap
/// (spec "Abuse limits", 500) counts ([`insert_dcr`] checks it itself).
///
/// # Arguments
/// * `pool` - the database.
///
/// # Returns
/// The count.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn count_unbound_dcr(pool: &PgPool) -> Result<i64, OkfError> {
    Ok(
        sqlx::query_scalar::<_, i64>(concat!("SELECT ", unbound_dcr_count!()))
            .fetch_one(pool)
            .await?,
    )
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

    async fn insert_dcr_row(pool: &PgPool, client_id: &str, created_at: DateTime<Utc>) {
        sqlx::query(
            "INSERT INTO oauth_clients (client_id, kind, redirect_uris, metadata, created_at) \
             VALUES ($1, 'dcr', ARRAY['https://client.example.com/callback'], '{}', $2)",
        )
        .bind(client_id)
        .bind(created_at)
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn find_reads_a_client_back() {
        let db = test_db().await;
        let id = insert_client(
            &db.pool,
            "https://client.example.com/client.json",
            "Example",
            now(),
        )
        .await;

        let found = find(&db.pool, &id).await.unwrap().expect("stored");
        let missing = find(&db.pool, "no-such-client").await.unwrap();

        assert_eq!(found.kind, ClientKind::Cimd);
        assert_eq!(found.client_name.as_deref(), Some("Example"));
        assert_eq!(found.redirect_uris, ["https://client.example.com/callback"]);
        assert_eq!(found.metadata["client_id"], id.as_str());
        assert_eq!(found.fetched_at, Some(now()));
        assert_eq!(found.cache_expires_at, Some(now() + TimeDelta::hours(1)));
        assert_eq!(missing, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prune_keeps_bound_recent_and_document_clients() {
        let db = test_db().await;
        catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog {
                name: "main",
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &crate::store::audit::AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let old = now() - TimeDelta::hours(25);
        insert_dcr_row(&db.pool, "old-unbound", old).await;
        insert_dcr_row(&db.pool, "old-bound", old).await;
        insert_dcr_row(&db.pool, "recent-unbound", now()).await;
        insert_client(
            &db.pool,
            "https://client.example.com/client.json",
            "Example",
            old,
        )
        .await;
        insert_grant(&db.pool, "old-bound", Access::Read, &["main"], now()).await;

        let pruned = prune_unbound_dcr(&db.pool, now() - TimeDelta::hours(24))
            .await
            .unwrap();

        assert_eq!(pruned, 1);
        assert!(find(&db.pool, "old-unbound").await.unwrap().is_none());
        for kept in [
            "old-bound",
            "recent-unbound",
            "https://client.example.com/client.json",
        ] {
            assert!(find(&db.pool, kept).await.unwrap().is_some(), "{kept}");
        }
    }

    fn cimd(url: &str, name: &str, fetched: DateTime<Utc>) -> ClientRecord {
        ClientRecord {
            client_id: url.to_string(),
            kind: ClientKind::Cimd,
            client_name: Some(name.to_string()),
            redirect_uris: vec!["https://client.example.com/callback".to_string()],
            metadata: serde_json::json!({ "client_id": url, "client_name": name }),
            fetched_at: Some(fetched),
            cache_expires_at: Some(fetched + TimeDelta::minutes(5)),
            created_at: fetched,
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_dcr_client_gets_a_fresh_uuid_and_is_found() {
        let db = test_db().await;
        let uris = vec!["https://client.example.com/callback".to_string()];
        let metadata = serde_json::json!({ "client_name": "Example" });
        let named = NewDcrClient {
            client_name: Some("Example"),
            redirect_uris: &uris,
            metadata: &metadata,
        };

        let first = insert_dcr(&db.pool, &named, now(), 500)
            .await
            .unwrap()
            .expect("under the cap");
        let second = insert_dcr(
            &db.pool,
            &NewDcrClient {
                client_name: None,
                ..named
            },
            now(),
            500,
        )
        .await
        .unwrap()
        .expect("under the cap");

        assert_eq!(first.kind, ClientKind::Dcr);
        assert_eq!(first.client_id.len(), 36, "a UUID: {}", first.client_id);
        assert_ne!(first.client_id, second.client_id);
        assert_eq!(first.client_name.as_deref(), Some("Example"));
        assert_eq!(second.client_name, None);
        assert_eq!(first.redirect_uris, uris);
        assert_eq!(first.metadata, metadata);
        assert_eq!(first.fetched_at, None);
        assert_eq!(first.cache_expires_at, None);
        assert_eq!(first.created_at, now());
        assert_eq!(find(&db.pool, &first.client_id).await.unwrap(), Some(first));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn insert_dcr_stops_at_the_cap_of_unbound_clients() {
        let db = test_db().await;
        catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog {
                name: "main",
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &crate::store::audit::AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        let uris = vec!["https://client.example.com/callback".to_string()];
        let metadata = serde_json::json!({});
        let new = NewDcrClient {
            client_name: None,
            redirect_uris: &uris,
            metadata: &metadata,
        };
        insert_dcr_row(&db.pool, "unbound", now()).await;
        insert_dcr_row(&db.pool, "bound", now()).await;
        insert_grant(&db.pool, "bound", Access::Read, &["main"], now()).await;

        let second = insert_dcr(&db.pool, &new, now(), 2).await.unwrap();
        let third = insert_dcr(&db.pool, &new, now(), 2).await.unwrap();

        assert!(second.is_some(), "a bound client does not count");
        assert_eq!(third, None, "two unbound clients reach a cap of 2");
        assert_eq!(count_unbound_dcr(&db.pool).await.unwrap(), 2);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn upsert_cimd_inserts_then_refreshes_but_keeps_created_at() {
        let db = test_db().await;
        let url = "https://client.example.com/oauth/client.json";
        let earlier = now() - TimeDelta::hours(1);

        upsert_cimd(&db.pool, &cimd(url, "Old", earlier))
            .await
            .unwrap();
        upsert_cimd(&db.pool, &cimd(url, "New", now()))
            .await
            .unwrap();

        let found = find(&db.pool, url).await.unwrap().expect("stored");
        assert_eq!(found.kind, ClientKind::Cimd);
        assert_eq!(found.client_name.as_deref(), Some("New"));
        assert_eq!(found.metadata["client_name"], "New");
        assert_eq!(found.fetched_at, Some(now()));
        assert_eq!(found.cache_expires_at, Some(now() + TimeDelta::minutes(5)));
        assert_eq!(found.created_at, earlier, "the first sighting is kept");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn upsert_cimd_never_rewrites_a_dcr_row() {
        let db = test_db().await;
        insert_dcr_row(&db.pool, "https://client.example.com/c.json", now()).await;

        upsert_cimd(
            &db.pool,
            &cimd("https://client.example.com/c.json", "Spoof", now()),
        )
        .await
        .unwrap();

        let found = find(&db.pool, "https://client.example.com/c.json")
            .await
            .unwrap()
            .expect("stored");
        assert_eq!(found.kind, ClientKind::Dcr);
        assert_eq!(found.client_name, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn only_dcr_clients_without_a_grant_are_counted() {
        let db = test_db().await;
        catalogs::create(
            &db.pool,
            &crate::store::catalogs::NewCatalog {
                name: "main",
                description: "",
                owner_team: crate::db::TEST_TEAM,
            },
            &crate::store::audit::AuditActor::cli(),
            now(),
        )
        .await
        .unwrap();
        insert_dcr_row(&db.pool, "unbound-1", now()).await;
        insert_dcr_row(&db.pool, "unbound-2", now()).await;
        insert_dcr_row(&db.pool, "bound", now()).await;
        insert_client(&db.pool, "https://client.example.com/c.json", "C", now()).await;
        insert_grant(&db.pool, "bound", Access::Read, &["main"], now()).await;

        assert_eq!(count_unbound_dcr(&db.pool).await.unwrap(), 2);
    }
}

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

    async fn insert_dcr(pool: &PgPool, client_id: &str, created_at: DateTime<Utc>) {
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
        catalogs::create(&db.pool, "main", "", now()).await.unwrap();
        let old = now() - TimeDelta::hours(25);
        insert_dcr(&db.pool, "old-unbound", old).await;
        insert_dcr(&db.pool, "old-bound", old).await;
        insert_dcr(&db.pool, "recent-unbound", now()).await;
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
}

//! `sign_in_codes` (OAuth design "Sign-in codes"): one-time credentials an
//! admin mints and hands to a person, each bound to a grant template
//! (catalogs, access, default catalog, actor). Only a code's SHA-256 is
//! stored; the code itself is returned once, by [`create`]. [`redeem`] turns a
//! live code into an OAuth grant, an `api_keys` row with `kind = 'oauth'`, in
//! one transaction. [`prune`] (run by the poll loop) deletes old unredeemed
//! codes.

use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use sqlx::PgPool;

use crate::error::OkfError;
use crate::keys::{self, SIGN_IN_CODE_PREFIX};
use crate::store::catalogs::bump_admin_generation;
use crate::store::keys::{is_valid_name, resolve_grants, Access, Grants};
use crate::store::oauth::clients::{ClientKind, ClientRecord};
use crate::store::users;

/// How long a code stays valid when its creator names no expiry: a day.
pub const DEFAULT_EXPIRY: Duration = Duration::from_secs(24 * 60 * 60);

/// A code to mint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewSignInCode<'a> {
    /// Its label, `[A-Za-z0-9._-]+`; the grant's name starts with it.
    pub name: &'a str,
    /// `Read` or `Write`; `Admin` is refused.
    pub access: Access,
    /// The catalogs the grant will reach.
    pub grants: Grants<'a>,
    /// The grant's default catalog (must be granted).
    pub default_catalog: Option<&'a str>,
    /// The actor the grant's writes are recorded as; blank means the code's
    /// name (as for keys).
    pub actor: &'a str,
    /// When the code stops working if nobody redeems it.
    pub expires_at: DateTime<Utc>,
    /// The user it, and the grant it becomes, belongs to (must exist).
    pub user: &'a str,
}

/// A minted code: its id and, this once, the code itself.
// `Debug` by hand, so a stray `{:?}` can never print the code.
#[derive(Clone, PartialEq, Eq)]
pub struct CreatedSignInCode {
    /// Its immutable id.
    pub id: String,
    /// The `okfsc_` code. Not stored; it cannot be shown again.
    pub code: String,
}

impl std::fmt::Debug for CreatedSignInCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("CreatedSignInCode")
            .field("id", &self.id)
            .field("code", &"<redacted>")
            .finish()
    }
}

/// Where a code is in its life.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodeState {
    /// Unexpired and unused: it can be redeemed.
    Live,
    /// Past `expires_at`, never redeemed or revoked.
    Expired,
    /// Turned into a grant.
    Redeemed,
    /// Revoked before anyone redeemed it.
    Revoked,
}

impl CodeState {
    /// The spelling the CLI and the API show.
    ///
    /// # Returns
    /// `"live"`, `"expired"`, `"redeemed"` or `"revoked"`.
    pub fn as_str(self) -> &'static str {
        match self {
            CodeState::Live => "live",
            CodeState::Expired => "expired",
            CodeState::Redeemed => "redeemed",
            CodeState::Revoked => "revoked",
        }
    }
}

/// A stored code (never the code or its hash).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignInCodeRecord {
    /// Its immutable id.
    pub id: String,
    /// Its label.
    pub name: String,
    /// The grant's access.
    pub access: Access,
    /// The grant reaches every catalog.
    pub all_catalogs: bool,
    /// The grant's catalogs, sorted (empty with `all_catalogs`).
    pub catalogs: Vec<String>,
    /// The grant's default catalog.
    pub default_catalog: Option<String>,
    /// The grant's actor.
    pub actor: String,
    /// When it was minted.
    pub created_at: DateTime<Utc>,
    /// When it stops working unredeemed.
    pub expires_at: DateTime<Utc>,
    /// When it was redeemed.
    pub redeemed_at: Option<DateTime<Utc>>,
    /// The grant it became (cleared if that key row is ever deleted).
    pub grant_id: Option<String>,
    /// When it was revoked.
    pub revoked_at: Option<DateTime<Utc>>,
    /// The user it belongs to; `None` for a code from before migration 0009
    /// that no admin has assigned yet.
    pub user: Option<String>,
}

impl SignInCodeRecord {
    /// Its state at `now`.
    ///
    /// # Arguments
    /// * `now` - the server clock's time.
    ///
    /// # Returns
    /// Redeemed or revoked if it was, else expired once `expires_at` is
    /// reached, else live.
    pub fn state(&self, now: DateTime<Utc>) -> CodeState {
        if self.redeemed_at.is_some() {
            CodeState::Redeemed
        } else if self.revoked_at.is_some() {
            CodeState::Revoked
        } else if self.expires_at <= now {
            CodeState::Expired
        } else {
            CodeState::Live
        }
    }
}

/// One `sign_in_codes` row with its catalog names, before `access` is parsed.
// `derive(sqlx::FromRow)` maps columns to fields by name.
#[derive(sqlx::FromRow)]
struct CodeRow {
    id: String,
    name: String,
    access: String,
    all_catalogs: bool,
    catalogs: Vec<String>,
    default_catalog: Option<String>,
    actor: String,
    created_at: DateTime<Utc>,
    expires_at: DateTime<Utc>,
    redeemed_at: Option<DateTime<Utc>>,
    grant_id: Option<String>,
    revoked_at: Option<DateTime<Utc>>,
    user: Option<String>,
}

impl From<CodeRow> for SignInCodeRecord {
    fn from(row: CodeRow) -> Self {
        Self {
            id: row.id,
            name: row.name,
            access: Access::parse(&row.access).expect("the schema checks sign_in_codes.access"),
            all_catalogs: row.all_catalogs,
            catalogs: row.catalogs,
            default_catalog: row.default_catalog,
            actor: row.actor,
            created_at: row.created_at,
            expires_at: row.expires_at,
            redeemed_at: row.redeemed_at,
            grant_id: row.grant_id,
            revoked_at: row.revoked_at,
            user: row.user,
        }
    }
}

// A macro that expands to a string literal, so `concat!` can glue the shared
// SELECT and each query's tail together at compile time (a `const &str`
// cannot go into `concat!`, and sqlx 0.9 refuses a `format!`ed `String`).
macro_rules! select_codes {
    () => {
        "SELECT s.id, s.name, s.access, s.all_catalogs, d.name AS default_catalog, s.actor, \
                s.created_at, s.expires_at, s.redeemed_at, s.grant_id, s.revoked_at, \
                u.name AS user, \
                COALESCE(array_agg(c.name ORDER BY c.name) FILTER (WHERE c.name IS NOT NULL), \
                         '{}'::text[]) AS catalogs \
         FROM sign_in_codes s \
         LEFT JOIN catalogs d ON d.id = s.default_catalog_id \
         LEFT JOIN users u ON u.id = s.user_id \
         LEFT JOIN sign_in_code_catalogs g ON g.code_id = s.id \
         LEFT JOIN catalogs c ON c.id = g.catalog_id"
    };
}

/// When a code asked to last `requested` expires: the one rule the CLI and
/// the API share.
///
/// # Arguments
/// * `now` - the server clock's time.
/// * `requested` - how long it should last.
/// * `max` - `oauth.code_max_expiry`.
///
/// # Returns
/// `now + requested`.
///
/// # Errors
/// [`OkfError::Refused`] for zero, more than `max`, or a time chrono cannot
/// represent.
pub fn expiry(
    now: DateTime<Utc>,
    requested: Duration,
    max: Duration,
) -> Result<DateTime<Utc>, OkfError> {
    // `humantime_serde` re-exports the `humantime` crate it wraps.
    use humantime_serde::re::humantime::format_duration;
    if requested.is_zero() || requested > max {
        return Err(OkfError::Refused {
            reason: format!(
                "a sign-in code must expire within {} (oauth.code_max_expiry), not {}",
                format_duration(max),
                format_duration(requested)
            ),
        });
    }
    // `checked_add_signed` is `None` past chrono's range instead of panicking.
    TimeDelta::from_std(requested)
        .ok()
        .and_then(|delta| now.checked_add_signed(delta))
        .ok_or_else(|| OkfError::Refused {
            reason: "the expiry is out of range".to_string(),
        })
}

/// Mints a code and its catalog grants in one transaction. It changes no
/// key, so the admin generation is left alone.
///
/// # Arguments
/// * `pool` - the database.
/// * `new` - the code's template and expiry.
/// * `now` - the creation time (server clock).
///
/// # Returns
/// The code itself: hand it over once; it is not stored and cannot be shown again.
///
/// # Errors
/// [`OkfError::Refused`] for an invalid name, `admin` access, an expiry not
/// after `now`, no catalogs or an ungranted default;
/// [`OkfError::CatalogNotSelected`] for an unknown catalog;
/// [`OkfError::UnknownName`] for an unknown user; [`OkfError::Io`] if the OS
/// random source fails; [`OkfError::Database`] otherwise.
pub async fn create(
    pool: &PgPool,
    new: &NewSignInCode<'_>,
    now: DateTime<Utc>,
) -> Result<CreatedSignInCode, OkfError> {
    let refuse = |reason: String| Err(OkfError::Refused { reason });
    if !is_valid_name(new.name) {
        return refuse(format!(
            "sign-in code name {:?} must be letters, digits, '.', '_' and '-'",
            new.name
        ));
    }
    if new.access == Access::Admin {
        return refuse("a sign-in code grants read or write access, never admin".to_string());
    }
    // A blank actor defaults to the code's name, as a key's does (Decision 6).
    let actor = match new.actor.trim() {
        "" => new.name,
        trimmed => trimmed,
    };
    if new.expires_at <= now {
        return refuse(format!(
            "sign-in code {:?} must expire in the future",
            new.name
        ));
    }
    let mut tx = pool.begin().await?;
    let (granted, default_catalog_id) = resolve_grants(
        &mut tx,
        "sign-in code",
        new.name,
        new.grants,
        new.default_catalog,
    )
    .await?;
    // Held `FOR KEY SHARE` until commit, so a concurrent `users::delete`
    // either waits and then counts this code, or wins and leaves an unknown name.
    let user_id = users::id_of(&mut tx, new.user).await?;
    let code = keys::generate_with(SIGN_IN_CODE_PREFIX)?;
    let id: String = sqlx::query_scalar(
        "INSERT INTO sign_in_codes \
         (name, sha256, access, all_catalogs, default_catalog_id, actor, created_at, expires_at, \
          user_id) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9) RETURNING id",
    )
    .bind(new.name)
    .bind(keys::hash(&code).as_slice())
    .bind(new.access.as_str())
    .bind(matches!(new.grants, Grants::All))
    .bind(default_catalog_id)
    .bind(actor)
    .bind(now)
    .bind(new.expires_at)
    .bind(&user_id)
    .fetch_one(&mut *tx)
    .await?;
    // `unnest` turns the bound array into rows; a name given twice is one grant.
    sqlx::query(
        "INSERT INTO sign_in_code_catalogs (code_id, catalog_id) \
         SELECT $1, unnest($2::bigint[]) ON CONFLICT DO NOTHING",
    )
    .bind(&id)
    .bind(&granted)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(CreatedSignInCode { id, code })
}

/// Every code, newest first, in every state.
///
/// # Arguments
/// * `pool` - the database.
///
/// # Returns
/// The records.
///
/// # Errors
/// [`OkfError::Database`].
pub async fn list(pool: &PgPool) -> Result<Vec<SignInCodeRecord>, OkfError> {
    // Grouping by the primary key `s.id` lets the query select `s`'s other
    // columns ungrouped (Postgres knows they depend on it).
    let rows = sqlx::query_as::<_, CodeRow>(concat!(
        select_codes!(),
        " GROUP BY s.id, d.name, u.name ORDER BY s.created_at DESC, s.id"
    ))
    .fetch_all(pool)
    .await?;
    Ok(rows.into_iter().map(SignInCodeRecord::from).collect())
}

/// The live code a person typed, if it is one.
///
/// # Arguments
/// * `pool` - the database.
/// * `presented` - the text entered on the consent page.
/// * `now` - the server clock's time.
///
/// # Returns
/// Its record when it exists, is unexpired, and is neither redeemed nor
/// revoked; `None` otherwise (one answer for every reason, so the consent
/// page cannot tell them apart).
///
/// # Errors
/// [`OkfError::Database`].
pub async fn find_live(
    pool: &PgPool,
    presented: &str,
    now: DateTime<Utc>,
) -> Result<Option<SignInCodeRecord>, OkfError> {
    let presented = presented.trim();
    if !presented.starts_with(SIGN_IN_CODE_PREFIX) {
        // Not a sign-in code at all: no query.
        return Ok(None);
    }
    let row = sqlx::query_as::<_, CodeRow>(concat!(
        select_codes!(),
        " WHERE s.sha256 = $1 AND s.redeemed_at IS NULL AND s.revoked_at IS NULL \
          AND s.expires_at > $2 \
          GROUP BY s.id, d.name, u.name"
    ))
    .bind(keys::hash(presented).as_slice())
    .bind(now)
    .fetch_optional(pool)
    .await?;
    Ok(row.map(SignInCodeRecord::from))
}

/// Revokes a code nobody has redeemed (an expired one too).
///
/// # Arguments
/// * `pool` - the database.
/// * `id` - the code's id.
/// * `now` - the revocation time.
///
/// # Errors
/// [`OkfError::Refused`] for an unknown, redeemed or already revoked code;
/// [`OkfError::Database`].
pub async fn revoke(pool: &PgPool, id: &str, now: DateTime<Utc>) -> Result<(), OkfError> {
    let revoked = sqlx::query(
        "UPDATE sign_in_codes SET revoked_at = $2 \
         WHERE id = $1 AND revoked_at IS NULL AND redeemed_at IS NULL",
    )
    .bind(id)
    .bind(now)
    .execute(pool)
    .await?;
    if revoked.rows_affected() == 0 {
        return Err(OkfError::Refused {
            reason: format!("no unredeemed, unrevoked sign-in code {id:?}"),
        });
    }
    Ok(())
}

/// Gives a sign-in code a user, in any state (a redeemed code's history is
/// attributed too). The grant it became is a key row and is assigned
/// separately ([`crate::store::keys::assign_user_by_id`]). A code is not in
/// the key cache, so the admin generation is left alone.
///
/// # Arguments
/// * `pool` - the database.
/// * `id` - the code's id.
/// * `user` - the user it belongs to from now on.
///
/// # Errors
/// [`OkfError::UnknownName`] for an unknown user; [`OkfError::NotFound`]
/// (`oauth/codes/<id>`) for an unknown code; [`OkfError::Database`].
pub async fn assign_user(pool: &PgPool, id: &str, user: &str) -> Result<(), OkfError> {
    let mut tx = pool.begin().await?;
    // `FOR KEY SHARE` until commit: see `users::id_of`.
    let user_id = users::id_of(&mut tx, user).await?;
    let updated = sqlx::query("UPDATE sign_in_codes SET user_id = $2 WHERE id = $1")
        .bind(id)
        .bind(&user_id)
        .execute(&mut *tx)
        .await?;
    if updated.rows_affected() == 0 {
        return Err(OkfError::NotFound {
            path: format!("oauth/codes/{id}"),
        });
    }
    tx.commit().await?;
    Ok(())
}

/// The locked row [`redeem`] reads.
#[derive(sqlx::FromRow)]
struct LockedCode {
    name: String,
    access: String,
    all_catalogs: bool,
    default_catalog_id: Option<i64>,
    actor: String,
    expires_at: DateTime<Utc>,
    used: bool,
    /// The code's user, which the grant inherits (`None` for a legacy code).
    user_id: Option<String>,
}

/// Turns a live code into an OAuth grant for `client`, in one transaction:
/// lock the code, re-check it, insert the grant and its catalogs, mark the
/// code redeemed, and bump the admin generation so the key cache loads the
/// grant. Any failure rolls all of it back and leaves the code as it was.
///
/// # Arguments
/// * `pool` - the database.
/// * `code_id` - the code (from [`find_live`]).
/// * `client` - the client being authorized (already stored).
/// * `now` - the redemption time.
///
/// # Returns
/// The grant's id (its `api_keys.id`).
///
/// # Errors
/// [`OkfError::Refused`] when the code is unknown, expired, redeemed or
/// revoked (one message for all four), or its named catalogs are all gone;
/// [`OkfError::Database`].
pub async fn redeem(
    pool: &PgPool,
    code_id: &str,
    client: &ClientRecord,
    now: DateTime<Utc>,
) -> Result<String, OkfError> {
    let not_live = || OkfError::Refused {
        reason: "the sign-in code is unknown, expired, redeemed or revoked".to_string(),
    };
    // Dropping `tx` without `commit` (every early return below) rolls back.
    let mut tx = pool.begin().await?;
    // `FOR UPDATE` holds the row until commit: a racing redeem of the same
    // code waits here, then reads the committed `redeemed_at` and is refused.
    let code: Option<LockedCode> = sqlx::query_as(
        "SELECT name, access, all_catalogs, default_catalog_id, actor, expires_at, \
                (redeemed_at IS NOT NULL OR revoked_at IS NOT NULL) AS used, user_id \
         FROM sign_in_codes WHERE id = $1 FOR UPDATE",
    )
    .bind(code_id)
    .fetch_optional(&mut *tx)
    .await?;
    let code = code.ok_or_else(not_live)?;
    if code.used || code.expires_at <= now {
        return Err(not_live());
    }
    let catalog_ids: Vec<i64> = sqlx::query_scalar(
        "SELECT catalog_id FROM sign_in_code_catalogs WHERE code_id = $1 ORDER BY catalog_id",
    )
    .bind(code_id)
    .fetch_all(&mut *tx)
    .await?;
    if !code.all_catalogs && catalog_ids.is_empty() {
        return Err(OkfError::Refused {
            reason: "every catalog this sign-in code granted has been deleted".to_string(),
        });
    }
    // The live name/actor unique index covers `kind = 'key'` only (migration
    // 0008), so a grant's name can never collide with a key or another grant.
    let grant_id: String = sqlx::query_scalar(
        "INSERT INTO api_keys \
         (name, sha256, kind, client_id, access, maintenance_report, all_catalogs, \
          default_catalog_id, actor, created_at, user_id) \
         VALUES ($1, NULL, 'oauth', $2, $3, false, $4, $5, $6, $7, $8) RETURNING id",
    )
    .bind(grant_name(&code.name, client, code_id))
    .bind(&client.client_id)
    .bind(&code.access)
    .bind(code.all_catalogs)
    .bind(code.default_catalog_id)
    .bind(&code.actor)
    .bind(now)
    .bind(&code.user_id)
    .fetch_one(&mut *tx)
    .await?;
    sqlx::query("INSERT INTO key_catalogs (key_id, catalog_id) SELECT $1, unnest($2::bigint[])")
        .bind(&grant_id)
        .bind(&catalog_ids)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE sign_in_codes SET redeemed_at = $2, grant_id = $3 WHERE id = $1")
        .bind(code_id)
        .bind(now)
        .bind(&grant_id)
        .execute(&mut *tx)
        .await?;
    bump_admin_generation(&mut tx).await?;
    tx.commit().await?;
    Ok(grant_id)
}

/// A grant's key name (Task 4, Decision 1): `<code name>.<client host>-<code
/// id prefix>`. The suffix tells apart the grants of two codes with one name
/// for one client.
///
/// # Arguments
/// * `code_name` - the code's label (already in the name charset).
/// * `client` - the client being authorized.
/// * `code_id` - the code's id.
///
/// # Returns
/// A name that passes the `api_keys` name check.
fn grant_name(code_name: &str, client: &ClientRecord, code_id: &str) -> String {
    // `get(..8)` is `None` for a shorter id instead of panicking.
    let suffix = code_id.get(..8).unwrap_or(code_id);
    format!("{code_name}.{}-{suffix}", client_host(client))
}

/// The client's host for a grant name: the CIMD document URL's host, or a
/// DCR client's first redirect URI's, lowercased and mapped to
/// `[a-z0-9._-]`; `client` when nothing is left.
///
/// # Arguments
/// * `client` - the client.
///
/// # Returns
/// A non-empty string in the key name charset.
fn client_host(client: &ClientRecord) -> String {
    let source = match client.kind {
        ClientKind::Cimd => client.client_id.as_str(),
        ClientKind::Dcr => client.redirect_uris.first().map_or("", String::as_str),
    };
    // `split_once` is Python's `str.partition` without the separator.
    let rest = source.split_once("://").map_or(source, |(_, rest)| rest);
    // The first `split` item always exists (an empty string at worst).
    let authority = rest.split(['/', '?', '#']).next().unwrap_or_default();
    let host_port = authority
        .rsplit_once('@')
        .map_or(authority, |(_, host)| host);
    let host = match host_port.rsplit_once(':') {
        // A port follows the last ':' unless that ':' is inside "[...]" (IPv6).
        Some((host, port)) if !port.contains(']') => host,
        _ => host_port,
    };
    let mapped: String = host
        .chars()
        .map(|c| match c {
            'A'..='Z' => c.to_ascii_lowercase(),
            'a'..='z' | '0'..='9' | '.' | '_' | '-' => c,
            _ => '-',
        })
        .collect();
    let trimmed = mapped.trim_matches(|c| c == '.' || c == '-');
    if trimmed.is_empty() {
        "client".to_string()
    } else {
        trimmed.to_string()
    }
}

/// Deletes the unredeemed codes that expired before `expired_before`.
/// Redeemed codes stay: they record which code made which grant.
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
    let deleted =
        sqlx::query("DELETE FROM sign_in_codes WHERE redeemed_at IS NULL AND expires_at < $1")
            .bind(expired_before)
            .execute(pool)
            .await?;
    Ok(deleted.rows_affected())
}

/// Stores an OAuth client row, for tests that need a client of either kind
/// to redeem a code for (Task 3's writers are not available to this task).
///
/// # Arguments
/// * `pool` - the database.
/// * `client_id` - the id (the document URL for CIMD).
/// * `kind` - DCR or CIMD.
/// * `redirect_uri` - its one redirect URI.
///
/// # Returns
/// The stored record, read back.
///
/// # Panics
/// If the insert fails: a broken test setup.
#[cfg(test)]
pub(crate) async fn insert_test_client(
    pool: &PgPool,
    client_id: &str,
    kind: ClientKind,
    redirect_uri: &str,
) -> ClientRecord {
    // A CIMD row must carry its fetch times (`oauth_clients_fetched_cimd`);
    // a DCR row must not.
    sqlx::query(
        "INSERT INTO oauth_clients \
         (client_id, kind, client_name, redirect_uris, metadata, fetched_at, cache_expires_at, \
          created_at) \
         VALUES ($1, $2, 'Example Client', ARRAY[$3], '{}'::jsonb, \
                 CASE WHEN $2 = 'cimd' THEN now() END, \
                 CASE WHEN $2 = 'cimd' THEN now() + interval '1 hour' END, now())",
    )
    .bind(client_id)
    .bind(kind.as_str())
    .bind(redirect_uri)
    .execute(pool)
    .await
    .expect("insert a test client");
    crate::store::oauth::clients::find(pool, client_id)
        .await
        .expect("read the test client")
        .expect("the test client exists")
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    use crate::db::pg_support::TestDb;
    use crate::db::test_db;
    use crate::store::catalogs;
    use crate::store::keys::{self as key_rows, KeyKind};
    use crate::store::oauth::testing::{insert_client, insert_grant};
    use crate::store::users;

    fn now() -> DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 11, 11, 12, 0, 0).unwrap()
    }

    fn new_code<'a>(
        name: &'a str,
        grants: Grants<'a>,
        default: Option<&'a str>,
    ) -> NewSignInCode<'a> {
        NewSignInCode {
            name,
            access: Access::Write,
            grants,
            default_catalog: default,
            actor: "example-actor",
            expires_at: now() + TimeDelta::hours(24),
            user: crate::db::TEST_USER,
        }
    }

    async fn with_catalogs(names: &[&str]) -> TestDb {
        let db = test_db().await;
        for name in names {
            catalogs::create(&db.pool, name, "", crate::db::TEST_TEAM, now())
                .await
                .unwrap();
        }
        db
    }

    async fn dcr_client(pool: &PgPool) -> ClientRecord {
        insert_test_client(
            pool,
            "dcr-client",
            ClientKind::Dcr,
            "https://client.example.com/callback",
        )
        .await
    }

    async fn oauth_grants(pool: &PgPool) -> Vec<key_rows::KeyRecord> {
        key_rows::list(pool)
            .await
            .unwrap()
            .into_iter()
            .filter(|key| key.kind == KeyKind::OAuth)
            .collect()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_stores_only_the_hash_and_the_template() {
        let db = with_catalogs(&["main", "other"]).await;
        let names = ["other".to_string(), "main".to_string()];
        let generation = catalogs::admin_generation(&db.pool).await.unwrap();

        let created = create(
            &db.pool,
            &new_code("laptop", Grants::Catalogs(&names), Some("main")),
            now(),
        )
        .await
        .unwrap();

        assert!(created.code.starts_with(crate::keys::SIGN_IN_CODE_PREFIX));
        assert!(
            !format!("{created:?}").contains(&created.code),
            "Debug redacts the code"
        );
        let codes = list(&db.pool).await.unwrap();
        assert_eq!(codes.len(), 1);
        let code = &codes[0];
        assert_eq!(code.id, created.id);
        assert_eq!(code.name, "laptop");
        assert_eq!(code.access, Access::Write);
        assert!(!code.all_catalogs);
        assert_eq!(code.catalogs, ["main", "other"]);
        assert_eq!(code.default_catalog.as_deref(), Some("main"));
        assert_eq!(code.actor, "example-actor");
        assert_eq!(code.state(now()), CodeState::Live);
        let stored: Vec<u8> = sqlx::query_scalar("SELECT sha256 FROM sign_in_codes")
            .fetch_one(&db.pool)
            .await
            .unwrap();
        assert!(crate::keys::matches(&stored, &created.code));
        assert_eq!(
            catalogs::admin_generation(&db.pool).await.unwrap(),
            generation,
            "minting a code changes no key, so the key cache is not reloaded"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn create_refuses_admin_bad_names_ungranted_defaults_and_the_past() {
        // A blank actor is no longer refused: see
        // `a_blank_actor_defaults_to_the_codes_name` (plan Decision 6).
        let db = with_catalogs(&["main"]).await;
        let main = ["main".to_string()];
        let nope = ["nope".to_string()];
        let refused = |result: Result<CreatedSignInCode, OkfError>| {
            matches!(result, Err(OkfError::Refused { .. }))
        };
        let admin = NewSignInCode {
            access: Access::Admin,
            ..new_code("a", Grants::Catalogs(&main), None)
        };
        let past = NewSignInCode {
            expires_at: now(),
            ..new_code("p", Grants::Catalogs(&main), None)
        };

        assert!(refused(create(&db.pool, &admin, now()).await));
        assert!(refused(create(&db.pool, &past, now()).await));
        assert!(refused(
            create(
                &db.pool,
                &new_code("bad name", Grants::Catalogs(&main), None),
                now()
            )
            .await
        ));
        assert!(refused(
            create(&db.pool, &new_code("x", Grants::Catalogs(&[]), None), now()).await
        ));
        assert!(refused(
            create(
                &db.pool,
                &new_code("x", Grants::Catalogs(&main), Some("other")),
                now()
            )
            .await
        ));
        assert!(matches!(
            create(
                &db.pool,
                &new_code("x", Grants::Catalogs(&nope), None),
                now()
            )
            .await,
            Err(OkfError::CatalogNotSelected { .. })
        ));
        assert!(list(&db.pool).await.unwrap().is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn find_live_skips_expired_revoked_and_redeemed_codes() {
        let db = with_catalogs(&["main"]).await;
        let main = ["main".to_string()];
        let client = dcr_client(&db.pool).await;
        let mint = |name: &'static str| {
            let pool = db.pool.clone();
            let main = main.clone();
            async move {
                create(&pool, &new_code(name, Grants::Catalogs(&main), None), now())
                    .await
                    .unwrap()
            }
        };
        let live = mint("live").await;
        let revoked = mint("gone").await;
        let redeemed = mint("used").await;
        revoke(&db.pool, &revoked.id, now()).await.unwrap();
        redeem(&db.pool, &redeemed.id, &client, now())
            .await
            .unwrap();
        let later = now() + TimeDelta::hours(25);

        let found = find_live(&db.pool, &format!(" {} ", live.code), now())
            .await
            .unwrap();

        assert_eq!(found.map(|code| code.id), Some(live.id.clone()));
        assert!(find_live(&db.pool, &live.code, later)
            .await
            .unwrap()
            .is_none());
        assert!(find_live(&db.pool, &revoked.code, now())
            .await
            .unwrap()
            .is_none());
        assert!(find_live(&db.pool, &redeemed.code, now())
            .await
            .unwrap()
            .is_none());
        assert!(find_live(&db.pool, "okf_not-a-code", now())
            .await
            .unwrap()
            .is_none());
        let states: Vec<(String, CodeState)> = list(&db.pool)
            .await
            .unwrap()
            .into_iter()
            .map(|code| (code.name.clone(), code.state(later)))
            .collect();
        assert!(states.contains(&("live".to_string(), CodeState::Expired)));
        assert!(states.contains(&("gone".to_string(), CodeState::Revoked)));
        assert!(states.contains(&("used".to_string(), CodeState::Redeemed)));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoke_refuses_redeemed_revoked_and_unknown_codes() {
        let db = with_catalogs(&["main"]).await;
        let main = ["main".to_string()];
        let client = dcr_client(&db.pool).await;
        let code = create(
            &db.pool,
            &new_code("c", Grants::Catalogs(&main), None),
            now(),
        )
        .await
        .unwrap();
        let used = create(
            &db.pool,
            &new_code("u", Grants::Catalogs(&main), None),
            now(),
        )
        .await
        .unwrap();
        redeem(&db.pool, &used.id, &client, now()).await.unwrap();

        revoke(&db.pool, &code.id, now()).await.unwrap();

        for id in [code.id.as_str(), used.id.as_str(), "no-such-code"] {
            assert!(matches!(
                revoke(&db.pool, id, now()).await,
                Err(OkfError::Refused { .. })
            ));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn redeem_turns_the_code_into_an_oauth_grant_once() {
        let db = with_catalogs(&["main", "other"]).await;
        let names = ["main".to_string(), "other".to_string()];
        let client = insert_test_client(
            &db.pool,
            "https://client.example.com/client.json",
            ClientKind::Cimd,
            "https://callback.example.com/cb",
        )
        .await;
        let code = create(
            &db.pool,
            &new_code("laptop", Grants::Catalogs(&names), Some("other")),
            now(),
        )
        .await
        .unwrap();
        let generation = catalogs::admin_generation(&db.pool).await.unwrap();

        let grant_id = redeem(&db.pool, &code.id, &client, now()).await.unwrap();

        let grant = key_rows::list(&db.pool)
            .await
            .unwrap()
            .into_iter()
            .find(|key| key.id == grant_id)
            .expect("the grant is a key row");
        assert_eq!(grant.kind, KeyKind::OAuth);
        assert_eq!(grant.sha256, None);
        assert_eq!(grant.access, Access::Write);
        assert!(!grant.maintenance_report);
        assert!(!grant.all_catalogs);
        assert_eq!(grant.catalogs, ["main", "other"]);
        assert_eq!(grant.default_catalog.as_deref(), Some("other"));
        assert_eq!(grant.actor, "example-actor");
        assert_eq!(grant.client_id.as_deref(), Some(client.client_id.as_str()));
        assert_eq!(grant.created_at, now());
        // The CIMD client's host, not the redirect URI's (Decision 1).
        let suffix = &code.id[..8];
        assert_eq!(grant.name, format!("laptop.client.example.com-{suffix}"));
        assert_eq!(
            catalogs::admin_generation(&db.pool).await.unwrap(),
            generation + 1,
            "redeeming reloads the key cache"
        );
        let redeemed = list(&db.pool).await.unwrap().remove(0);
        assert_eq!(redeemed.state(now()), CodeState::Redeemed);
        assert_eq!(redeemed.redeemed_at, Some(now()));
        assert_eq!(redeemed.grant_id.as_deref(), Some(grant_id.as_str()));
        assert!(matches!(
            redeem(&db.pool, &code.id, &client, now()).await,
            Err(OkfError::Refused { .. })
        ));
        assert_eq!(oauth_grants(&db.pool).await.len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn redeem_refuses_expired_revoked_and_unknown_codes_and_changes_nothing() {
        let db = with_catalogs(&["main"]).await;
        let all = NewSignInCode {
            grants: Grants::All,
            ..new_code("all", Grants::All, None)
        };
        let client = dcr_client(&db.pool).await;
        let expired = create(&db.pool, &all, now()).await.unwrap();
        let revoked = create(&db.pool, &all, now()).await.unwrap();
        revoke(&db.pool, &revoked.id, now()).await.unwrap();
        let generation = catalogs::admin_generation(&db.pool).await.unwrap();
        let after_expiry = now() + TimeDelta::hours(24);

        for (id, at) in [
            (expired.id.as_str(), after_expiry),
            (revoked.id.as_str(), now()),
            ("no-such-code", now()),
        ] {
            let result = redeem(&db.pool, id, &client, at).await;
            assert!(
                matches!(&result, Err(OkfError::Refused { reason }) if reason.contains("unknown, expired")),
                "{result:?}"
            );
        }

        assert!(oauth_grants(&db.pool).await.is_empty());
        assert_eq!(
            catalogs::admin_generation(&db.pool).await.unwrap(),
            generation
        );
        // An all-catalogs code still redeems while it is live.
        let grant = redeem(&db.pool, &expired.id, &client, now()).await.unwrap();
        let grants = oauth_grants(&db.pool).await;
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].id, grant);
        assert!(grants[0].all_catalogs && grants[0].catalogs.is_empty());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_racing_double_redeem_yields_one_grant() {
        let db = with_catalogs(&["main"]).await;
        let main = ["main".to_string()];
        let client = dcr_client(&db.pool).await;
        let code = create(
            &db.pool,
            &new_code("race", Grants::Catalogs(&main), None),
            now(),
        )
        .await
        .unwrap();
        // Hold the code's row lock on a third connection, so both redeems
        // are certainly in flight at once, each on its own pooled connection,
        // blocked on the same `FOR UPDATE`.
        let mut blocker = db.pool.begin().await.unwrap();
        sqlx::query("SELECT id FROM sign_in_codes WHERE id = $1 FOR UPDATE")
            .bind(&code.id)
            .execute(&mut *blocker)
            .await
            .unwrap();
        // `tokio::spawn` runs each redeem as its own task, possibly on its
        // own thread (the runtime is multi-threaded); `move` gives each task
        // its own copies.
        let spawn_redeem = || {
            let pool = db.pool.clone();
            let id = code.id.clone();
            let client = client.clone();
            tokio::spawn(async move { redeem(&pool, &id, &client, now()).await })
        };
        let first = spawn_redeem();
        let second = spawn_redeem();
        let waiting = || async {
            sqlx::query_scalar::<_, i64>(
                "SELECT count(*) FROM pg_stat_activity \
                 WHERE datname = current_database() AND wait_event_type = 'Lock' \
                 AND query LIKE '%FROM sign_in_codes%FOR UPDATE%'",
            )
            .fetch_one(&db.pool)
            .await
            .unwrap()
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        while waiting().await < 2 {
            assert!(
                std::time::Instant::now() < deadline,
                "both redeems should block on the row lock"
            );
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        }

        blocker.rollback().await.unwrap();
        let first = first.await.expect("the first redeem task");
        let second = second.await.expect("the second redeem task");

        let outcomes = [&first, &second];
        assert_eq!(
            outcomes.iter().filter(|result| result.is_ok()).count(),
            1,
            "{first:?} {second:?}"
        );
        assert!(outcomes
            .iter()
            .any(|result| matches!(result, Err(OkfError::Refused { .. }))));
        let grants = oauth_grants(&db.pool).await;
        assert_eq!(grants.len(), 1);
        let winner = first.or(second).expect("one redeem won");
        assert_eq!(grants[0].id, winner);
        assert_eq!(
            list(&db.pool).await.unwrap()[0].grant_id.as_deref(),
            Some(winner.as_str())
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn two_codes_with_one_name_for_one_client_make_two_grants() {
        let db = with_catalogs(&["main"]).await;
        let main = ["main".to_string()];
        let client = dcr_client(&db.pool).await;
        let first = create(
            &db.pool,
            &new_code("phone", Grants::Catalogs(&main), None),
            now(),
        )
        .await
        .unwrap();
        let second = create(
            &db.pool,
            &new_code("phone", Grants::Catalogs(&main), None),
            now(),
        )
        .await
        .unwrap();

        let one = redeem(&db.pool, &first.id, &client, now()).await.unwrap();
        let two = redeem(&db.pool, &second.id, &client, now()).await.unwrap();

        assert_ne!(one, two);
        let names: Vec<String> = oauth_grants(&db.pool)
            .await
            .into_iter()
            .map(|key| key.name)
            .collect();
        assert_eq!(names.len(), 2);
        assert_ne!(names[0], names[1]);
        assert!(names
            .iter()
            .all(|name| name.starts_with("phone.client.example.com-")));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn redeem_refuses_a_code_whose_catalogs_are_gone() {
        let db = with_catalogs(&["main"]).await;
        let main = ["main".to_string()];
        let client = dcr_client(&db.pool).await;
        let code = create(
            &db.pool,
            &new_code("c", Grants::Catalogs(&main), Some("main")),
            now(),
        )
        .await
        .unwrap();

        catalogs::delete(&db.pool, "main", true).await.unwrap();

        // Deleting the catalog cascades to the code's grants and clears its default.
        let listed = list(&db.pool).await.unwrap().remove(0);
        assert!(listed.catalogs.is_empty());
        assert_eq!(listed.default_catalog, None);
        assert!(matches!(
            redeem(&db.pool, &code.id, &client, now()).await,
            Err(OkfError::Refused { reason }) if reason.contains("deleted")
        ));
        assert_eq!(
            list(&db.pool).await.unwrap()[0].state(now()),
            CodeState::Live
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_oauth_grant_cannot_be_given_maintenance_reporting() {
        let db = with_catalogs(&["main"]).await;
        let main = ["main".to_string()];
        let client = dcr_client(&db.pool).await;
        let code = create(
            &db.pool,
            &new_code("c", Grants::Catalogs(&main), None),
            now(),
        )
        .await
        .unwrap();
        let grant = redeem(&db.pool, &code.id, &client, now()).await.unwrap();

        let enabled = key_rows::set_maintenance_report_by_id(&db.pool, &grant, true).await;
        let disabled = key_rows::set_maintenance_report_by_id(&db.pool, &grant, false).await;

        assert!(
            matches!(&enabled, Err(OkfError::Refused { reason }) if reason.contains("OAuth grant")),
            "{enabled:?}"
        );
        assert!(disabled.is_ok(), "turning it off is a no-op: {disabled:?}");
    }

    #[test]
    fn the_grant_name_uses_the_client_host_in_the_key_name_charset() {
        let client = |client_id: &str, kind: ClientKind, redirect: &str| ClientRecord {
            client_id: client_id.to_string(),
            kind,
            client_name: None,
            redirect_uris: vec![redirect.to_string()],
            metadata: serde_json::json!({}),
            fetched_at: None,
            cache_expires_at: None,
            created_at: now(),
        };
        let id = "1a2b3c4d-0000-4000-8000-000000000000";

        let cimd = client(
            "https://Client.Example.com:8443/meta.json",
            ClientKind::Cimd,
            "https://cb.example.com/x",
        );
        let dcr = client(
            "abc",
            ClientKind::Dcr,
            "http://user@127.0.0.1:3000/callback?x=1",
        );
        let ipv6 = client("abc", ClientKind::Dcr, "http://[::1]:3000/callback");
        let empty = client("abc", ClientKind::Dcr, "");

        assert_eq!(
            grant_name("laptop", &cimd, id),
            "laptop.client.example.com-1a2b3c4d"
        );
        assert_eq!(grant_name("laptop", &dcr, id), "laptop.127.0.0.1-1a2b3c4d");
        // "[::1]" maps to "---1-", trimmed to "1".
        assert_eq!(grant_name("laptop", &ipv6, id), "laptop.1-1a2b3c4d");
        assert_eq!(grant_name("laptop", &empty, id), "laptop.client-1a2b3c4d");
        assert_eq!(grant_name("laptop", &empty, "abc"), "laptop.client-abc");
        assert!(is_valid_name(&grant_name("laptop", &ipv6, id)));
    }

    #[test]
    fn expiry_defaults_to_a_day_and_is_capped() {
        let max = Duration::from_secs(7 * 24 * 60 * 60);

        assert_eq!(
            expiry(now(), DEFAULT_EXPIRY, max).unwrap(),
            now() + TimeDelta::hours(24)
        );
        assert_eq!(expiry(now(), max, max).unwrap(), now() + TimeDelta::days(7));
        assert!(matches!(
            expiry(now(), max + Duration::from_secs(1), max),
            Err(OkfError::Refused { .. })
        ));
        assert!(matches!(
            expiry(now(), Duration::ZERO, max),
            Err(OkfError::Refused { .. })
        ));
        assert!(matches!(
            expiry(now(), Duration::MAX, Duration::MAX),
            Err(OkfError::Refused { .. })
        ));
    }

    #[test]
    fn code_states_have_their_spellings() {
        let spelled: Vec<&str> = [
            CodeState::Live,
            CodeState::Expired,
            CodeState::Redeemed,
            CodeState::Revoked,
        ]
        .map(CodeState::as_str)
        .to_vec();

        assert_eq!(spelled, ["live", "expired", "redeemed", "revoked"]);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn prune_deletes_only_old_unredeemed_codes() {
        let db = test_db().await;
        catalogs::create(&db.pool, "main", "", crate::db::TEST_TEAM, now())
            .await
            .unwrap();
        let client = insert_client(
            &db.pool,
            "https://client.example.com/client.json",
            "Example",
            now(),
        )
        .await;
        let grant = insert_grant(&db.pool, &client, Access::Read, &["main"], now()).await;
        let old = now() - TimeDelta::days(8);
        for (byte, expires_at, grant_id) in [
            (1u8, old, None),
            (2, old, Some(grant.as_str())),
            (3, now() - TimeDelta::days(6), None),
            (4, now() + TimeDelta::hours(1), None),
        ] {
            sqlx::query(
                "INSERT INTO sign_in_codes \
                 (name, sha256, access, all_catalogs, actor, created_at, expires_at, redeemed_at, grant_id) \
                 VALUES ('laptop', $1, 'read', true, 'example-actor', $2, $3, $4, $5)",
            )
            .bind([byte; 32].as_slice())
            .bind(expires_at - TimeDelta::hours(24))
            .bind(expires_at)
            .bind(grant_id.map(|_| expires_at - TimeDelta::hours(1)))
            .bind(grant_id)
            .execute(&db.pool)
            .await
            .unwrap();
        }

        let pruned = prune(&db.pool, now() - TimeDelta::days(7)).await.unwrap();

        assert_eq!(pruned, 1);
        let left: Vec<Vec<u8>> =
            sqlx::query_scalar("SELECT sha256 FROM sign_in_codes ORDER BY sha256")
                .fetch_all(&db.pool)
                .await
                .unwrap();
        assert_eq!(left, [vec![2u8; 32], vec![3u8; 32], vec![4u8; 32]]);
    }

    /// The stored record of code `id`.
    async fn code_record(pool: &PgPool, id: &str) -> SignInCodeRecord {
        list(pool)
            .await
            .unwrap()
            .into_iter()
            .find(|record| record.id == id)
            .expect("the code exists")
    }

    /// Clears a code's user, as a code minted before migration 0009 has none.
    async fn make_legacy(pool: &PgPool, id: &str) {
        sqlx::query("UPDATE sign_in_codes SET user_id = NULL WHERE id = $1")
            .bind(id)
            .execute(pool)
            .await
            .unwrap();
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_code_belongs_to_its_user_and_an_unknown_user_is_unknown_name() {
        let db = test_db().await;
        let unknown = NewSignInCode {
            user: "nobody",
            ..new_code("other", Grants::All, None)
        };

        let code = create(&db.pool, &new_code("laptop", Grants::All, None), now())
            .await
            .unwrap();
        let refused = create(&db.pool, &unknown, now()).await;

        assert_eq!(
            code_record(&db.pool, &code.id).await.user.as_deref(),
            Some(crate::db::TEST_USER)
        );
        assert!(
            matches!(&refused, Err(OkfError::UnknownName { noun: "user", .. })),
            "{refused:?}"
        );
        assert_eq!(list(&db.pool).await.unwrap().len(), 1);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_redeemed_grant_inherits_its_codes_user() {
        let db = test_db().await;
        let client = dcr_client(&db.pool).await;
        let code = create(&db.pool, &new_code("laptop", Grants::All, None), now())
            .await
            .unwrap();

        let grant = redeem(&db.pool, &code.id, &client, now()).await.unwrap();

        let record = oauth_grants(&db.pool)
            .await
            .into_iter()
            .find(|record| record.id == grant)
            .unwrap();
        assert_eq!(record.user.as_deref(), Some(crate::db::TEST_USER));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_legacy_codes_grant_stays_unassigned() {
        let db = test_db().await;
        let client = dcr_client(&db.pool).await;
        let code = create(&db.pool, &new_code("laptop", Grants::All, None), now())
            .await
            .unwrap();
        make_legacy(&db.pool, &code.id).await;

        let grant = redeem(&db.pool, &code.id, &client, now()).await.unwrap();

        let record = oauth_grants(&db.pool)
            .await
            .into_iter()
            .find(|record| record.id == grant)
            .unwrap();
        assert_eq!(record.user, None);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_blank_actor_defaults_to_the_codes_name() {
        let db = test_db().await;
        let blank = NewSignInCode {
            actor: "  ",
            ..new_code("laptop", Grants::All, None)
        };

        let code = create(&db.pool, &blank, now()).await.unwrap();

        assert_eq!(code_record(&db.pool, &code.id).await.actor, "laptop");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn assign_user_sets_a_codes_user_without_bumping_the_generation() {
        let db = test_db().await;
        let code = create(&db.pool, &new_code("laptop", Grants::All, None), now())
            .await
            .unwrap();
        make_legacy(&db.pool, &code.id).await;
        let generation = catalogs::admin_generation(&db.pool).await.unwrap();

        assign_user(&db.pool, &code.id, crate::db::TEST_USER)
            .await
            .unwrap();

        assert_eq!(
            code_record(&db.pool, &code.id).await.user.as_deref(),
            Some(crate::db::TEST_USER)
        );
        assert_eq!(
            catalogs::admin_generation(&db.pool).await.unwrap(),
            generation,
            "a code is not in the key cache"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn assign_user_refuses_an_unknown_code_or_user() {
        let db = test_db().await;
        let code = create(&db.pool, &new_code("laptop", Grants::All, None), now())
            .await
            .unwrap();

        let no_code = assign_user(&db.pool, "no-such-id", crate::db::TEST_USER).await;
        let no_user = assign_user(&db.pool, &code.id, "nobody").await;

        assert!(
            matches!(&no_code, Err(OkfError::NotFound { path }) if path == "oauth/codes/no-such-id"),
            "{no_code:?}"
        );
        assert!(
            matches!(&no_user, Err(OkfError::UnknownName { noun: "user", .. })),
            "{no_user:?}"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_code_create_or_assign_that_waited_on_a_user_delete_is_unknown_name() {
        // Arrange: alice exists, a legacy code waits for a user, and a
        // transaction deletes alice and holds its commit.
        let db = test_db().await;
        let alice = users::NewUser {
            name: "alice",
            display_name: "",
        };
        users::create(&db.pool, &alice, now()).await.unwrap();
        let legacy = create(&db.pool, &new_code("old", Grants::All, None), now())
            .await
            .unwrap();
        make_legacy(&db.pool, &legacy.id).await;
        let mut deleting = db.pool.begin().await.unwrap();
        sqlx::query(
            "DELETE FROM teams WHERE personal_of = (SELECT id FROM users WHERE name = 'alice')",
        )
        .execute(&mut *deleting)
        .await
        .unwrap();
        sqlx::query("DELETE FROM users WHERE name = 'alice'")
            .execute(&mut *deleting)
            .await
            .unwrap();
        // `tokio::spawn` runs each concurrently, like a thread; their
        // `FOR KEY SHARE` reads of the user wait on the delete's row lock.
        let pool = db.pool.clone();
        let creating = tokio::spawn(async move {
            let new = NewSignInCode {
                user: "alice",
                ..new_code("laptop", Grants::All, None)
            };
            create(&pool, &new, now()).await
        });
        let pool = db.pool.clone();
        let id = legacy.id.clone();
        let assigning = tokio::spawn(async move { assign_user(&pool, &id, "alice").await });
        crate::store::catalogs::tests::until_locks_are_awaited(&db.pool, 2).await;

        deleting.commit().await.unwrap();
        let created = creating.await.unwrap();
        let assigned = assigning.await.unwrap();

        // Not foreign-key violations (500s): the user is simply unknown.
        assert!(
            matches!(&created, Err(OkfError::UnknownName { noun: "user", .. })),
            "{created:?}"
        );
        assert!(
            matches!(&assigned, Err(OkfError::UnknownName { noun: "user", .. })),
            "{assigned:?}"
        );
        assert_eq!(list(&db.pool).await.unwrap().len(), 1);
        assert_eq!(code_record(&db.pool, &legacy.id).await.user, None);
    }
}

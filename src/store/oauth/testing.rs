//! Test helpers for the OAuth store (`#[cfg(test)]`): rows a test needs
//! before the endpoints that make them exist, written with plain SQL.

use chrono::{DateTime, TimeDelta, Utc};
use sqlx::PgPool;

use crate::app::App;
use crate::config::Config;
use crate::keys;
use crate::store::keys::Access;
use crate::store::oauth::tokens::{self, NewToken, TokenKind};

/// Stores a Client ID Metadata Document client whose one redirect URI is
/// `https://client.example.com/callback`.
///
/// # Arguments
/// * `pool` - the database.
/// * `client_id` - the document URL (the client's id).
/// * `client_name` - its display name.
/// * `now` - its creation and fetch time.
///
/// # Returns
/// The client id.
///
/// # Panics
/// If the insert fails: a broken test setup.
pub(crate) async fn insert_client(
    pool: &PgPool,
    client_id: &str,
    client_name: &str,
    now: DateTime<Utc>,
) -> String {
    sqlx::query(
        "INSERT INTO oauth_clients \
         (client_id, kind, client_name, redirect_uris, metadata, fetched_at, cache_expires_at, \
          created_at) \
         VALUES ($1, 'cimd', $2, ARRAY['https://client.example.com/callback'], \
                 jsonb_build_object('client_id', $1::text, 'client_name', $2::text), $3, \
                 $3 + interval '1 hour', $3)",
    )
    .bind(client_id)
    .bind(client_name)
    .bind(now)
    .execute(pool)
    .await
    .expect("insert a test client");
    client_id.to_string()
}

/// Stores a live OAuth grant for `client_id` and bumps the administrative
/// generation, as redeeming a sign-in code will (Task 4), so the next
/// `App::poll` loads it.
///
/// # Arguments
/// * `pool` - the database.
/// * `client_id` - its client (already stored).
/// * `access` - `Read` or `Write` (the schema refuses `Admin`).
/// * `catalogs` - the catalogs it reaches (each must exist).
/// * `now` - its creation time.
///
/// # Returns
/// The grant's id.
///
/// # Panics
/// If an insert fails: a broken test setup.
pub(crate) async fn insert_grant(
    pool: &PgPool,
    client_id: &str,
    access: Access,
    catalogs: &[&str],
    now: DateTime<Utc>,
) -> String {
    let mut tx = pool.begin().await.expect("begin");
    let id: String = sqlx::query_scalar(
        "INSERT INTO api_keys (name, access, all_catalogs, actor, created_at, kind, client_id) \
         VALUES ('laptop', $1, false, 'example-actor', $2, 'oauth', $3) RETURNING id",
    )
    .bind(access.as_str())
    .bind(now)
    .bind(client_id)
    .fetch_one(&mut *tx)
    .await
    .expect("insert a test grant");
    sqlx::query(
        "INSERT INTO key_catalogs (key_id, catalog_id) \
         SELECT $1, id FROM catalogs WHERE name = ANY($2)",
    )
    .bind(&id)
    .bind(catalogs)
    .execute(&mut *tx)
    .await
    .expect("grant the test grant its catalogs");
    crate::store::catalogs::bump_admin_generation(&mut tx)
        .await
        .expect("bump the generation");
    tx.commit().await.expect("commit");
    id
}

/// The defaults with OAuth on at `public_url`, its host (and loopback) allowed.
///
/// # Arguments
/// * `public_url` - e.g. `https://okf.example.com`.
///
/// # Returns
/// A configuration `Config::validate` accepts.
///
/// # Panics
/// If `public_url` has no host, or the result does not validate.
pub(crate) fn oauth_config(public_url: &str) -> Config {
    let host = public_url
        .parse::<axum::http::Uri>()
        .ok()
        .and_then(|uri| uri.host().map(str::to_string))
        .expect("public_url has a host");
    let config = Config {
        public_url: Some(public_url.to_string()),
        allowed_hosts: vec![host, "localhost".into(), "127.0.0.1".into()],
        ..Config::default()
    };
    assert_eq!(
        config.validate(),
        Ok(()),
        "a valid OAuth test configuration"
    );
    config
}

/// Stores a new access token for `grant_id`, issued for `resource`, valid
/// for `ttl` from the app's clock.
///
/// # Arguments
/// * `app` - the app (its pool and clock).
/// * `grant_id` - the grant.
/// * `resource` - the audience.
/// * `ttl` - how long it is valid; a negative one makes an already expired
///   token, created twice that long ago so `expires_at > created_at` holds.
///
/// # Returns
/// The `okfat_` token.
///
/// # Panics
/// If the insert fails.
pub(crate) async fn mint_access(
    app: &App,
    grant_id: &str,
    resource: &str,
    ttl: TimeDelta,
) -> String {
    let token = keys::generate_with(keys::ACCESS_TOKEN_PREFIX).expect("random token");
    let now = app.clock().now();
    let created = if ttl > TimeDelta::zero() {
        now
    } else {
        now + ttl * 2
    };
    let mut conn = app.pool().acquire().await.expect("a connection");
    tokens::insert(
        &mut conn,
        &NewToken {
            sha256: keys::hash(&token),
            grant_id,
            kind: TokenKind::Access,
            family_id: "test-family",
            resource,
            expires_at: now + ttl,
        },
        created,
    )
    .await
    .expect("store a test access token");
    token
}

//! API-key authentication (design "Data flow", "Read", step 1): the
//! in-memory key cache, the [`Principal`] a request acts as, and the axum
//! middleware that requires a key on every route but `/healthz`.
//!
//! Keys are looked up by their SHA-256 in a `HashMap`. A timing difference
//! in that lookup can tell an attacker only how the *hash* of a guess
//! compares with stored hashes, which does not help find a key, so there is
//! no constant-time scan (the design's deviation, recorded in the M4 plan).

use std::collections::{BTreeSet, HashMap};
use std::sync::{Arc, RwLock};

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use sqlx::PgPool;

use crate::error::OkfError;
use crate::keys;
use crate::store::keys::{self as key_rows, Access, KeyRecord};

/// The catalogs a key may reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// Every catalog, present and future (`key create --all`).
    All,
    /// These catalogs, by name.
    Catalogs(BTreeSet<String>),
}

/// Who a request acts as: one live API key (never the key itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// The key's id.
    pub key_id: String,
    /// What it may do.
    pub access: Access,
    /// Which catalogs it reaches.
    pub reach: Reach,
    /// Its default catalog (design "Catalog selection", step 3).
    pub default_catalog: Option<String>,
    /// The actor its writes are recorded as.
    pub actor: String,
}

impl Principal {
    /// Does the key reach this catalog?
    ///
    /// # Arguments
    /// * `catalog` - a catalog name.
    ///
    /// # Returns
    /// `true` for a key granted every catalog, or this one.
    pub fn reaches(&self, catalog: &str) -> bool {
        match &self.reach {
            Reach::All => true,
            Reach::Catalogs(names) => names.contains(catalog),
        }
    }

    /// Checks that the key may do something needing `access` to `catalog`.
    ///
    /// # Arguments
    /// * `access` - the level the operation needs.
    /// * `catalog` - the catalog it acts on.
    ///
    /// # Errors
    /// [`OkfError::CatalogNotSelected`] if the key does not reach the
    /// catalog (worded like an unknown catalog, so a key cannot probe which
    /// catalogs exist); [`OkfError::Forbidden`] if it does but its level is
    /// lower than `access`.
    pub fn require(&self, access: Access, catalog: &str) -> Result<(), OkfError> {
        if !self.reaches(catalog) {
            return Err(OkfError::CatalogNotSelected {
                catalog: catalog.to_string(),
            });
        }
        // `Access` derives `Ord` in declaration order: Read < Write < Admin.
        if self.access < access {
            return Err(OkfError::Forbidden {
                reason: format!(
                    "key {:?} has {} access; this needs {}",
                    self.key_id,
                    self.access.as_str(),
                    access.as_str()
                ),
            });
        }
        Ok(())
    }
}

// `From` is Rust's conversion trait: `Principal::from(record)` (or
// `record.into()`) builds a principal from a stored key.
impl From<KeyRecord> for Principal {
    fn from(record: KeyRecord) -> Self {
        let reach = if record.all_catalogs {
            Reach::All
        } else {
            Reach::Catalogs(record.catalogs.into_iter().collect())
        };
        Self {
            key_id: record.id,
            access: record.access,
            reach,
            default_catalog: record.default_catalog,
            actor: record.actor,
        }
    }
}

/// Every live key, by SHA-256. Immutable: a reload builds a new one.
#[derive(Debug, Default)]
pub struct KeyCache {
    by_hash: HashMap<[u8; 32], Arc<Principal>>,
}

impl KeyCache {
    /// Builds the cache from stored keys, leaving out revoked ones.
    ///
    /// # Arguments
    /// * `records` - the keys, as [`key_rows::list`] returns them.
    ///
    /// # Returns
    /// The cache.
    pub fn from_records(records: Vec<KeyRecord>) -> Self {
        let by_hash = records
            .into_iter()
            .filter(|record| record.revoked_at.is_none())
            .filter_map(|record| {
                // The schema checks `length(sha256) = 32`, so this fails only
                // on a row edited by hand; such a key cannot authenticate.
                let hash: [u8; 32] = record.sha256.as_slice().try_into().ok()?;
                Some((hash, Arc::new(Principal::from(record))))
            })
            .collect();
        Self { by_hash }
    }

    /// The principal of a presented key.
    ///
    /// # Arguments
    /// * `presented` - the key from the request.
    ///
    /// # Returns
    /// Its principal, or `None` for an unknown or revoked key.
    pub fn find(&self, presented: &str) -> Option<Arc<Principal>> {
        self.by_hash.get(&keys::hash(presented)).cloned()
    }
}

/// The key cache the server shares, replaced whole on reload.
#[derive(Debug)]
pub struct Keys {
    // As in `CatalogState`: a `std` lock held only to clone or swap the
    // `Arc`, never across an `.await`.
    cache: RwLock<Arc<KeyCache>>,
}

impl Keys {
    /// Wraps a cache.
    ///
    /// # Arguments
    /// * `cache` - the keys to start with.
    ///
    /// # Returns
    /// The shared cache.
    pub fn new(cache: KeyCache) -> Self {
        Self {
            cache: RwLock::new(Arc::new(cache)),
        }
    }

    /// Loads every live key from the database.
    ///
    /// # Arguments
    /// * `pool` - the database.
    ///
    /// # Returns
    /// The shared cache.
    ///
    /// # Errors
    /// [`OkfError::Database`].
    pub async fn load(pool: &PgPool) -> Result<Self, OkfError> {
        Ok(Self::new(KeyCache::from_records(
            key_rows::list(pool).await?,
        )))
    }

    /// Re-reads every key and swaps the new cache in.
    ///
    /// # Arguments
    /// * `pool` - the database.
    ///
    /// # Errors
    /// [`OkfError::Database`]; the old cache stays in place.
    pub async fn reload(&self, pool: &PgPool) -> Result<(), OkfError> {
        let cache = Arc::new(KeyCache::from_records(key_rows::list(pool).await?));
        *self.cache.write().expect("key cache lock poisoned") = cache;
        Ok(())
    }

    /// The principal of a presented key, from the current cache.
    ///
    /// # Arguments
    /// * `presented` - the key from the request.
    ///
    /// # Returns
    /// Its principal, or `None` for an unknown or revoked key.
    pub fn find(&self, presented: &str) -> Option<Arc<Principal>> {
        self.cache
            .read()
            .expect("key cache lock poisoned")
            .find(presented)
    }
}

/// The token of an `Authorization: Bearer <token>` header.
///
/// # Arguments
/// * `headers` - the request's headers.
///
/// # Returns
/// The token (trimmed), or `None` without a header, with another scheme
/// (the scheme is matched case-insensitively, as RFC 6750 allows) or with an
/// empty token.
pub fn bearer(headers: &HeaderMap) -> Option<&str> {
    let value = headers.get(header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = value.split_once(' ')?;
    let token = token.trim();
    // `then_some` turns a `bool` into `Some(value)` or `None`.
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

/// axum middleware: admits a request with a live key, inserting its
/// `Arc<Principal>` into the request's extensions, and answers anything
/// else with 401.
///
/// # Arguments
/// * `keys` - the shared key cache (the layer's state).
/// * `request` - the incoming request.
/// * `next` - the rest of the stack.
///
/// # Returns
/// The route's response, or 401 with `WWW-Authenticate: Bearer`.
// Used as `axum::middleware::from_fn_with_state(keys, require_key)`; axum
// fills each argument from the request (the extractor pattern).
pub async fn require_key(
    State(keys): State<Arc<Keys>>,
    mut request: Request,
    next: Next,
) -> Response {
    let principal = bearer(request.headers()).and_then(|key| keys.find(key));
    let Some(principal) = principal else {
        return (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, "Bearer")],
            "missing or unknown API key\n",
        )
            .into_response();
    };
    tracing::debug!(key = %principal.key_id, "authenticated");
    request.extensions_mut().insert(principal);
    next.run(request).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::get;
    use axum::Router;
    use chrono::Utc;
    // `oneshot` sends one request through a router without a socket.
    use tower::ServiceExt;

    fn record(id: &str, key: &str, revoked: bool) -> KeyRecord {
        KeyRecord {
            id: id.to_string(),
            name: id.to_string(),
            sha256: keys::hash(key).to_vec(),
            access: Access::Read,
            all_catalogs: false,
            catalogs: vec!["pg".to_string()],
            default_catalog: Some("pg".to_string()),
            actor: format!("actor-{id}"),
            created_at: Utc::now(),
            revoked_at: revoked.then(Utc::now),
        }
    }

    fn headers(value: &str) -> HeaderMap {
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, value.parse().unwrap());
        headers
    }

    #[test]
    fn bearer_reads_the_token_with_any_scheme_case() {
        assert_eq!(bearer(&headers("Bearer reader-key")), Some("reader-key"));
        assert_eq!(bearer(&headers("bearer  reader-key ")), Some("reader-key"));
        assert_eq!(bearer(&headers("Basic reader-key")), None);
        assert_eq!(bearer(&headers("Bearer ")), None);
        assert_eq!(bearer(&headers("reader-key")), None);
        assert_eq!(bearer(&HeaderMap::new()), None);
    }

    #[test]
    fn the_cache_finds_live_keys_only() {
        let cache = KeyCache::from_records(vec![
            record("reader", "reader-key", false),
            record("old", "old-key", true),
            KeyRecord {
                sha256: vec![0; 5],
                ..record("short", "short-key", false)
            },
        ]);

        let found = cache.find("reader-key").expect("live key");

        assert_eq!(found.key_id, "reader");
        assert_eq!(found.actor, "actor-reader");
        assert_eq!(found.default_catalog.as_deref(), Some("pg"));
        assert!(cache.find("old-key").is_none());
        // A stored hash of the wrong length is left out, not a panic.
        assert!(cache.find("short-key").is_none());
        assert_eq!(cache.by_hash.len(), 1);
        assert!(cache.find("no-such-key").is_none());
    }

    #[test]
    fn require_checks_reach_before_access() {
        let principal = Principal::from(record("reader", "reader-key", false));

        assert!(principal.require(Access::Read, "pg").is_ok());
        assert!(matches!(
            principal.require(Access::Write, "pg"),
            Err(OkfError::Forbidden { .. })
        ));
        assert!(matches!(
            principal.require(Access::Admin, "pg"),
            Err(OkfError::Forbidden { .. })
        ));
        // A write key may write and read, but not administer.
        let writer = Principal {
            access: Access::Write,
            ..principal.clone()
        };
        assert!(writer.require(Access::Write, "pg").is_ok());
        assert!(writer.require(Access::Read, "pg").is_ok());
        assert!(matches!(
            writer.require(Access::Admin, "pg"),
            Err(OkfError::Forbidden { .. })
        ));
        // A catalog the key does not reach is "not available", whatever the access.
        assert!(matches!(
            principal.require(Access::Read, "work"),
            Err(OkfError::CatalogNotSelected { catalog }) if catalog == "work"
        ));
        let mut everything = record("all", "all-key", false);
        everything.all_catalogs = true;
        everything.catalogs.clear();
        assert!(Principal::from(everything).reaches("work"));
    }

    /// A router whose one route echoes the principal's key id.
    fn app(cache: KeyCache) -> Router {
        let keys = Arc::new(Keys::new(cache));
        Router::new()
            .route(
                "/",
                get(|request: Request| async move {
                    request
                        .extensions()
                        .get::<Arc<Principal>>()
                        .map(|principal| principal.key_id.clone())
                        .unwrap_or_default()
                }),
            )
            .route_layer(axum::middleware::from_fn_with_state(keys, require_key))
    }

    /// The response's status, its `WWW-Authenticate` header and its body.
    async fn status_and_body(
        router: Router,
        authorization: Option<&str>,
    ) -> (StatusCode, Option<String>, String) {
        let mut request = axum::http::Request::builder().uri("/");
        if let Some(value) = authorization {
            request = request.header(header::AUTHORIZATION, value);
        }
        let response = router
            .oneshot(request.body(Body::empty()).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let challenge = response
            .headers()
            .get(header::WWW_AUTHENTICATE)
            .map(|value| value.to_str().unwrap().to_string());
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 16)
            .await
            .unwrap();
        (
            status,
            challenge,
            String::from_utf8(bytes.to_vec()).unwrap(),
        )
    }

    #[tokio::test]
    async fn the_middleware_admits_a_known_key_and_refuses_the_rest() {
        let cache = || {
            KeyCache::from_records(vec![
                record("reader", "reader-key", false),
                record("old", "old-key", true),
            ])
        };

        let admitted = status_and_body(app(cache()), Some("Bearer reader-key")).await;
        let refused = [
            status_and_body(app(cache()), Some("Bearer other-key")).await,
            status_and_body(app(cache()), Some("Bearer old-key")).await,
            status_and_body(app(cache()), Some("Basic reader-key")).await,
            status_and_body(app(cache()), None).await,
        ];

        assert_eq!(admitted, (StatusCode::OK, None, "reader".to_string()));
        for (status, challenge, _) in refused {
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            assert_eq!(challenge.as_deref(), Some("Bearer"));
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn keys_load_from_the_database_and_reload_after_a_revoke() {
        use crate::store::{catalogs, keys as key_rows};
        let db = crate::db::test_db().await;
        catalogs::create(&db.pool, "pg", "", Utc::now())
            .await
            .unwrap();
        let names = ["pg".to_string()];
        let new = key_rows::NewKey {
            id: "reader",
            access: Access::Read,
            grants: key_rows::Grants::Catalogs(&names),
            default_catalog: Some("pg"),
            actor: "tester",
        };
        let created = key_rows::create_with_id(&db.pool, &new, Utc::now())
            .await
            .unwrap();
        let key = created.key;

        let loaded = Keys::load(&db.pool).await.unwrap();
        assert_eq!(loaded.find(&key).unwrap().key_id, created.id);
        key_rows::revoke(&db.pool, "reader", Utc::now())
            .await
            .unwrap();
        // The cache keeps the key until it is reloaded (the poll loop does that).
        assert!(loaded.find(&key).is_some());
        loaded.reload(&db.pool).await.unwrap();

        assert!(loaded.find(&key).is_none());
        // A reload that fails keeps the cache it had.
        let other = key_rows::create_with_id(
            &db.pool,
            &key_rows::NewKey { id: "other", ..new },
            Utc::now(),
        )
        .await
        .unwrap();
        loaded.reload(&db.pool).await.unwrap();
        db.pool.close().await;
        assert!(loaded.reload(&db.pool).await.is_err());
        assert_eq!(loaded.find(&other.key).unwrap().key_id, other.id);
    }
}

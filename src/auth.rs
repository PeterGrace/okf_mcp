//! API-key and OAuth-token authentication (design "Data flow", "Read", step
//! 1): the in-memory key cache, the [`Principal`] a request acts as, the
//! [`BearerVerifier`] seam between credential kinds, and the axum
//! middleware that requires a credential on every route but `/healthz`.
//!
//! Keys are looked up by their SHA-256 in a `HashMap`. A timing difference
//! in that lookup can tell an attacker only how the *hash* of a guess
//! compares with stored hashes, which does not help find a key, so there is
//! no constant-time scan (the design's deviation, recorded in the M4 plan).

use std::collections::{BTreeMap, HashMap};
use std::fmt;
// `Future` is not in the 2021 prelude; the trait below names it.
use std::future::Future;
use std::sync::{Arc, RwLock};

use axum::extract::{Request, State};
use axum::http::{header, HeaderMap, HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use chrono::{DateTime, Utc};
use sqlx::PgPool;

use crate::app::App;
use crate::clock::Clock;
use crate::error::OkfError;
use crate::http::AppError;
use crate::keys;
use crate::oauth::access::OAuthVerifier;
use crate::store::keys::{self as key_rows, Access, KeyKind, KeyRecord};

/// The catalogs a credential may reach, and at what level.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Reach {
    /// Every catalog, present and future (`key create --all`), at the
    /// principal's own `access`.
    All,
    /// These catalogs, by name, each at its own level (Decision 1).
    Catalogs(BTreeMap<String, Access>),
}

/// What credential a request came with.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CredentialKind {
    /// An `okf_` API key.
    ApiKey,
    /// An OAuth access token of a grant.
    OAuth {
        /// The grant's client id (`None` once the client row is gone).
        client_id: Option<String>,
        /// The grant's client, as it names itself.
        client_name: Option<String>,
    },
}

/// Who a request acts as: one live API key or OAuth grant (never the
/// secret itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Principal {
    /// The key's id.
    pub key_id: String,
    /// The credential's highest level, for admin checks and display; what
    /// it may do in a catalog is [`Principal::level`]. An admin credential
    /// has admin on every catalog it reaches (the store refuses lower
    /// per-catalog levels on an admin key), so `access == Admin` never
    /// widens a catalog.
    pub access: Access,
    /// Independent opt-in to submit maintenance reports.
    pub maintenance_report: bool,
    /// Which catalogs it reaches.
    pub reach: Reach,
    /// Its default catalog (design "Catalog selection", step 3).
    pub default_catalog: Option<String>,
    /// The actor its writes are recorded as.
    pub actor: String,
    /// An API key or an OAuth grant.
    pub kind: CredentialKind,
    /// The user this credential belongs to; `None` for one not yet
    /// assigned. Display only: it grants nothing, so `reaches` and
    /// `require` never read it.
    pub user: Option<String>,
}

impl Principal {
    /// The level this principal has on `catalog`.
    ///
    /// # Arguments
    /// * `catalog` - a catalog name.
    ///
    /// # Returns
    /// Its level there, or `None` when it does not reach the catalog.
    pub fn level(&self, catalog: &str) -> Option<Access> {
        match &self.reach {
            Reach::All => Some(self.access),
            // `copied` turns the map's `Option<&Access>` into
            // `Option<Access>` (`Access` is `Copy`).
            Reach::Catalogs(levels) => levels.get(catalog).copied(),
        }
    }

    /// Does the key reach this catalog?
    ///
    /// # Arguments
    /// * `catalog` - a catalog name.
    ///
    /// # Returns
    /// `true` for a key granted every catalog, or this one.
    pub fn reaches(&self, catalog: &str) -> bool {
        self.level(catalog).is_some()
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
    /// catalogs exist); [`OkfError::Forbidden`] if it does but its level
    /// there is lower than `access`.
    pub fn require(&self, access: Access, catalog: &str) -> Result<(), OkfError> {
        // `let ... else`: bind the level, or leave early when there is none.
        let Some(level) = self.level(catalog) else {
            return Err(OkfError::CatalogNotSelected {
                catalog: catalog.to_string(),
            });
        };
        // `Access` derives `Ord` in declaration order: Read < Write < Admin.
        if level < access {
            return Err(OkfError::Forbidden {
                reason: format!(
                    "key {:?} has {} access to {catalog:?}; this needs {}",
                    self.key_id,
                    level.as_str(),
                    access.as_str()
                ),
            });
        }
        Ok(())
    }

    /// Requires read access to the catalog and the independent reporting flag.
    ///
    /// # Errors
    /// As [`Self::require`], or [`OkfError::Forbidden`] when reporting is disabled.
    pub fn require_maintenance_report(&self, catalog: &str) -> Result<(), OkfError> {
        self.require(Access::Read, catalog)?;
        if !self.maintenance_report {
            return Err(OkfError::Forbidden {
                reason: format!("key {:?} cannot submit maintenance reports", self.key_id),
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
            Reach::Catalogs(record.catalog_access)
        };
        let kind = match record.kind {
            KeyKind::Key => CredentialKind::ApiKey,
            // Moving the two fields out of `record` is fine: the struct
            // literal below moves only its other fields.
            KeyKind::OAuth => CredentialKind::OAuth {
                client_id: record.client_id,
                client_name: record.client_name,
            },
        };
        Self {
            key_id: record.id,
            access: record.access,
            maintenance_report: record.maintenance_report,
            reach,
            default_catalog: record.default_catalog,
            actor: record.actor,
            kind,
            user: record.user,
        }
    }
}

/// Whether a credential with this expiry still works at `now`.
///
/// # Arguments
/// * `expires_at` - its expiry; `None` never expires.
/// * `now` - the server clock.
///
/// # Returns
/// `true` before the expiry (or with none).
fn unexpired(expires_at: Option<DateTime<Utc>>, now: DateTime<Utc>) -> bool {
    // `is_none_or`: true for `None`, else the test's answer.
    expires_at.is_none_or(|at| at > now)
}

/// One cached credential and when it stops working.
#[derive(Debug)]
struct Cached {
    principal: Arc<Principal>,
    expires_at: Option<DateTime<Utc>>,
}

impl Cached {
    /// The principal, unless the credential has expired since the cache
    /// was built (it can be up to one `poll_interval` old).
    fn live_at(&self, now: DateTime<Utc>) -> Option<Arc<Principal>> {
        // `then` runs the closure only when the test is true.
        unexpired(self.expires_at, now).then(|| Arc::clone(&self.principal))
    }
}

/// Every live key, by SHA-256, and every live OAuth grant, by id.
/// Immutable: a reload builds a new one.
#[derive(Debug, Default)]
pub struct KeyCache {
    by_hash: HashMap<[u8; 32], Cached>,
    // Grants have no secret of their own: an access token names one by id.
    by_id: HashMap<String, Cached>,
}

impl KeyCache {
    /// Builds the cache from stored rows, leaving out revoked ones and
    /// those already expired.
    ///
    /// # Arguments
    /// * `records` - the rows, as [`key_rows::list`] returns them.
    /// * `now` - the server clock.
    ///
    /// # Returns
    /// The cache.
    pub fn from_records(records: Vec<KeyRecord>, now: DateTime<Utc>) -> Self {
        let mut cache = Self::default();
        for record in records
            .into_iter()
            .filter(|record| record.revoked_at.is_none() && unexpired(record.expires_at, now))
        {
            let expires_at = record.expires_at;
            match record.kind {
                KeyKind::Key => {
                    // The schema checks `length(sha256) = 32`, so this fails
                    // only on a row edited by hand; such a key cannot
                    // authenticate. `let ... else` skips it.
                    let Some(hash) = record
                        .sha256
                        .as_deref()
                        .and_then(|sha256| <[u8; 32]>::try_from(sha256).ok())
                    else {
                        continue;
                    };
                    let principal = Arc::new(Principal::from(record));
                    cache.by_hash.insert(
                        hash,
                        Cached {
                            principal,
                            expires_at,
                        },
                    );
                }
                KeyKind::OAuth => {
                    let id = record.id.clone();
                    let principal = Arc::new(Principal::from(record));
                    cache.by_id.insert(
                        id,
                        Cached {
                            principal,
                            expires_at,
                        },
                    );
                }
            }
        }
        cache
    }

    /// Reads every key and builds the cache.
    ///
    /// # Errors
    /// [`OkfError::Database`].
    async fn read(pool: &PgPool, now: DateTime<Utc>) -> Result<Self, OkfError> {
        Ok(Self::from_records(key_rows::list(pool).await?, now))
    }

    /// The principal of a presented key.
    ///
    /// # Arguments
    /// * `presented` - the key from the request.
    /// * `now` - the server clock.
    ///
    /// # Returns
    /// Its principal, or `None` for an unknown, revoked or expired key.
    pub fn find(&self, presented: &str, now: DateTime<Utc>) -> Option<Arc<Principal>> {
        self.by_hash
            .get(&keys::hash(presented))
            .and_then(|cached| cached.live_at(now))
    }

    /// A live OAuth grant's principal.
    ///
    /// # Arguments
    /// * `grant_id` - the grant's id (from its access token).
    /// * `now` - the server clock.
    ///
    /// # Returns
    /// Its principal, or `None` for an unknown, revoked or expired grant.
    pub fn find_grant(&self, grant_id: &str, now: DateTime<Utc>) -> Option<Arc<Principal>> {
        self.by_id
            .get(grant_id)
            .and_then(|cached| cached.live_at(now))
    }
}

/// The key cache the server shares, replaced whole on reload. It holds the
/// server clock, so a lookup refuses a key that expired since the load.
pub struct Keys {
    // As in `CatalogState`: a `std` lock held only to clone or swap the
    // `Arc`, never across an `.await`.
    cache: RwLock<Arc<KeyCache>>,
    clock: Arc<dyn Clock>,
}

// By hand: `dyn Clock` has no `Debug`. `finish_non_exhaustive` prints `..`
// for the field left out.
impl fmt::Debug for Keys {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("Keys")
            .field("cache", &self.cache)
            .finish_non_exhaustive()
    }
}

impl Keys {
    /// Wraps a cache.
    ///
    /// # Arguments
    /// * `cache` - the keys to start with.
    /// * `clock` - the server clock, for expiry at lookup.
    ///
    /// # Returns
    /// The shared cache.
    pub fn new(cache: KeyCache, clock: Arc<dyn Clock>) -> Self {
        Self {
            cache: RwLock::new(Arc::new(cache)),
            clock,
        }
    }

    /// Loads every live key from the database.
    ///
    /// # Arguments
    /// * `pool` - the database.
    /// * `clock` - the server clock.
    ///
    /// # Returns
    /// The shared cache.
    ///
    /// # Errors
    /// [`OkfError::Database`].
    pub async fn load(pool: &PgPool, clock: Arc<dyn Clock>) -> Result<Self, OkfError> {
        let cache = KeyCache::read(pool, clock.now()).await?;
        Ok(Self::new(cache, clock))
    }

    /// Re-reads every key and swaps the new cache in.
    ///
    /// # Arguments
    /// * `pool` - the database.
    ///
    /// # Errors
    /// [`OkfError::Database`]; the old cache stays in place.
    pub async fn reload(&self, pool: &PgPool) -> Result<(), OkfError> {
        let cache = Arc::new(KeyCache::read(pool, self.clock.now()).await?);
        *self.cache.write().expect("key cache lock poisoned") = cache;
        Ok(())
    }

    /// The principal of a presented key, from the current cache.
    ///
    /// # Arguments
    /// * `presented` - the key from the request.
    ///
    /// # Returns
    /// Its principal, or `None` for an unknown, revoked or expired key.
    pub fn find(&self, presented: &str) -> Option<Arc<Principal>> {
        // The clock is read before the lock is taken.
        let now = self.clock.now();
        self.cache
            .read()
            .expect("key cache lock poisoned")
            .find(presented, now)
    }

    /// A live OAuth grant's principal, from the current cache.
    ///
    /// # Arguments
    /// * `grant_id` - the grant's id.
    ///
    /// # Returns
    /// Its principal, or `None` for an unknown, revoked or expired grant (a
    /// revoke takes effect at the next reload, within `poll_interval`; an
    /// expiry at once).
    pub fn find_grant(&self, grant_id: &str) -> Option<Arc<Principal>> {
        let now = self.clock.now();
        self.cache
            .read()
            .expect("key cache lock poisoned")
            .find_grant(grant_id, now)
    }
}

/// Turns a presented bearer token into the principal it acts as. The seam
/// for credential kinds: API keys ([`Keys`]), OAuth access tokens
/// ([`OAuthVerifier`]) and, in a later round, an external issuer.
// `-> impl Future<...> + Send`: the method returns some future that can move
// between threads (tokio's worker threads need that), without naming its
// type; an implementation may simply be an `async fn`.
pub trait BearerVerifier {
    /// The principal of `token`.
    ///
    /// # Arguments
    /// * `token` - the bearer token, as presented.
    ///
    /// # Returns
    /// The principal, or `None` for an unknown, expired, revoked or
    /// wrong-audience token.
    ///
    /// # Errors
    /// [`OkfError::Database`] when the lookup needs the database and it fails.
    fn verify(
        &self,
        token: &str,
    ) -> impl Future<Output = Result<Option<Arc<Principal>>, OkfError>> + Send;
}

impl BearerVerifier for Keys {
    async fn verify(&self, token: &str) -> Result<Option<Arc<Principal>>, OkfError> {
        Ok(self.find(token))
    }
}

/// The principal of a bearer token. Dispatch is by prefix with no fallback
/// between kinds: an `okfat_` token is only ever an OAuth access token
/// (`None` with OAuth off), and anything else is looked up only among the
/// API keys, which all carry `okf_`. Refresh tokens, sign-in codes and
/// authorization codes are never bearer credentials and match nothing.
///
/// # Arguments
/// * `app` - the shared state.
/// * `token` - the bearer token.
///
/// # Returns
/// The principal, or `None`.
///
/// # Errors
/// [`OkfError::Database`] from an access-token lookup.
pub async fn authenticate(app: &App, token: &str) -> Result<Option<Arc<Principal>>, OkfError> {
    if token.starts_with(keys::ACCESS_TOKEN_PREFIX) {
        return match app.oauth() {
            Some(oauth) => OAuthVerifier { app, oauth }.verify(token).await,
            None => Ok(None),
        };
    }
    app.keys().verify(token).await
}

/// The `WWW-Authenticate` value of a 401: with OAuth on, it names the
/// Protected Resource Metadata (RFC 9728 section 5.1), so a client can
/// discover the authorization server.
///
/// # Arguments
/// * `app` - the shared state.
///
/// # Returns
/// `Bearer`, or `Bearer resource_metadata="<public_url>/.well-known/oauth-protected-resource"`.
pub fn challenge(app: &App) -> HeaderValue {
    app.oauth().map_or_else(
        || HeaderValue::from_static("Bearer"),
        |oauth| oauth.challenge.clone(),
    )
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

/// axum middleware: admits a request with a live API key or OAuth access
/// token, inserting its `Arc<Principal>` into the request's extensions, and
/// answers anything else with 401 and [`challenge`].
///
/// # Arguments
/// * `app` - the shared state (the layer's state).
/// * `request` - the incoming request.
/// * `next` - the rest of the stack.
///
/// # Returns
/// The route's response; 401 with the challenge; or 500 if an access-token
/// lookup fails (logged).
// Used as `axum::middleware::from_fn_with_state(app, require_key)`; axum
// fills each argument from the request (the extractor pattern).
pub async fn require_key(
    State(app): State<Arc<App>>,
    mut request: Request,
    next: Next,
) -> Response {
    // The borrowed token (`&str` into the headers) ends with this `match`,
    // before the request is changed below.
    let found = match bearer(request.headers()) {
        Some(token) => authenticate(&app, token).await,
        None => Ok(None),
    };
    match found {
        Ok(Some(principal)) => {
            tracing::debug!(key = %principal.key_id, "authenticated");
            request.extensions_mut().insert(principal);
            next.run(request).await
        }
        Ok(None) => (
            StatusCode::UNAUTHORIZED,
            [(header::WWW_AUTHENTICATE, challenge(&app))],
            "missing or unknown API key\n",
        )
            .into_response(),
        // `AppError` logs it and answers 500 without detail.
        Err(error) => AppError(error).into_response(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::routing::get;
    use axum::Router;
    use chrono::Utc;

    use crate::clock::FixedClock;
    use crate::config::Config;
    // `oneshot` sends one request through a router without a socket.
    use tower::ServiceExt;

    /// The catalog-to-level map of a record granted `names` at `access`.
    fn levels(names: &[&str], access: Access) -> BTreeMap<String, Access> {
        names
            .iter()
            .map(|name| (name.to_string(), access))
            .collect()
    }

    fn record(id: &str, key: &str, revoked: bool) -> KeyRecord {
        KeyRecord {
            id: id.to_string(),
            name: id.to_string(),
            sha256: Some(keys::hash(key).to_vec()),
            access: Access::Read,
            maintenance_report: false,
            all_catalogs: false,
            catalogs: vec!["pg".to_string()],
            catalog_access: levels(&["pg"], Access::Read),
            default_catalog: Some("pg".to_string()),
            actor: format!("actor-{id}"),
            created_at: Utc::now(),
            revoked_at: revoked.then(Utc::now),
            expires_at: None,
            kind: KeyKind::Key,
            client_id: None,
            client_name: None,
            user: Some(crate::db::TEST_USER.to_string()),
            user_id: None,
            self_issued: false,
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
        let now = Utc::now();
        let cache = KeyCache::from_records(
            vec![
                record("reader", "reader-key", false),
                record("old", "old-key", true),
                KeyRecord {
                    sha256: Some(vec![0; 5]),
                    ..record("short", "short-key", false)
                },
            ],
            now,
        );

        let found = cache.find("reader-key", now).expect("live key");

        assert_eq!(found.key_id, "reader");
        assert_eq!(found.actor, "actor-reader");
        assert_eq!(found.user.as_deref(), Some(crate::db::TEST_USER));
        assert_eq!(found.default_catalog.as_deref(), Some("pg"));
        assert!(cache.find("old-key", now).is_none());
        // A stored hash of the wrong length is left out, not a panic.
        assert!(cache.find("short-key", now).is_none());
        assert_eq!(cache.by_hash.len(), 1);
        assert!(cache.find("no-such-key", now).is_none());
    }

    #[test]
    fn maintenance_capability_does_not_imply_write() {
        let mut reporter_record = record("reporter", "reporter-key", false);
        reporter_record.maintenance_report = true;
        let reporter = Principal::from(reporter_record);
        assert!(reporter.require_maintenance_report("pg").is_ok());
        assert!(reporter.require(Access::Write, "pg").is_err());
        assert!(matches!(
            reporter.require_maintenance_report("other"),
            Err(OkfError::CatalogNotSelected { .. })
        ));
        let writer_without_flag = Principal {
            access: Access::Write,
            maintenance_report: false,
            ..reporter.clone()
        };
        assert!(matches!(
            writer_without_flag.require_maintenance_report("pg"),
            Err(OkfError::Forbidden { .. })
        ));
        let admin_without_flag = Principal {
            access: Access::Admin,
            ..writer_without_flag
        };
        assert!(admin_without_flag.require_maintenance_report("pg").is_err());
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
        // A write key may write and read, but not administer. Its level is
        // per catalog, so the record (not just `access`) says write.
        let writer = Principal::from(KeyRecord {
            access: Access::Write,
            catalog_access: levels(&["pg"], Access::Write),
            ..record("writer", "writer-key", false)
        });
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
        everything.catalog_access.clear();
        assert!(Principal::from(everything).reaches("work"));
    }

    #[test]
    fn require_checks_the_level_of_each_catalog() {
        // A write key whose grant of `work` is read only.
        let principal = Principal::from(KeyRecord {
            access: Access::Write,
            catalogs: vec!["pg".to_string(), "work".to_string()],
            catalog_access: BTreeMap::from([
                ("pg".to_string(), Access::Write),
                ("work".to_string(), Access::Read),
            ]),
            ..record("mixed", "mixed-key", false)
        });
        let everything = Principal {
            access: Access::Write,
            reach: Reach::All,
            ..principal.clone()
        };

        assert_eq!(principal.level("pg"), Some(Access::Write));
        assert_eq!(principal.level("work"), Some(Access::Read));
        assert_eq!(principal.level("other"), None);
        assert!(principal.require(Access::Write, "pg").is_ok());
        assert!(principal.require(Access::Read, "work").is_ok());
        assert!(matches!(
            principal.require(Access::Write, "work"),
            Err(OkfError::Forbidden { reason }) if reason.contains("\"work\"")
        ));
        assert!(matches!(
            principal.require(Access::Read, "other"),
            Err(OkfError::CatalogNotSelected { .. })
        ));
        // Every catalog: the principal's own level everywhere.
        assert_eq!(everything.level("anything"), Some(Access::Write));
    }

    #[test]
    fn the_cache_leaves_out_keys_expired_at_load_and_refuses_them_at_lookup() {
        let now = Utc::now();
        let hour = chrono::TimeDelta::hours(1);
        let cache = KeyCache::from_records(
            vec![
                KeyRecord {
                    expires_at: Some(now - hour),
                    ..record("expired", "expired-key", false)
                },
                KeyRecord {
                    expires_at: Some(now + hour),
                    ..record("soon", "soon-key", false)
                },
                KeyRecord {
                    expires_at: Some(now + hour),
                    ..grant_record("grant-soon", false)
                },
                record("forever", "forever-key", false),
            ],
            now,
        );

        assert!(cache.find("expired-key", now).is_none());
        assert_eq!(cache.by_hash.len(), 2, "the expired key is not even cached");
        assert!(cache.find("soon-key", now).is_some());
        assert!(cache.find_grant("grant-soon", now).is_some());
        // The cache may be one poll old: an expiry passed since the load
        // is still refused.
        let later = now + hour * 2;
        assert!(cache.find("soon-key", later).is_none());
        assert!(cache.find_grant("grant-soon", later).is_none());
        assert!(cache.find("forever-key", later).is_some());
    }

    #[test]
    fn keys_check_expiry_against_their_clock() {
        let now = Utc::now();
        let hour = chrono::TimeDelta::hours(1);
        let soon = KeyRecord {
            expires_at: Some(now + hour),
            ..record("soon", "soon-key", false)
        };
        let fresh = Keys::new(
            KeyCache::from_records(vec![soon.clone()], now),
            Arc::new(FixedClock(now)),
        );
        let stale = Keys::new(
            KeyCache::from_records(vec![soon], now),
            Arc::new(FixedClock(now + hour * 2)),
        );

        assert!(fresh.find("soon-key").is_some());
        assert!(stale.find("soon-key").is_none());
    }

    /// A router whose one route echoes the principal's key id, behind the
    /// real middleware over `app`.
    fn echo(app: &Arc<App>) -> Router {
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
            .route_layer(axum::middleware::from_fn_with_state(
                Arc::clone(app),
                require_key,
            ))
            .with_state(Arc::clone(app))
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

    /// A stored read key on `main` called `name`, polled into the app's cache.
    async fn stored_key(app: &App, name: &str) -> (String, String) {
        let names = ["main".to_string()];
        let created = key_rows::create_with_id(
            app.pool(),
            &key_rows::NewKey {
                name,
                access: Access::Read,
                maintenance_report: false,
                grants: key_rows::Grants::Catalogs(&names),
                default_catalog: Some("main"),
                actor: "example-actor",
                user: crate::db::TEST_USER,
                self_issued: false,
                expires_at: None,
                levels: &[],
            },
            Utc::now(),
        )
        .await
        .unwrap();
        app.poll().await.unwrap();
        (created.id, created.key)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_middleware_admits_a_known_key_and_refuses_the_rest() {
        let test = crate::mcp::testing::app_with(&[("main", &[])], Config::default()).await;
        let (id, key) = stored_key(&test.app, "reader").await;
        let (old_id, old_key) = stored_key(&test.app, "old").await;
        key_rows::revoke_by_id(test.app.pool(), &old_id, Utc::now())
            .await
            .unwrap();
        test.app.poll().await.unwrap();
        let router = || echo(&test.app);

        let admitted = status_and_body(router(), Some(&format!("Bearer {key}"))).await;
        let refused = [
            status_and_body(router(), Some("Bearer okf_other-key")).await,
            status_and_body(router(), Some(&format!("Bearer {old_key}"))).await,
            status_and_body(router(), Some(&format!("Basic {key}"))).await,
            // With OAuth off, an access-token-shaped bearer is simply unknown.
            status_and_body(router(), Some("Bearer okfat_anything")).await,
            status_and_body(router(), None).await,
        ];

        assert_eq!(admitted, (StatusCode::OK, None, id));
        for (status, challenge, body) in refused {
            assert_eq!(status, StatusCode::UNAUTHORIZED);
            assert_eq!(challenge.as_deref(), Some("Bearer"));
            assert_eq!(body, "missing or unknown API key\n");
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn with_oauth_on_a_401_names_the_resource_metadata() {
        let config = crate::store::oauth::testing::oauth_config("https://okf.example.com");
        let test = crate::mcp::testing::app_with(&[("main", &[])], config).await;
        let (_, key) = stored_key(&test.app, "reader").await;

        let refused = status_and_body(echo(&test.app), None).await;
        let admitted = status_and_body(echo(&test.app), Some(&format!("Bearer {key}"))).await;

        assert_eq!(refused.0, StatusCode::UNAUTHORIZED);
        assert_eq!(
            refused.1.as_deref(),
            Some(
                "Bearer resource_metadata=\"https://okf.example.com/.well-known/oauth-protected-resource\""
            )
        );
        // Static keys keep working beside OAuth.
        assert_eq!(admitted.0, StatusCode::OK);
    }

    fn grant_record(id: &str, revoked: bool) -> KeyRecord {
        KeyRecord {
            sha256: None,
            kind: KeyKind::OAuth,
            client_id: Some("https://client.example.com/client.json".into()),
            client_name: Some("Example Client".into()),
            ..record(id, "unused", revoked)
        }
    }

    #[test]
    fn grants_are_found_by_id_only_and_carry_their_client() {
        let now = Utc::now();
        let cache = KeyCache::from_records(
            vec![
                record("reader", "reader-key", false),
                grant_record("grant-1", false),
                grant_record("grant-old", true),
            ],
            now,
        );

        let grant = cache.find_grant("grant-1", now).expect("live grant");

        assert_eq!(
            grant.kind,
            CredentialKind::OAuth {
                client_id: Some("https://client.example.com/client.json".into()),
                client_name: Some("Example Client".into())
            }
        );
        assert_eq!(grant.actor, "actor-grant-1");
        assert!(cache.find_grant("grant-old", now).is_none());
        // A key is not a grant, and a grant has no key to present.
        assert!(cache.find_grant("reader", now).is_none());
        assert!(cache.find("unused", now).is_none());
        assert_eq!(
            cache.find("reader-key", now).unwrap().kind,
            CredentialKind::ApiKey
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn maintenance_capability_change_refreshes_cache() {
        use crate::store::{catalogs, keys as key_rows};
        let db = crate::db::test_db().await;
        catalogs::create(&db.pool, "pg", "", crate::db::TEST_TEAM, Utc::now())
            .await
            .unwrap();
        let new = key_rows::NewKey {
            name: "reporter",
            access: Access::Read,
            grants: key_rows::Grants::Catalogs(&["pg".to_string()]),
            default_catalog: Some("pg"),
            actor: "tester",
            maintenance_report: true,
            user: crate::db::TEST_USER,
            self_issued: false,
            expires_at: None,
            levels: &[],
        };
        let created = key_rows::create_with_id(&db.pool, &new, Utc::now())
            .await
            .unwrap();
        let app = crate::app::App::load(
            db.pool.clone(),
            crate::config::Config::default(),
            Arc::new(crate::clock::SystemClock),
        )
        .await
        .unwrap();
        assert!(app.keys().find(&created.key).unwrap().maintenance_report);
        let before = catalogs::admin_generation(&db.pool).await.unwrap();
        assert_eq!(
            key_rows::set_maintenance_report_target(&db.pool, &created.id, None, false)
                .await
                .unwrap(),
            created.id
        );
        assert_eq!(
            catalogs::admin_generation(&db.pool).await.unwrap(),
            before + 1
        );
        assert!(app.keys().find(&created.key).unwrap().maintenance_report);
        app.poll().await.unwrap();
        let principal_after_poll = app.keys().find(&created.key).unwrap();
        assert!(!principal_after_poll.maintenance_report);
        assert!(principal_after_poll
            .require_maintenance_report("pg")
            .is_err());
        key_rows::set_maintenance_report_target(&db.pool, &created.id, None, false)
            .await
            .unwrap();
        assert_eq!(
            catalogs::admin_generation(&db.pool).await.unwrap(),
            before + 1
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn keys_load_from_the_database_and_reload_after_a_revoke() {
        use crate::store::{catalogs, keys as key_rows};
        let db = crate::db::test_db().await;
        catalogs::create(&db.pool, "pg", "", crate::db::TEST_TEAM, Utc::now())
            .await
            .unwrap();
        let names = ["pg".to_string()];
        let new = key_rows::NewKey {
            name: "reader",
            access: Access::Read,
            maintenance_report: false,
            grants: key_rows::Grants::Catalogs(&names),
            default_catalog: Some("pg"),
            actor: "tester",
            user: crate::db::TEST_USER,
            self_issued: false,
            expires_at: None,
            levels: &[],
        };
        let created = key_rows::create_with_id(&db.pool, &new, Utc::now())
            .await
            .unwrap();
        let key = created.key;

        let loaded = Keys::load(&db.pool, Arc::new(crate::clock::SystemClock))
            .await
            .unwrap();
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
            &key_rows::NewKey {
                name: "other",
                ..new
            },
            Utc::now(),
        )
        .await
        .unwrap();
        loaded.reload(&db.pool).await.unwrap();
        db.pool.close().await;
        assert!(loaded.reload(&db.pool).await.is_err());
        assert_eq!(loaded.find(&other.key).unwrap().key_id, other.id);
    }

    #[hegel::test]
    fn bearer_never_panics_and_returns_a_trimmed_nonempty_token(tc: hegel::TestCase) {
        use hegel::generators;
        let value: String = tc.draw(generators::text());
        let mut headers = HeaderMap::new();
        // Text that is not a valid header value is simply not a header.
        let Ok(parsed) = value.parse() else { return };
        headers.insert(header::AUTHORIZATION, parsed);

        if let Some(token) = bearer(&headers) {
            assert!(!token.is_empty());
            assert_eq!(token, token.trim());
        }
    }
}

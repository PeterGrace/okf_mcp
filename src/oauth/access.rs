//! OAuth access tokens on the resource side: an in-memory cache over
//! `oauth_tokens`, the audience check (RFC 8707), and the grant check
//! through the key cache, behind [`BearerVerifier`].
//!
//! As with API keys (`auth`), tokens are looked up by their SHA-256 in a
//! `HashMap`; a timing difference tells an attacker only how a guess's
//! *hash* compares with stored ones, which does not help find a token.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};

use chrono::{DateTime, Utc};

use crate::app::App;
use crate::auth::{BearerVerifier, Principal};
use crate::error::OkfError;
use crate::keys;
use crate::oauth::OAuth;
use crate::store::oauth::tokens::{self, LiveAccessToken};

/// The most access tokens cached at once. Past it, expired entries are
/// pruned; if it is still full, a token is checked against the database each
/// time instead of cached.
const MAX_CACHED: usize = 10_000;

/// The cache's contents: the tokens, and how many revocations it has seen.
#[derive(Debug, Default)]
struct Cache {
    tokens: HashMap<[u8; 32], LiveAccessToken>,
    // Bumped by every `forget`. A lookup records it before reading the
    // database and may cache its result only if it has not moved, so a
    // revocation that commits while the lookup is in flight cannot be undone
    // by the lookup re-caching the token it read before the revocation
    // (PR #243 review).
    generation: u64,
}

/// Live access tokens seen recently, by SHA-256.
#[derive(Debug, Default)]
pub struct AccessTokens {
    // A `std` mutex, held only to read or change the cache, never across an
    // `.await` (each use below is its own statement or block).
    cache: Mutex<Cache>,
}

impl AccessTokens {
    /// An empty cache.
    ///
    /// # Returns
    /// The cache.
    pub fn new() -> Self {
        Self::default()
    }

    fn lock(&self) -> MutexGuard<'_, Cache> {
        self.cache.lock().expect("access token cache lock poisoned")
    }

    /// The revocation generation. Read it before a database lookup (or, in
    /// Task 6, before minting) and pass it to [`Self::remember`].
    ///
    /// # Returns
    /// How many times [`Self::forget`] has run.
    pub fn generation(&self) -> u64 {
        self.lock().generation
    }

    /// Caches a live token, unless a revocation happened since
    /// `seen_generation` was read: then the token may be the one just
    /// revoked, and the next request rechecks the database instead.
    ///
    /// # Arguments
    /// * `sha256` - `keys::hash` of the token.
    /// * `token` - its facts.
    /// * `now` - the current time, to prune expired entries when full.
    /// * `seen_generation` - [`Self::generation`], read before the token was
    ///   looked up or minted.
    pub fn remember(
        &self,
        sha256: [u8; 32],
        token: LiveAccessToken,
        now: DateTime<Utc>,
        seen_generation: u64,
    ) {
        // The check and the insert happen under one lock, so no `forget` can
        // fall between them.
        let mut cache = self.lock();
        if cache.generation != seen_generation {
            return;
        }
        if cache.tokens.len() >= MAX_CACHED {
            cache.tokens.retain(|_, cached| cached.expires_at > now);
            if cache.tokens.len() >= MAX_CACHED {
                return;
            }
        }
        cache.tokens.insert(sha256, token);
    }

    /// Drops tokens from the cache and bumps the generation (Task 6 calls
    /// this after a revocation commits), so a revoked token stops working at
    /// once, and no lookup already in flight can cache it again.
    ///
    /// # Arguments
    /// * `hashes` - the revoked tokens' hashes.
    pub fn forget(&self, hashes: &[[u8; 32]]) {
        let mut cache = self.lock();
        // `wrapping_add`: a u64 that never realistically wraps, but if it
        // did, a wrapped value still differs from every recent reading.
        cache.generation = cache.generation.wrapping_add(1);
        for hash in hashes {
            cache.tokens.remove(hash);
        }
    }

    /// The principal of an access token.
    ///
    /// # Arguments
    /// * `app` - the shared state (database, clock, key cache).
    /// * `resource` - this server's resource identifier (`<public_url>/mcp`).
    /// * `token` - the presented `okfat_` token.
    ///
    /// # Returns
    /// The grant's principal; `None` for an unknown, revoked or expired
    /// token, one issued for another resource, or one whose grant is revoked.
    ///
    /// # Errors
    /// [`OkfError::Database`] from the lookup on a cache miss.
    pub async fn verify(
        &self,
        app: &App,
        resource: &str,
        token: &str,
    ) -> Result<Option<Arc<Principal>>, OkfError> {
        let sha256 = keys::hash(token);
        let now = app.clock().now();
        // Read the generation under the same lock as the cache miss, before
        // the database read: a revocation committed after this point bumps
        // it. The block's guard is released at its `}`, before the `.await`.
        let (cached, generation) = {
            let cache = self.lock();
            (cache.tokens.get(&sha256).cloned(), cache.generation)
        };
        let live = match cached {
            Some(live) => live,
            None => match tokens::find_access(app.pool(), &sha256, now).await? {
                Some(live) => {
                    self.remember(sha256, live.clone(), now, generation);
                    live
                }
                None => return Ok(None),
            },
        };
        if live.expires_at <= now {
            // Expiry is not a revocation: drop the entry without bumping the
            // generation, so lookups of other tokens in flight still cache.
            self.lock().tokens.remove(&sha256);
            return Ok(None);
        }
        // The audience check (RFC 8707): a token issued for another
        // resource is not a credential here.
        if live.resource != resource {
            return Ok(None);
        }
        Ok(app.keys().find_grant(&live.grant_id))
    }
}

/// The OAuth implementation of [`BearerVerifier`]: borrows what the lookup
/// needs for one request.
pub struct OAuthVerifier<'a> {
    /// The shared state.
    pub app: &'a App,
    /// The OAuth state (its settings give the resource).
    pub oauth: &'a OAuth,
}

impl BearerVerifier for OAuthVerifier<'_> {
    async fn verify(&self, token: &str) -> Result<Option<Arc<Principal>>, OkfError> {
        self.oauth
            .access
            .verify(self.app, &self.oauth.settings.resource, token)
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeDelta;

    use crate::auth::{authenticate, CredentialKind};
    use crate::mcp::testing::{app_with, TestApp};
    use crate::store::keys::{self as key_rows, Access};
    use crate::store::oauth::testing::{insert_client, insert_grant, mint_access, oauth_config};

    const RESOURCE: &str = "https://okf.example.com/mcp";

    /// An OAuth-on app with one grant on `main`, polled in.
    async fn with_grant() -> (TestApp, String) {
        let test = app_with(&[("main", &[])], oauth_config("https://okf.example.com")).await;
        let now = test.app.clock().now();
        let client = insert_client(
            test.app.pool(),
            "https://client.example.com/client.json",
            "Example Client",
            now,
        )
        .await;
        let grant = insert_grant(test.app.pool(), &client, Access::Write, &["main"], now).await;
        test.app.poll().await.unwrap();
        (test, grant)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_live_token_for_this_resource_acts_as_its_grant() {
        let (test, grant) = with_grant().await;
        let token = mint_access(&test.app, &grant, RESOURCE, TimeDelta::hours(1)).await;

        let principal = authenticate(&test.app, &token)
            .await
            .unwrap()
            .expect("admitted");
        // The second call is served from the cache: the same answer.
        let again = authenticate(&test.app, &token)
            .await
            .unwrap()
            .expect("admitted");

        assert_eq!(principal.key_id, grant);
        assert_eq!(principal.access, Access::Write);
        assert!(principal.reaches("main"));
        assert_eq!(principal.actor, "example-actor");
        assert_eq!(
            principal.kind,
            CredentialKind::OAuth {
                client_id: Some("https://client.example.com/client.json".into()),
                client_name: Some("Example Client".into())
            }
        );
        assert_eq!(again, principal);
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn wrong_audience_expired_revoked_and_unknown_tokens_are_refused() {
        let (test, grant) = with_grant().await;
        let other = mint_access(
            &test.app,
            &grant,
            "https://other.example.com/mcp",
            TimeDelta::hours(1),
        )
        .await;
        let expired = mint_access(&test.app, &grant, RESOURCE, -TimeDelta::minutes(1)).await;
        let revoked = mint_access(&test.app, &grant, RESOURCE, TimeDelta::hours(1)).await;
        // Seen once (cached), then revoked: `forget` makes it stop at once.
        assert!(authenticate(&test.app, &revoked).await.unwrap().is_some());
        crate::store::oauth::tokens::revoke(
            test.app.pool(),
            &crate::keys::hash(&revoked),
            test.app.clock().now(),
        )
        .await
        .unwrap();
        test.app
            .oauth()
            .unwrap()
            .access
            .forget(&[crate::keys::hash(&revoked)]);
        let unknown = crate::keys::generate_with(crate::keys::ACCESS_TOKEN_PREFIX).unwrap();

        for token in [other, expired, revoked, unknown] {
            assert!(authenticate(&test.app, &token).await.unwrap().is_none());
        }
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn revoking_the_grant_refuses_its_cached_token_after_a_reload() {
        let (test, grant) = with_grant().await;
        let token = mint_access(&test.app, &grant, RESOURCE, TimeDelta::hours(1)).await;
        assert!(authenticate(&test.app, &token).await.unwrap().is_some());

        key_rows::revoke_by_id(test.app.pool(), &grant, test.app.clock().now())
            .await
            .unwrap();
        test.app.poll().await.unwrap();

        assert!(authenticate(&test.app, &token).await.unwrap().is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn an_access_token_reaches_the_api_through_the_router() {
        let (test, grant) = with_grant().await;
        let token = mint_access(&test.app, &grant, RESOURCE, TimeDelta::hours(1)).await;

        let (status, body) =
            crate::http::api::testing::send(&test, "GET", "/api/v1/whoami", Some(&token), None)
                .await;

        assert_eq!(status.as_u16(), 200);
        assert_eq!(body["key_id"], grant.as_str());
        assert_eq!(body["kind"], "oauth");
        assert_eq!(body["client_name"], "Example Client");
        assert_eq!(body["access"], "write");
    }

    #[test]
    fn remember_prunes_expired_entries_when_full() {
        let tokens = AccessTokens::new();
        let now = chrono::Utc::now();
        let live = |expires_at| LiveAccessToken {
            grant_id: "g".into(),
            resource: RESOURCE.into(),
            expires_at,
        };
        for index in 0..MAX_CACHED {
            let mut hash = [0u8; 32];
            hash[..8].copy_from_slice(&(index as u64).to_le_bytes());
            tokens.remember(hash, live(now - TimeDelta::seconds(1)), now, 0);
        }

        tokens.remember([0xff; 32], live(now + TimeDelta::hours(1)), now, 0);

        let cache = tokens.lock();
        assert_eq!(cache.tokens.len(), 1, "the expired entries were pruned");
        assert!(cache.tokens.contains_key(&[0xff; 32]));
    }

    #[test]
    fn a_lookup_that_straddles_a_revocation_does_not_re_cache_the_token() {
        // The interleaving from the PR #243 review, made deterministic by
        // calling the cache's steps in that order: a request misses and reads
        // the generation, the revocation commits and forgets, then the first
        // request tries to cache what it read before the revocation.
        let tokens = AccessTokens::new();
        let now = chrono::Utc::now();
        let hash = [0x11; 32];
        let token = LiveAccessToken {
            grant_id: "g".into(),
            resource: RESOURCE.into(),
            expires_at: now + TimeDelta::hours(1),
        };
        let seen = tokens.generation();

        tokens.forget(&[hash]);
        tokens.remember(hash, token.clone(), now, seen);

        assert!(!tokens.lock().tokens.contains_key(&hash), "not re-cached");
        // A lookup that starts after the revocation caches normally (the
        // database, not the cache, then says whether the token is live).
        tokens.remember(hash, token, now, tokens.generation());
        assert!(tokens.lock().tokens.contains_key(&hash));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_revoked_token_read_before_its_revocation_is_refused_next_time() {
        // The same race end to end: a lookup reads the token while it is
        // live, the revocation commits and forgets, then the stale lookup's
        // `remember` runs with the generation it read. The next verify goes
        // to the database, which says revoked.
        let (test, grant) = with_grant().await;
        let oauth = test.app.oauth().expect("OAuth is on");
        let token = mint_access(&test.app, &grant, RESOURCE, TimeDelta::hours(1)).await;
        let hash = crate::keys::hash(&token);
        let now = test.app.clock().now();
        let seen = oauth.access.generation();
        let stale = tokens::find_access(test.app.pool(), &hash, now)
            .await
            .unwrap()
            .expect("live before the revocation");

        tokens::revoke(test.app.pool(), &hash, now).await.unwrap();
        oauth.access.forget(&[hash]);
        oauth.access.remember(hash, stale, now, seen);

        let after = oauth
            .access
            .verify(&test.app, RESOURCE, &token)
            .await
            .unwrap();
        assert!(
            after.is_none(),
            "the revoked token is not accepted from the cache"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_cached_token_past_its_expiry_is_refused_and_dropped() {
        // A token that entered the cache while live and has since expired.
        // It is not in the database, so only the cache-expiry check can
        // refuse it: without that check, `verify` would admit the live grant.
        let (test, grant) = with_grant().await;
        let oauth = test.app.oauth().expect("OAuth is on");
        let token = keys::generate_with(keys::ACCESS_TOKEN_PREFIX).unwrap();
        let hash = keys::hash(&token);
        let now = test.app.clock().now();
        let generation = oauth.access.generation();
        oauth.access.remember(
            hash,
            LiveAccessToken {
                grant_id: grant,
                resource: RESOURCE.into(),
                expires_at: now - TimeDelta::seconds(1),
            },
            now,
            generation,
        );
        assert!(oauth.access.lock().tokens.contains_key(&hash), "cached");

        let after = oauth
            .access
            .verify(&test.app, RESOURCE, &token)
            .await
            .unwrap();

        assert!(after.is_none(), "an expired cached token is refused");
        assert!(
            !oauth.access.lock().tokens.contains_key(&hash),
            "and dropped"
        );
        assert_eq!(
            oauth.access.generation(),
            generation,
            "expiry is not a revocation"
        );
    }
}

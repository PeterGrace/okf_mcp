//! OIDC providers from `[[auth.oidc]]` (spec "OIDC", plan Decision 12).
//! Each provider's discovery document is cached for an hour, and a failed
//! refresh keeps the cached one. The registry builds the authorize URL
//! (PKCE S256, `state`, `nonce`) and exchanges the code, verifying the ID
//! token. HTTP goes through the server's own reqwest 0.13 by a small
//! adapter, so no second reqwest is linked.
//!
//! Secrets stay out of logs: the client secret is an `openidconnect`
//! `ClientSecret` (its `Debug` is redacted), and the flow's `state`, `nonce`
//! and PKCE verifier live in [`FlowSecrets`], whose `Debug` is redacted too.

use std::collections::BTreeMap;
use std::time::{Duration, Instant};

use openidconnect::core::{CoreAuthenticationFlow, CoreClient, CoreProviderMetadata};
use openidconnect::{
    AuthorizationCode, ClientId, ClientSecret, CsrfToken, EndpointMaybeSet, EndpointNotSet,
    EndpointSet, HttpRequest, HttpResponse, IssuerUrl, Nonce, PkceCodeChallenge, PkceCodeVerifier,
    RedirectUrl, Scope, TokenResponse,
};

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;

use crate::config::{Config, OidcProviderConfig};
use crate::error::OkfError;
use crate::http::limits::KeyedLimiter;
use crate::keys;
use crate::oauth::expiring::ExpiringMap;

/// A client configured from a discovery document: an authorization
/// endpoint, and maybe a token and a userinfo endpoint (the typestate the
/// crate gives `from_provider_metadata`).
pub type ConfiguredClient = CoreClient<
    EndpointSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointNotSet,
    EndpointMaybeSet,
    EndpointMaybeSet,
>;

/// How long a discovery document is used before it is fetched again.
pub const DISCOVERY_TTL: Duration = Duration::from_secs(60 * 60);
/// After a failed refresh, how long the cached document is used before
/// the next try.
pub const FAILED_REFRESH_RETRY: Duration = Duration::from_secs(60);
/// How long a flow may take (spec: a ten-minute cookie).
pub const FLOW_TTL: Duration = Duration::from_secs(10 * 60);
/// The most flows in progress at once (they start unauthenticated, so the
/// start route also takes from the per-address sign-in limit).
pub const MAX_FLOWS: usize = 10_000;
/// The per-user link allowance: a token bucket with a burst of this many
/// starts, refilled one per minute (this many per [`FLOW_TTL`]). Each start
/// holds a flow-map entry for ten minutes, so one signed-in user holds at
/// most about twice this many (the burst plus ten minutes of refill) of
/// the map every sign-in shares.
pub const LINK_STARTS_PER_USER: u32 = 10;
/// The most users the per-user link limiter tracks at once.
const MAX_LINKING_USERS: usize = 10_000;
/// The longest any provider request may take.
const HTTP_TIMEOUT: Duration = Duration::from_secs(10);
/// The longest `sub` OIDC Core allows (section 2: "MUST NOT exceed 255
/// ASCII characters").
const MAX_SUBJECT_LENGTH: usize = 255;

/// Why an OIDC step failed.
#[derive(Debug, thiserror::Error)]
pub enum OidcError {
    /// No provider has this id.
    #[error("unknown OIDC provider {0:?}")]
    UnknownProvider(String),
    /// Discovery failed and nothing is cached.
    #[error("OIDC discovery for {provider} failed: {message}")]
    Discovery {
        /// The provider's id.
        provider: String,
        /// What went wrong.
        message: String,
    },
    /// The code exchange failed.
    #[error("the OIDC code exchange failed: {0}")]
    Exchange(String),
    /// The ID token is missing or did not verify.
    #[error("the ID token was refused: {0}")]
    IdToken(String),
}

impl From<OidcError> for OkfError {
    fn from(error: OidcError) -> Self {
        let message = error.to_string();
        match error {
            OidcError::UnknownProvider(id) => OkfError::NotFound {
                path: format!("auth/oidc/{id}"),
            },
            OidcError::Discovery { .. } => OkfError::Busy { reason: message },
            OidcError::Exchange(_) | OidcError::IdToken(_) => {
                OkfError::Unauthenticated { reason: message }
            }
        }
    }
}

/// A provider request that failed: the adapter's error type.
#[derive(Debug, thiserror::Error)]
#[error("{0}")]
pub struct HttpError(String);

/// The adapter: sends one openidconnect request with reqwest 0.13. A
/// closure calling it is an `AsyncHttpClient` (oauth2's blanket impl for
/// any `Fn(HttpRequest) -> impl Future<Output = Result<HttpResponse, E>>`).
async fn send(client: reqwest::Client, request: HttpRequest) -> Result<HttpResponse, HttpError> {
    // The annotation names the conversion's error type for `map_err`.
    let request: reqwest::Request = request
        .try_into()
        .map_err(|error: reqwest::Error| HttpError(error.to_string()))?;
    let response = client
        .execute(request)
        .await
        .map_err(|error| HttpError(error.to_string()))?;
    let mut builder = openidconnect::http::Response::builder().status(response.status());
    for (name, value) in response.headers() {
        builder = builder.header(name, value);
    }
    let body = response
        .bytes()
        .await
        .map_err(|error| HttpError(error.to_string()))?
        .to_vec();
    builder
        .body(body)
        .map_err(|error| HttpError(error.to_string()))
}

/// A flow's secrets: compared or sent back once, never logged.
pub struct FlowSecrets {
    /// The `state` sent to the provider.
    pub state: String,
    /// The `nonce` the ID token must carry.
    pub nonce: String,
    /// The PKCE verifier.
    pub verifier: String,
}

// Written by hand, not derived (Python's `__repr__`), so a stray `{:?}`
// prints no secret.
impl std::fmt::Debug for FlowSecrets {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("FlowSecrets(<redacted>)")
    }
}

/// A signed-in user linking a provider from the account page.
pub struct LinkRequest {
    /// The user's id.
    pub user_id: String,
    /// The raw id of the session that asked: the callback must come back
    /// with it, and it is kept when the others are ended.
    pub session_id: String,
    /// Keep the user's other sessions.
    pub keep_sessions: bool,
}

// Hand-written: the session id is a credential.
impl std::fmt::Debug for LinkRequest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("LinkRequest")
            .field("user_id", &self.user_id)
            .field("session_id", &"<redacted>")
            .field("keep_sessions", &self.keep_sessions)
            .finish()
    }
}

/// A flow in progress, keyed by the `okf_oidc` cookie's random id. Its
/// `Debug` shows no secret ([`FlowSecrets`] and [`LinkRequest`] redact).
#[derive(Debug)]
pub struct Flow {
    /// The provider it was started with.
    pub provider: String,
    /// Its secrets.
    pub secrets: FlowSecrets,
    /// Where to go after signing in.
    pub return_to: String,
    /// Set when a signed-in user is linking, not signing in.
    pub link: Option<LinkRequest>,
}

/// What a verified ID token says about the person.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    /// The provider's `sub` (1 to 255 characters, no control character).
    pub subject: String,
    /// Its `email`, if sent.
    pub email: Option<String>,
    /// Whether `email_verified` was `true` (or the string `"true"`, which
    /// some providers send: the crate's `accept-string-booleans`).
    pub email_verified: bool,
}

/// Whether a `sub` may be stored and looked up: non-empty, at most 255
/// bytes, and free of control characters (Postgres refuses a NUL in text,
/// which would turn a hostile token into a database error).
fn valid_subject(subject: &str) -> bool {
    (1..=MAX_SUBJECT_LENGTH).contains(&subject.len()) && !subject.chars().any(char::is_control)
}

/// One configured provider.
struct Provider {
    config: OidcProviderConfig,
    secret: ClientSecret,
    redirect: RedirectUrl,
    // tokio's mutex, held across a discovery fetch: concurrent sign-ins
    // wait for one fetch instead of each making their own.
    cached: tokio::sync::Mutex<Option<Cached>>,
}

/// A configured client and when to refresh it.
struct Cached {
    client: ConfiguredClient,
    refresh_at: Instant,
}

/// The configured providers and the flows in progress, held by `App`.
pub struct OidcProviders {
    http: reqwest::Client,
    providers: BTreeMap<String, Provider>,
    /// Flows in progress (ten minutes, single use), keyed by the flow
    /// cookie's random id (chosen by the server, never by the client).
    /// Add to it with [`Self::store_flow`], which bounds each actor's share.
    pub flows: ExpiringMap<Flow>,
    /// Each console session's live link flow, by the SHA-256 of the session
    /// id: a new link flow replaces (and removes) the session's previous one.
    link_flows: ExpiringMap<String>,
    /// Link starts by user id: a burst of [`LINK_STARTS_PER_USER`], refilled
    /// one per minute.
    link_starts: KeyedLimiter,
}

// Hand-written: the providers hold client secrets, and the flows theirs.
impl std::fmt::Debug for OidcProviders {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("OidcProviders")
            .field("providers", &self.providers.keys().collect::<Vec<_>>())
            .finish_non_exhaustive()
    }
}

impl OidcProviders {
    /// The registry for `[[auth.oidc]]`.
    ///
    /// # Arguments
    /// * `config` - the server's settings (validated).
    /// * `secret_of` - reads an environment variable (`std::env::var` in
    ///   production; a closure in tests).
    ///
    /// # Returns
    /// `None` without `public_url` or providers.
    ///
    /// # Errors
    /// [`OkfError::Config`] when a provider's secret variable is unset or
    /// empty, or the HTTP client cannot be built.
    pub fn new(
        config: &Config,
        secret_of: impl Fn(&str) -> Option<String>,
    ) -> Result<Option<Self>, OkfError> {
        let Some(public_url) = config.public_url.as_deref() else {
            return Ok(None);
        };
        if config.auth.oidc.is_empty() {
            return Ok(None);
        }
        let config_error = |path: String, message: String| OkfError::Config { path, message };
        let http = reqwest::Client::builder()
            // Discovery and the token endpoint answer directly; following a
            // redirect could reach an internal address.
            .redirect(reqwest::redirect::Policy::none())
            .timeout(HTTP_TIMEOUT)
            .user_agent(concat!("okf-mcp/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|error| {
                config_error(
                    "auth.oidc".to_string(),
                    format!("building its HTTP client: {error}"),
                )
            })?;
        let mut providers = BTreeMap::new();
        for provider in &config.auth.oidc {
            let path = format!("auth.oidc.{}", provider.id);
            // The message names the variable, never its value.
            let secret = secret_of(&provider.client_secret_env)
                .filter(|secret| !secret.is_empty())
                .ok_or_else(|| {
                    config_error(
                        path.clone(),
                        format!(
                            "the environment variable {} is not set",
                            provider.client_secret_env
                        ),
                    )
                })?;
            let redirect = RedirectUrl::new(format!(
                "{}/auth/oidc/{}/callback",
                public_url.trim_end_matches('/'),
                provider.id
            ))
            .map_err(|error| config_error(path.clone(), error.to_string()))?;
            providers.insert(
                provider.id.clone(),
                Provider {
                    config: provider.clone(),
                    secret: ClientSecret::new(secret),
                    redirect,
                    cached: tokio::sync::Mutex::new(None),
                },
            );
        }
        Ok(Some(Self {
            http,
            providers,
            flows: ExpiringMap::new(FLOW_TTL, MAX_FLOWS),
            link_flows: ExpiringMap::new(FLOW_TTL, MAX_FLOWS),
            link_starts: KeyedLimiter::new(LINK_STARTS_PER_USER, FLOW_TTL, MAX_LINKING_USERS),
        }))
    }

    /// Stores a new flow so that no actor holds more than its share of
    /// [`Self::flows`]: the flow this browser held before (its old cookie)
    /// is removed, and a link flow replaces the one its session started
    /// before. The new flow is stored before the old one is removed, so
    /// two concurrent starts of one session leave one flow, never a
    /// stranded one.
    ///
    /// # Arguments
    /// * `id` - the new flow cookie's random id.
    /// * `flow` - the flow.
    /// * `previous` - the id in the browser's current flow cookie, if any.
    /// * `now` - the current instant.
    ///
    /// # Returns
    /// `false`, storing nothing, when the map is full.
    pub fn store_flow(&self, id: &str, flow: Flow, previous: Option<&str>, now: Instant) -> bool {
        // The session the link belongs to, as the key of its index entry
        // (a hash: the raw session id is a credential).
        let session_key = flow
            .link
            .as_ref()
            .map(|link| URL_SAFE_NO_PAD.encode(keys::hash(&link.session_id)));
        if let Some(previous) = previous.filter(|previous| *previous != id) {
            self.flows.remove(previous);
        }
        if !self.flows.insert(id.to_string(), flow, now) {
            return false;
        }
        if let Some(key) = session_key {
            // `mem::replace` swaps the session's flow id for the new one and
            // returns the old (empty for a new entry), under the map's lock.
            let replaced =
                self.link_flows
                    .update_or_insert_sliding(&key, now, String::new, |current| {
                        std::mem::replace(current, id.to_string())
                    });
            match replaced {
                Some(old) if !old.is_empty() && old != id => self.flows.remove(&old),
                Some(_) => {}
                // The index is full: keep the map bounded by refusing.
                None => {
                    self.flows.remove(id);
                    return false;
                }
            }
        }
        true
    }

    /// Takes one link start from the user's allowance.
    ///
    /// # Arguments
    /// * `user_id` - the signed-in user.
    /// * `now` - the current instant.
    ///
    /// # Returns
    /// `false` once the user's bucket is empty: a burst of
    /// [`LINK_STARTS_PER_USER`] starts, then one more per minute.
    pub fn may_start_link(&self, user_id: &str, now: Instant) -> bool {
        self.link_starts.try_take(user_id, now)
    }

    /// Every provider's id and label, by id.
    ///
    /// # Returns
    /// `(id, label)` pairs.
    // `+ '_`: the iterator borrows `self`.
    pub fn labels(&self) -> impl Iterator<Item = (&str, &str)> + '_ {
        self.providers
            .values()
            .map(|provider| (provider.config.id.as_str(), provider.config.label.as_str()))
    }

    /// A provider's label.
    ///
    /// # Arguments
    /// * `id` - the provider's id.
    ///
    /// # Returns
    /// Its label, or `None` for an unknown id.
    pub fn label(&self, id: &str) -> Option<&str> {
        self.providers
            .get(id)
            .map(|provider| provider.config.label.as_str())
    }

    /// The provider called `id`.
    fn provider(&self, id: &str) -> Result<&Provider, OidcError> {
        self.providers
            .get(id)
            .ok_or_else(|| OidcError::UnknownProvider(id.to_string()))
    }

    /// Fetches the discovery document and configures a client from it.
    async fn discover(&self, provider: &Provider) -> Result<ConfiguredClient, OidcError> {
        let failed = |message: String| OidcError::Discovery {
            provider: provider.config.id.clone(),
            message,
        };
        let issuer = IssuerUrl::new(provider.config.issuer.clone())
            .map_err(|error| failed(error.to_string()))?;
        // The closure is the `AsyncHttpClient`; each call gets its own
        // handle to the pooled client (a cheap `Arc` clone).
        let http = |request: HttpRequest| send(self.http.clone(), request);
        let metadata = CoreProviderMetadata::discover_async(issuer, &http)
            .await
            .map_err(|error| failed(error.to_string()))?;
        Ok(CoreClient::from_provider_metadata(
            metadata,
            ClientId::new(provider.config.client_id.clone()),
            Some(provider.secret.clone()),
        )
        .set_redirect_uri(provider.redirect.clone()))
    }

    /// A provider's client, from the cache while it is fresh.
    ///
    /// # Arguments
    /// * `id` - the provider's id.
    /// * `now` - the current instant (passed in, so tests control time).
    ///
    /// # Returns
    /// The client: freshly discovered, cached, or (after a failed refresh)
    /// the stale cached one, retried after [`FAILED_REFRESH_RETRY`].
    ///
    /// # Errors
    /// [`OidcError::UnknownProvider`]; [`OidcError::Discovery`] when
    /// nothing is cached and discovery fails.
    pub async fn client(&self, id: &str, now: Instant) -> Result<ConfiguredClient, OidcError> {
        let provider = self.provider(id)?;
        let mut cached = provider.cached.lock().await;
        if let Some(entry) = cached.as_ref().filter(|entry| now < entry.refresh_at) {
            return Ok(entry.client.clone());
        }
        match self.discover(provider).await {
            Ok(client) => {
                *cached = Some(Cached {
                    client: client.clone(),
                    refresh_at: now + DISCOVERY_TTL,
                });
                Ok(client)
            }
            Err(error) => match cached.as_mut() {
                Some(entry) => {
                    tracing::warn!(
                        provider = id,
                        %error,
                        "OIDC discovery failed; keeping the cached document"
                    );
                    entry.refresh_at = now + FAILED_REFRESH_RETRY;
                    Ok(entry.client.clone())
                }
                None => Err(error),
            },
        }
    }

    /// The provider's authorize URL for a new flow.
    ///
    /// # Arguments
    /// * `id` - the provider's id.
    /// * `now` - the current instant.
    ///
    /// # Returns
    /// The URL and the flow's secrets (for the caller to keep).
    ///
    /// # Errors
    /// As [`Self::client`].
    pub async fn authorize_url(
        &self,
        id: &str,
        now: Instant,
    ) -> Result<(String, FlowSecrets), OidcError> {
        let client = self.client(id, now).await?;
        let provider = self.provider(id)?;
        let (challenge, verifier) = PkceCodeChallenge::new_random_sha256();
        let mut request = client
            .authorize_url(
                CoreAuthenticationFlow::AuthorizationCode,
                CsrfToken::new_random,
                Nonce::new_random,
            )
            .set_pkce_challenge(challenge);
        // The crate always sends `openid`; adding it again would repeat it.
        for scope in provider
            .config
            .scopes
            .iter()
            .filter(|scope| scope.as_str() != "openid")
        {
            request = request.add_scope(Scope::new(scope.clone()));
        }
        let (url, state, nonce) = request.url();
        let secrets = FlowSecrets {
            state: state.into_secret(),
            nonce: nonce.secret().clone(),
            verifier: verifier.into_secret(),
        };
        Ok((url.to_string(), secrets))
    }

    /// Exchanges the code and verifies the ID token (issuer, audience,
    /// expiry, signature and nonce). The caller has already checked
    /// `state`: the crate does not.
    ///
    /// # Arguments
    /// * `id` - the provider's id.
    /// * `code` - the callback's `code`.
    /// * `secrets` - the flow's secrets (consumed: they are single use).
    /// * `now` - the current instant.
    ///
    /// # Returns
    /// What the token says about the person.
    ///
    /// # Errors
    /// [`OidcError::Exchange`] or [`OidcError::IdToken`] (also for a `sub`
    /// that is empty, too long or holds a control character); as
    /// [`Self::client`].
    pub async fn identify(
        &self,
        id: &str,
        code: String,
        secrets: FlowSecrets,
        now: Instant,
    ) -> Result<Identity, OidcError> {
        let client = self.client(id, now).await?;
        let http = |request: HttpRequest| send(self.http.clone(), request);
        let tokens = client
            .exchange_code(AuthorizationCode::new(code))
            .map_err(|error| OidcError::Exchange(error.to_string()))?
            .set_pkce_verifier(PkceCodeVerifier::new(secrets.verifier))
            .request_async(&http)
            .await
            .map_err(|error| OidcError::Exchange(error.to_string()))?;
        // `id_token` comes from the `TokenResponse` trait imported above.
        let id_token = tokens
            .id_token()
            .ok_or_else(|| OidcError::IdToken("the provider sent no ID token".to_string()))?;
        let nonce = Nonce::new(secrets.nonce);
        let claims = id_token
            .claims(&client.id_token_verifier(), &nonce)
            .map_err(|error| OidcError::IdToken(error.to_string()))?;
        let subject = claims.subject().as_str();
        if !valid_subject(subject) {
            return Err(OidcError::IdToken(
                "its subject is empty, too long or holds a control character".to_string(),
            ));
        }
        Ok(Identity {
            subject: subject.to_string(),
            email: claims.email().map(|email| email.as_str().to_string()),
            email_verified: claims.email_verified() == Some(true),
        })
    }
}

#[cfg(test)]
pub(crate) mod testing;

#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use openidconnect::core::CoreIdTokenClaims;

    use super::testing::{self, SECRET};
    use super::*;
    use crate::config::AuthConfig;

    fn config(idp: &testing::MockIdp) -> Config {
        Config {
            public_url: Some("https://okf.example.com".to_string()),
            auth: AuthConfig {
                oidc: vec![testing::provider(idp, "UNUSED")],
                ..AuthConfig::default()
            },
            ..Config::default()
        }
    }

    fn providers(idp: &testing::MockIdp) -> OidcProviders {
        OidcProviders::new(&config(idp), |_| Some(SECRET.to_string()))
            .expect("built")
            .expect("on")
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn discovery_is_cached_for_an_hour_and_kept_when_a_refresh_fails() {
        let idp = testing::start().await;
        let providers = providers(&idp);
        let start = Instant::now();

        providers
            .client("example", start)
            .await
            .expect("discovered");
        providers
            .client("example", start + Duration::from_secs(30 * 60))
            .await
            .expect("cached");
        assert_eq!(idp.discoveries(), 1);
        idp.discovery_down.store(true, Ordering::SeqCst);
        let later = start + Duration::from_secs(2 * 60 * 60);
        providers
            .client("example", later)
            .await
            .expect("the cached document is kept");
        assert_eq!(idp.discoveries(), 2);
        // After a failed refresh it waits a minute before asking again.
        providers
            .client("example", later + Duration::from_secs(30))
            .await
            .expect("still kept");
        assert_eq!(idp.discoveries(), 2);
        let fresh = self::providers(&idp).client("example", start).await;
        assert!(
            matches!(fresh, Err(OidcError::Discovery { .. })),
            "nothing cached yet"
        );
        let unknown = providers.client("nope", start).await;
        assert!(matches!(unknown, Err(OidcError::UnknownProvider(_))));
    }

    #[test]
    fn a_missing_client_secret_fails_start_up() {
        let config = Config {
            public_url: Some("https://okf.example.com".to_string()),
            auth: AuthConfig {
                oidc: vec![OidcProviderConfig {
                    id: "example".to_string(),
                    label: "Example ID".to_string(),
                    issuer: "https://id.example.com".to_string(),
                    client_id: "okf-console".to_string(),
                    client_secret_env: "OKF_OIDC_EXAMPLE_SECRET".to_string(),
                    scopes: vec!["openid".to_string()],
                }],
                ..AuthConfig::default()
            },
            ..Config::default()
        };

        let refused = OidcProviders::new(&config, |_| None);
        let empty = OidcProviders::new(&config, |_| Some(String::new()));

        assert!(matches!(refused, Err(OkfError::Config { .. })));
        assert!(matches!(empty, Err(OkfError::Config { .. })));
        assert!(OidcProviders::new(&Config::default(), |_| None)
            .expect("no providers")
            .is_none());
    }

    #[test]
    fn no_secret_reaches_debug_output() {
        let secrets = FlowSecrets {
            state: "state-value".to_string(),
            nonce: "nonce-value".to_string(),
            verifier: "verifier-value".to_string(),
        };
        let flow = Flow {
            provider: "example".to_string(),
            secrets,
            return_to: "/ui/".to_string(),
            link: Some(LinkRequest {
                user_id: "user-1".to_string(),
                session_id: "session-value".to_string(),
                keep_sessions: false,
            }),
        };
        let providers = OidcProviders::new(
            &Config {
                public_url: Some("https://okf.example.com".to_string()),
                auth: AuthConfig {
                    oidc: vec![OidcProviderConfig {
                        id: "example".to_string(),
                        label: "Example ID".to_string(),
                        issuer: "https://id.example.com".to_string(),
                        client_id: "okf-console".to_string(),
                        client_secret_env: "UNUSED".to_string(),
                        scopes: vec!["openid".to_string()],
                    }],
                    ..AuthConfig::default()
                },
                ..Config::default()
            },
            |_| Some("client-secret-value".to_string()),
        )
        .expect("built")
        .expect("on");

        let printed = format!("{flow:?} {providers:?}");

        for secret in [
            "state-value",
            "nonce-value",
            "verifier-value",
            "session-value",
            "client-secret-value",
        ] {
            assert!(!printed.contains(secret), "{secret} in {printed}");
        }
        assert!(printed.contains("example"), "{printed}");
    }

    #[test]
    fn email_verified_may_be_the_string_true() {
        // Some providers send `"email_verified": "true"`; the crate's
        // `accept-string-booleans` feature reads it as `true`.
        let claims = |verified: serde_json::Value| {
            serde_json::from_value::<CoreIdTokenClaims>(serde_json::json!({
                "iss": "https://id.example.com",
                "aud": "okf-console",
                "exp": 4_000_000_000_i64,
                "iat": 1_700_000_000_i64,
                "sub": "sub-1",
                "email": "alice@example.com",
                "email_verified": verified,
            }))
            .expect("claims parse")
        };

        assert_eq!(
            claims(serde_json::json!("true")).email_verified(),
            Some(true)
        );
        assert_eq!(
            claims(serde_json::json!("false")).email_verified(),
            Some(false)
        );
        assert_eq!(claims(serde_json::json!(true)).email_verified(), Some(true));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn link_starts_are_a_burst_of_ten_refilled_one_per_minute() {
        let idp = testing::start().await;
        let providers = providers(&idp);
        // A fixed starting instant; later ones are offsets from it, so the
        // test controls time exactly.
        let start = Instant::now();
        let take = |at: Instant| providers.may_start_link("user-1", at);

        let burst = (0..LINK_STARTS_PER_USER).filter(|_| take(start)).count();
        let eleventh = take(start);
        let after_a_minute = start + Duration::from_secs(61);
        let one_more = take(after_a_minute);
        let not_two = take(after_a_minute);
        let refilled = after_a_minute + FLOW_TTL;
        let again = (0..LINK_STARTS_PER_USER).filter(|_| take(refilled)).count();
        let past_the_burst = take(refilled);

        assert_eq!(burst, 10);
        assert!(!eleventh, "the eleventh start in a burst is refused");
        assert!(one_more, "a minute refills one start");
        assert!(!not_two, "and only one");
        assert_eq!(again, 10, "a full refill allows a whole burst again");
        assert!(!past_the_burst);
        assert!(providers.may_start_link("user-2", start), "users are apart");
    }

    #[test]
    fn a_subject_postgres_would_refuse_or_too_long_is_invalid() {
        assert!(valid_subject("sub-1"));
        assert!(valid_subject(&"a".repeat(MAX_SUBJECT_LENGTH)));
        assert!(!valid_subject(""));
        assert!(!valid_subject("sub\0one"));
        assert!(!valid_subject("sub\none"));
        assert!(!valid_subject(&"a".repeat(MAX_SUBJECT_LENGTH + 1)));
    }
}

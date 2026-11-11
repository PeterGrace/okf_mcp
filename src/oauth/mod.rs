//! The built-in OAuth 2.1 authorization server and the OAuth side of the
//! resource server (M10; spec `docs/superpowers/specs/2026-11-11-okf-oauth-design.md`).
//! On only when `public_url` is set ([`OAuth::new`]).

pub mod access;
pub mod authorize;
pub mod clients;
pub mod consent;
pub mod error;
pub mod expiring;
pub mod metadata;
pub mod pkce;
pub mod ratelimit;
pub mod state;
// Task 5 adds `#[cfg(test)] pub(crate) mod testing;` with its file.
pub mod token;

use std::sync::Arc;
use std::time::Duration;

use axum::http::{header, HeaderValue};
use axum::middleware;
use axum::response::Response;
use axum::Router;

use crate::app::App;
use crate::config::Config;
use crate::error::OkfError;

/// Where the Protected Resource Metadata is served (RFC 9728), also with
/// the resource's path appended (`.../mcp`).
pub const PROTECTED_RESOURCE_PATH: &str = "/.well-known/oauth-protected-resource";

/// Where the Authorization Server Metadata is served (RFC 8414).
pub const AUTHORIZATION_SERVER_PATH: &str = "/.well-known/oauth-authorization-server";

/// Registration: a burst of 10, then one every 6 seconds.
const REGISTER_BURST: u32 = 10;
const REGISTER_PER_SEC: f64 = 1.0 / 6.0;
/// Token exchange: a burst of 60, then 2 a second.
const TOKEN_BURST: u32 = 60;
const TOKEN_PER_SEC: f64 = 2.0;
/// Consent posts: a burst of 20, then 1 a second (a person and a browser
/// journey post a few times per sign-in).
const CONSENT_BURST: u32 = 20;
const CONSENT_PER_SEC: f64 = 1.0;

/// The OAuth settings, derived once from the configuration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthSettings {
    /// The issuer: `public_url` without a trailing slash.
    pub issuer: String,
    /// The resource tokens are issued for: `<issuer>/mcp`.
    pub resource: String,
    /// The Protected Resource Metadata URL a 401 names.
    pub resource_metadata_url: String,
    /// `oauth.access_token_ttl`.
    pub access_token_ttl: Duration,
    /// `oauth.refresh_token_ttl`.
    pub refresh_token_ttl: Duration,
    /// `oauth.code_max_expiry`.
    pub code_max_expiry: Duration,
    /// `oauth.dcr_enabled`.
    pub dcr_enabled: bool,
    /// `oauth.cimd_allow_private`.
    pub cimd_allow_private: bool,
}

/// The authorization server's state, held by `App` when OAuth is on.
pub struct OAuth {
    /// The settings.
    pub settings: OAuthSettings,
    /// The 401 `WWW-Authenticate` value, built once.
    pub challenge: HeaderValue,
    /// Live access tokens (the resource side).
    pub access: access::AccessTokens,
    /// Consent pages in progress, by pending id.
    pub pending: expiring::ExpiringMap<state::PendingAuthorization>,
    /// Unexchanged authorization codes, by the hex SHA-256 of the code.
    pub codes: expiring::ExpiringMap<state::AuthorizationCode>,
    /// The Client ID Metadata Document fetcher.
    pub cimd: clients::CimdFetcher,
    /// `POST /oauth/register`'s limit.
    pub register_limit: ratelimit::TokenBucket,
    /// `POST /oauth/token`'s (and `/oauth/revoke`'s) limit.
    pub token_limit: ratelimit::TokenBucket,
    /// `POST /oauth/authorize`'s limit.
    pub consent_limit: ratelimit::TokenBucket,
}

impl OAuth {
    /// The OAuth state for a configuration that `Config::validate` accepted.
    ///
    /// # Arguments
    /// * `config` - the server's settings.
    ///
    /// # Returns
    /// `None` without `public_url` (OAuth off), else the state.
    ///
    /// # Errors
    /// [`OkfError::Config`] if `public_url` cannot appear in a header
    /// (`Config::validate` refuses such a URL first, so only a caller that
    /// skipped validation sees this).
    pub fn new(config: &Config) -> Result<Option<Self>, OkfError> {
        let Some(public_url) = config.public_url.as_deref() else {
            return Ok(None);
        };
        let issuer = public_url.trim_end_matches('/').to_string();
        let allow_private = config.oauth.cimd_allow_private;
        // Plain-http documents only on a test rig: private addresses allowed
        // and the server itself on loopback http.
        let allow_http = allow_private && issuer.starts_with("http://");
        let settings = OAuthSettings {
            resource: format!("{issuer}/mcp"),
            resource_metadata_url: format!("{issuer}{PROTECTED_RESOURCE_PATH}"),
            issuer,
            access_token_ttl: config.oauth.access_token_ttl,
            refresh_token_ttl: config.oauth.refresh_token_ttl,
            code_max_expiry: config.oauth.code_max_expiry,
            dcr_enabled: config.oauth.dcr_enabled,
            cimd_allow_private: config.oauth.cimd_allow_private,
        };
        let challenge = HeaderValue::from_str(&format!(
            "Bearer resource_metadata=\"{}\"",
            settings.resource_metadata_url
        ))
        .map_err(|_| OkfError::Config {
            path: "public_url".to_string(),
            message: format!("{public_url:?} cannot appear in a header"),
        })?;
        Ok(Some(Self {
            settings,
            challenge,
            access: access::AccessTokens::new(),
            pending: expiring::ExpiringMap::new(state::PENDING_TTL, state::MAX_PENDING),
            codes: expiring::ExpiringMap::new(
                state::AUTHORIZATION_CODE_TTL,
                state::MAX_AUTHORIZATION_CODES,
            ),
            cimd: clients::CimdFetcher::new(allow_private, allow_http)?,
            register_limit: ratelimit::TokenBucket::new(REGISTER_BURST, REGISTER_PER_SEC),
            token_limit: ratelimit::TokenBucket::new(TOKEN_BURST, TOKEN_PER_SEC),
            consent_limit: ratelimit::TokenBucket::new(CONSENT_BURST, CONSENT_PER_SEC),
        }))
    }
}

/// Every OAuth route (`/.well-known/*`, `/oauth/*`), for `http::router` to
/// merge when OAuth is on. No route needs a bearer token.
///
/// # Returns
/// A router whose responses all carry `Cache-Control: no-store`.
pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .merge(metadata::routes())
        .merge(clients::routes())
        .merge(authorize::routes())
        .merge(token::routes())
        .layer(middleware::map_response(no_store))
}

/// Marks a response uncacheable: tokens, codes and consent pages must never
/// be stored by a cache.
async fn no_store(mut response: Response) -> Response {
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn oauth_is_off_without_public_url() {
        assert!(OAuth::new(&Config::default()).unwrap().is_none());
    }

    #[test]
    fn settings_derive_from_public_url() {
        let config = crate::store::oauth::testing::oauth_config("https://okf.example.com/");

        let oauth = OAuth::new(&config).unwrap().expect("on");

        assert_eq!(oauth.settings.issuer, "https://okf.example.com");
        assert_eq!(oauth.settings.resource, "https://okf.example.com/mcp");
        assert_eq!(
            oauth.settings.resource_metadata_url,
            "https://okf.example.com/.well-known/oauth-protected-resource"
        );
        assert_eq!(
            oauth.challenge,
            "Bearer resource_metadata=\"https://okf.example.com/.well-known/oauth-protected-resource\""
        );
        assert_eq!(
            oauth.settings.access_token_ttl,
            config.oauth.access_token_ttl
        );
    }
}

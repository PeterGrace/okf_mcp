//! The two discovery documents (spec "Endpoints"): Protected Resource
//! Metadata (RFC 9728), which tells a client where this resource's
//! authorization server is, and Authorization Server Metadata (RFC 8414),
//! which tells it the endpoints and what they support. Both are public: no
//! token, and `Access-Control-Allow-Origin: *` so a browser-based MCP client
//! can read them too. Like every OAuth route they carry
//! `Cache-Control: no-store` (the layer in [`crate::oauth::routes`]).

use std::sync::Arc;

use axum::extract::State;
use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use serde::Serialize;

use crate::app::App;
use crate::oauth::{OAuthSettings, AUTHORIZATION_SERVER_PATH, PROTECTED_RESOURCE_PATH};

/// RFC 9728 Protected Resource Metadata for `<public_url>/mcp`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProtectedResourceMetadata {
    /// The resource identifier: `<public_url>/mcp`, the value a token's
    /// `resource` must equal.
    pub resource: String,
    /// The one authorization server: this server's issuer.
    pub authorization_servers: Vec<String>,
    /// Tokens travel only in the `Authorization` header.
    // `&'static [&'static str]`: a fixed list compiled into the binary;
    // serde writes it as a JSON array, with no allocation.
    pub bearer_methods_supported: &'static [&'static str],
    /// A human-readable name for the resource.
    pub resource_name: &'static str,
}

impl ProtectedResourceMetadata {
    /// The document for these settings.
    ///
    /// # Arguments
    /// * `settings` - the OAuth settings (issuer and resource).
    ///
    /// # Returns
    /// The document.
    pub fn new(settings: &OAuthSettings) -> Self {
        Self {
            resource: settings.resource.clone(),
            authorization_servers: vec![settings.issuer.clone()],
            bearer_methods_supported: &["header"],
            resource_name: "okf-mcp",
        }
    }
}

/// RFC 8414 Authorization Server Metadata for the built-in server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AuthorizationServerMetadata {
    /// The issuer: `<public_url>`, also sent as `iss` on authorization responses.
    pub issuer: String,
    /// `<public_url>/oauth/authorize`.
    pub authorization_endpoint: String,
    /// `<public_url>/oauth/token`.
    pub token_endpoint: String,
    /// `<public_url>/oauth/register`, left out when DCR is off.
    // `skip_serializing_if`: serde omits the key entirely for `None`,
    // instead of writing `null`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub registration_endpoint: Option<String>,
    /// `<public_url>/oauth/revoke`.
    pub revocation_endpoint: String,
    /// Only the authorization code flow.
    pub response_types_supported: &'static [&'static str],
    /// Authorization codes and refresh tokens.
    pub grant_types_supported: &'static [&'static str],
    /// PKCE with S256 only.
    pub code_challenge_methods_supported: &'static [&'static str],
    /// Public clients only.
    pub token_endpoint_auth_methods_supported: &'static [&'static str],
    /// Public clients only.
    pub revocation_endpoint_auth_methods_supported: &'static [&'static str],
    /// An `https://` `client_id` is fetched as a Client ID Metadata Document.
    pub client_id_metadata_document_supported: bool,
    /// Authorization responses carry `iss` (RFC 9207).
    pub authorization_response_iss_parameter_supported: bool,
}

impl AuthorizationServerMetadata {
    /// The document for these settings.
    ///
    /// # Arguments
    /// * `settings` - the OAuth settings (issuer and `dcr_enabled`).
    ///
    /// # Returns
    /// The document.
    pub fn new(settings: &OAuthSettings) -> Self {
        let issuer = &settings.issuer;
        Self {
            issuer: issuer.clone(),
            authorization_endpoint: format!("{issuer}/oauth/authorize"),
            token_endpoint: format!("{issuer}/oauth/token"),
            // `bool::then`: `Some(closure())` when true, `None` when false.
            registration_endpoint: settings
                .dcr_enabled
                .then(|| format!("{issuer}/oauth/register")),
            revocation_endpoint: format!("{issuer}/oauth/revoke"),
            response_types_supported: &["code"],
            grant_types_supported: &["authorization_code", "refresh_token"],
            code_challenge_methods_supported: &["S256"],
            token_endpoint_auth_methods_supported: &["none"],
            revocation_endpoint_auth_methods_supported: &["none"],
            client_id_metadata_document_supported: true,
            authorization_response_iss_parameter_supported: true,
        }
    }
}

/// The discovery routes. Mounted only when OAuth is on (Task 2); each
/// handler still answers 404 without it.
///
/// # Returns
/// `GET /.well-known/oauth-protected-resource` (also with the `/mcp`
/// suffix, the path-suffixed form RFC 9728 section 3.1 clients try first) and
/// `GET /.well-known/oauth-authorization-server`.
pub fn routes() -> Router<Arc<App>> {
    Router::new()
        .route(PROTECTED_RESOURCE_PATH, get(protected_resource))
        // `PROTECTED_RESOURCE_PATH` with the resource's path appended (a
        // `const` cannot be spliced into a literal, so it is spelled out).
        .route(
            "/.well-known/oauth-protected-resource/mcp",
            get(protected_resource),
        )
        .route(AUTHORIZATION_SERVER_PATH, get(authorization_server))
}

/// `GET /.well-known/oauth-protected-resource[/mcp]`.
async fn protected_resource(State(app): State<Arc<App>>) -> Response {
    match app.oauth() {
        Some(oauth) => public(ProtectedResourceMetadata::new(&oauth.settings)),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// `GET /.well-known/oauth-authorization-server`.
async fn authorization_server(State(app): State<Arc<App>>) -> Response {
    match app.oauth() {
        Some(oauth) => public(AuthorizationServerMetadata::new(&oauth.settings)),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// A public JSON document, readable cross-origin.
// `impl Serialize` in argument position: any serializable type, chosen at
// compile time (a generic parameter without a name).
fn public(document: impl Serialize) -> Response {
    let mut response = Json(document).into_response();
    response.headers_mut().insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::{HeaderMap, Request};
    use serde_json::{json, Value};
    // `oneshot` sends one request through a router without a socket.
    use tower::ServiceExt;

    use crate::config::Config;
    use crate::mcp::testing::{self, TestApp};

    fn settings(dcr_enabled: bool) -> OAuthSettings {
        OAuthSettings {
            issuer: "https://okf.example.com".into(),
            resource: "https://okf.example.com/mcp".into(),
            resource_metadata_url: "https://okf.example.com/.well-known/oauth-protected-resource"
                .into(),
            access_token_ttl: std::time::Duration::from_secs(3600),
            refresh_token_ttl: std::time::Duration::from_secs(30 * 86_400),
            code_max_expiry: std::time::Duration::from_secs(7 * 86_400),
            dcr_enabled,
            cimd_allow_private: false,
        }
    }

    #[test]
    fn protected_resource_metadata_matches_the_spec() {
        let document = serde_json::to_value(ProtectedResourceMetadata::new(&settings(true)))
            .expect("serializes");

        assert_eq!(
            document,
            json!({
                "resource": "https://okf.example.com/mcp",
                "authorization_servers": ["https://okf.example.com"],
                "bearer_methods_supported": ["header"],
                "resource_name": "okf-mcp"
            })
        );
    }

    #[test]
    fn authorization_server_metadata_matches_the_spec() {
        let document = serde_json::to_value(AuthorizationServerMetadata::new(&settings(true)))
            .expect("serializes");

        assert_eq!(
            document,
            json!({
                "issuer": "https://okf.example.com",
                "authorization_endpoint": "https://okf.example.com/oauth/authorize",
                "token_endpoint": "https://okf.example.com/oauth/token",
                "registration_endpoint": "https://okf.example.com/oauth/register",
                "revocation_endpoint": "https://okf.example.com/oauth/revoke",
                "response_types_supported": ["code"],
                "grant_types_supported": ["authorization_code", "refresh_token"],
                "code_challenge_methods_supported": ["S256"],
                "token_endpoint_auth_methods_supported": ["none"],
                "revocation_endpoint_auth_methods_supported": ["none"],
                "client_id_metadata_document_supported": true,
                "authorization_response_iss_parameter_supported": true
            })
        );
    }

    #[test]
    fn without_dcr_there_is_no_registration_endpoint() {
        let document = serde_json::to_value(AuthorizationServerMetadata::new(&settings(false)))
            .expect("serializes");

        assert!(document.get("registration_endpoint").is_none());
        assert_eq!(document["client_id_metadata_document_supported"], true);
    }

    /// Sends `GET uri` through every OAuth route, as `http::router` mounts them.
    async fn get(test: &TestApp, uri: &str) -> (StatusCode, HeaderMap, Value) {
        let response = crate::oauth::routes()
            .with_state(Arc::clone(&test.app))
            .oneshot(Request::get(uri).body(Body::empty()).expect("request"))
            .await
            .expect("infallible");
        let status = response.status();
        let headers = response.headers().clone();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 16)
            .await
            .expect("body");
        let body = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
        (status, headers, body)
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_routes_serve_both_documents_at_every_path() {
        let config = crate::store::oauth::testing::oauth_config("http://127.0.0.1:8080");
        let test = testing::app_with(&[("main", &[])], config).await;

        for path in [
            "/.well-known/oauth-protected-resource",
            "/.well-known/oauth-protected-resource/mcp",
        ] {
            let (status, headers, body) = get(&test, path).await;
            assert_eq!(status, StatusCode::OK, "{path}");
            assert_eq!(body["resource"], "http://127.0.0.1:8080/mcp", "{path}");
            assert_eq!(headers["access-control-allow-origin"], "*", "{path}");
            assert_eq!(headers[header::CACHE_CONTROL], "no-store", "{path}");
        }
        let (status, headers, body) = get(&test, "/.well-known/oauth-authorization-server").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(headers["access-control-allow-origin"], "*");
        assert_eq!(body["issuer"], "http://127.0.0.1:8080");
        assert_eq!(
            body["registration_endpoint"],
            "http://127.0.0.1:8080/oauth/register"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn without_oauth_the_documents_are_404() {
        let test = testing::app_with(&[("main", &[])], Config::default()).await;

        for path in [
            "/.well-known/oauth-protected-resource",
            "/.well-known/oauth-protected-resource/mcp",
            "/.well-known/oauth-authorization-server",
        ] {
            let (status, _, _) = get(&test, path).await;
            assert_eq!(status, StatusCode::NOT_FOUND, "{path}");
        }
    }
}

//! An in-process OIDC provider: discovery, an empty JWKS, an authorize
//! page that signs the person in at once, and a token endpoint issuing
//! HS256 ID tokens signed with the client secret.

use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::extract::{Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use openidconnect::core::{CoreHmacKey, CoreIdToken, CoreIdTokenClaims, CoreJwsSigningAlgorithm};
use openidconnect::{
    Audience, EmptyAdditionalClaims, EndUserEmail, IssuerUrl, Nonce, StandardClaims,
    SubjectIdentifier,
};

// `super::` rather than `crate::config::`: this file is also compiled
// into the browser tests (`tests/ui_e2e/oidc_mock.rs`), another crate,
// whose parent module provides the same name.
use super::OidcProviderConfig;

/// The client id the mock expects.
pub(crate) const CLIENT_ID: &str = "okf-console";
/// The client secret, also the HS256 key.
pub(crate) const SECRET: &str = "test-secret-at-least-32-bytes-long!!";

/// The claims the next ID token carries.
#[derive(Debug, Clone)]
pub(crate) struct NextToken {
    subject: String,
    email: Option<String>,
    email_verified: Option<bool>,
    nonce: Option<String>,
    /// Overrides the `iss` claim (a token from another issuer).
    issuer: Option<String>,
    /// Overrides the `aud` claim (a token for another client).
    audience: Option<String>,
    /// Issues a token that expired a minute ago.
    expired: bool,
}

/// A running mock provider.
#[derive(Clone)]
pub(crate) struct MockIdp {
    /// `http://127.0.0.1:<port>`.
    pub issuer: String,
    next: Arc<Mutex<NextToken>>,
    discoveries: Arc<AtomicUsize>,
    /// Answer discovery with 503 while set.
    pub discovery_down: Arc<AtomicBool>,
}

impl MockIdp {
    /// Sets the subject and email claims of the next ID tokens.
    pub(crate) fn will_issue(
        &self,
        subject: &str,
        email: Option<&str>,
        email_verified: Option<bool>,
    ) {
        let mut next = self.next.lock().expect("mock lock");
        next.subject = subject.to_string();
        next.email = email.map(str::to_string);
        next.email_verified = email_verified;
    }

    /// Makes the next ID tokens bad: another `iss`, another `aud`, or
    /// already expired.
    pub(crate) fn will_sign_badly(
        &self,
        issuer: Option<&str>,
        audience: Option<&str>,
        expired: bool,
    ) {
        let mut next = self.next.lock().expect("mock lock");
        next.issuer = issuer.map(str::to_string);
        next.audience = audience.map(str::to_string);
        next.expired = expired;
    }

    /// Sets the nonce of the next ID tokens.
    pub(crate) fn set_nonce(&self, nonce: &str) {
        self.next.lock().expect("mock lock").nonce = Some(nonce.to_string());
    }

    /// How many discovery requests arrived.
    pub(crate) fn discoveries(&self) -> usize {
        self.discoveries.load(Ordering::SeqCst)
    }
}

/// The `[[auth.oidc]]` entry for the mock, as `example`.
pub(crate) fn provider(idp: &MockIdp, secret_env: &str) -> OidcProviderConfig {
    OidcProviderConfig {
        id: "example".to_string(),
        label: "Example ID".to_string(),
        issuer: idp.issuer.clone(),
        client_id: CLIENT_ID.to_string(),
        client_secret_env: secret_env.to_string(),
        scopes: vec!["openid".to_string(), "email".to_string()],
    }
}

/// Starts a mock provider on a free port; it runs until the test ends.
pub(crate) async fn start() -> MockIdp {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let port = listener.local_addr().expect("address").port();
    let idp = MockIdp {
        issuer: format!("http://127.0.0.1:{port}"),
        next: Arc::new(Mutex::new(NextToken {
            subject: "sub-1".to_string(),
            email: None,
            email_verified: None,
            nonce: None,
            issuer: None,
            audience: None,
            expired: false,
        })),
        discoveries: Arc::default(),
        discovery_down: Arc::default(),
    };
    let router = Router::new()
        .route("/.well-known/openid-configuration", get(discovery))
        .route("/jwks", get(jwks))
        .route("/authorize", get(authorize))
        .route("/token", post(token))
        .with_state(idp.clone());
    tokio::spawn(async move { axum::serve(listener, router).await });
    idp
}

async fn discovery(State(idp): State<MockIdp>) -> Response {
    idp.discoveries.fetch_add(1, Ordering::SeqCst);
    if idp.discovery_down.load(Ordering::SeqCst) {
        return StatusCode::SERVICE_UNAVAILABLE.into_response();
    }
    let issuer = &idp.issuer;
    Json(serde_json::json!({
        "issuer": issuer,
        "authorization_endpoint": format!("{issuer}/authorize"),
        "token_endpoint": format!("{issuer}/token"),
        "jwks_uri": format!("{issuer}/jwks"),
        "response_types_supported": ["code"],
        "subject_types_supported": ["public"],
        "id_token_signing_alg_values_supported": ["HS256"],
    }))
    .into_response()
}

async fn jwks() -> Response {
    (
        [(header::CONTENT_TYPE, "application/json")],
        r#"{"keys":[]}"#,
    )
        .into_response()
}

/// Signs the person in at once: remembers the nonce and sends the
/// browser back with a code.
async fn authorize(
    State(idp): State<MockIdp>,
    Query(query): Query<HashMap<String, String>>,
) -> Response {
    if let Some(nonce) = query.get("nonce") {
        idp.set_nonce(nonce);
    }
    let back = format!(
        "{}?code=c&state={}",
        query
            .get("redirect_uri")
            .map(String::as_str)
            .unwrap_or_default(),
        query.get("state").map(String::as_str).unwrap_or_default(),
    );
    Redirect::to(&back).into_response()
}

async fn token(State(idp): State<MockIdp>, body: String) -> Response {
    if !body.contains("code_verifier=") {
        return (StatusCode::BAD_REQUEST, "no PKCE verifier").into_response();
    }
    let next = idp.next.lock().expect("mock lock").clone();
    let now = chrono::Utc::now();
    let issuer = next.issuer.unwrap_or_else(|| idp.issuer.clone());
    let audience = next.audience.unwrap_or_else(|| CLIENT_ID.to_string());
    let (issued, expires) = if next.expired {
        (
            now - chrono::Duration::minutes(10),
            now - chrono::Duration::minutes(1),
        )
    } else {
        (now, now + chrono::Duration::minutes(5))
    };
    let claims = CoreIdTokenClaims::new(
        IssuerUrl::new(issuer).expect("the issuer"),
        vec![Audience::new(audience)],
        expires,
        issued,
        StandardClaims::new(SubjectIdentifier::new(next.subject))
            .set_email(next.email.map(EndUserEmail::new))
            .set_email_verified(next.email_verified),
        EmptyAdditionalClaims {},
    )
    .set_nonce(next.nonce.map(Nonce::new));
    let id_token = CoreIdToken::new(
        claims,
        &CoreHmacKey::new(SECRET.as_bytes()),
        CoreJwsSigningAlgorithm::HmacSha256,
        None,
        None,
    )
    .expect("sign the ID token");
    Json(serde_json::json!({
        "access_token": "at",
        "token_type": "Bearer",
        "expires_in": 300,
        "id_token": id_token.to_string(),
    }))
    .into_response()
}

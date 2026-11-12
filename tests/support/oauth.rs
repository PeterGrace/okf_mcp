//! The client side of OAuth for end-to-end tests: a server with OAuth on at
//! its own URL, a local Client ID Metadata Document host, and a client that
//! walks discovery, consent (as form POSTs, no browser) and the token
//! endpoint the way an MCP connector would.

// Each test crate that includes `support` uses a different part of it.
#![allow(dead_code)]

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use chrono::{TimeDelta, Utc};
use okf_mcp::archive::Files;
use okf_mcp::config::{Config, OAuthConfig};
use okf_mcp::store::keys::{Access, Grants};
use okf_mcp::store::oauth::codes::{self as code_rows, NewSignInCode};
use sha2::{Digest, Sha256};

use super::server::TestServer;

/// A server with OAuth on at its own base URL (`http://127.0.0.1:<port>`),
/// allowing CIMD documents on loopback hosts over plain http.
///
/// # Arguments
/// * `catalogs` - as for [`TestServer::start`].
///
/// # Returns
/// The running server.
pub async fn start_oauth_server(catalogs: Vec<(&str, Files)>) -> TestServer {
    TestServer::start_with(catalogs, |base| Config {
        public_url: Some(base.to_string()),
        oauth: OAuthConfig {
            cimd_allow_private: true,
            ..OAuthConfig::default()
        },
        ..Config::default()
    })
    .await
}

/// A local host serving a client metadata document at `/client.json`, whose
/// `client_id` is that URL and whose redirect URI is `/callback` on it.
#[derive(Debug, Clone)]
pub struct CimdHost {
    /// `http://127.0.0.1:<port>/client.json`.
    pub client_id: String,
    /// `http://127.0.0.1:<port>/callback` (never fetched: the tests read
    /// the consent page's redirect instead of following it).
    pub redirect_uri: String,
}

impl CimdHost {
    /// Starts the host. It runs until the test's runtime shuts down.
    ///
    /// # Returns
    /// The host's client id and redirect URI.
    pub async fn start() -> Self {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
            .await
            .expect("bind");
        let base = format!("http://{}", listener.local_addr().expect("address"));
        let client_id = format!("{base}/client.json");
        let redirect_uri = format!("{base}/callback");
        let document = serde_json::json!({
            "client_id": client_id,
            "client_name": "Example CIMD Client",
            "redirect_uris": [redirect_uri],
            "grant_types": ["authorization_code", "refresh_token"],
            "response_types": ["code"],
            "token_endpoint_auth_method": "none",
        });
        let router = axum::Router::new().route(
            "/client.json",
            // The handler is called once per request, so it clones the
            // document it captured (`move` gives the closure its own copy).
            axum::routing::get(move || {
                let document = document.clone();
                async move { axum::Json(document) }
            }),
        );
        tokio::spawn(async move { axum::serve(listener, router).await });
        Self {
            client_id,
            redirect_uri,
        }
    }
}

/// A live sign-in code: `access` on `catalogs` (the first is the default),
/// actor `example-actor`.
///
/// # Arguments
/// * `server` - the server whose database stores it.
/// * `access` - `Read` or `Write`.
/// * `catalogs` - the catalogs it grants.
///
/// # Returns
/// The `okfsc_` code.
pub async fn sign_in_code(server: &TestServer, access: Access, catalogs: &[&str]) -> String {
    let names: Vec<String> = catalogs.iter().map(|name| name.to_string()).collect();
    code_rows::create(
        &server.db.pool,
        &NewSignInCode {
            name: "e2e",
            access,
            grants: Grants::Catalogs(&names),
            default_catalog: catalogs.first().copied(),
            actor: "example-actor",
            expires_at: Utc::now() + TimeDelta::hours(1),
            user: crate::support::server::OWNER,
        },
        Utc::now(),
    )
    .await
    .expect("create a sign-in code")
    .code
}

/// `application/x-www-form-urlencoded` text of `pairs`.
pub fn form(pairs: &[(&str, &str)]) -> String {
    form_urlencoded::Serializer::new(String::new())
        .extend_pairs(pairs)
        .finish()
}

/// What discovery found.
#[derive(Debug)]
pub struct Discovered {
    /// The resource the protected resource metadata names.
    pub resource: String,
    /// The authorization server metadata.
    pub metadata: serde_json::Value,
}

/// A token endpoint answer.
#[derive(Debug)]
pub struct TokenAnswer {
    /// The status.
    pub status: reqwest::StatusCode,
    /// The JSON body.
    pub body: serde_json::Value,
}

impl TokenAnswer {
    /// The access token of a successful answer.
    pub fn access(&self) -> String {
        self.body["access_token"]
            .as_str()
            .expect("access_token")
            .to_string()
    }

    /// The refresh token of a successful answer.
    pub fn refresh(&self) -> String {
        self.body["refresh_token"]
            .as_str()
            .expect("refresh_token")
            .to_string()
    }
}

/// One public OAuth client of the server, with its PKCE pair.
pub struct OAuthClient {
    http: reqwest::Client,
    base: String,
    /// Its `client_id`.
    pub client_id: String,
    /// Its redirect URI.
    pub redirect_uri: String,
    verifier: String,
    challenge: String,
}

impl OAuthClient {
    /// A client of `server` with `client_id` and `redirect_uri`.
    pub fn new(server: &TestServer, client_id: &str, redirect_uri: &str) -> Self {
        // 43 random base64url characters: a valid PKCE verifier, made at
        // run time.
        let verifier = okf_mcp::keys::generate_with("").expect("random verifier");
        let challenge = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
        let http = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("HTTP client");
        Self {
            http,
            base: server.base.clone(),
            client_id: client_id.to_string(),
            redirect_uri: redirect_uri.to_string(),
            verifier,
            challenge,
        }
    }

    /// Registers a new client by DCR (RFC 7591) and returns a client for it.
    pub async fn register(server: &TestServer, redirect_uri: &str) -> Self {
        let response = reqwest::Client::new()
            .post(format!("{}/oauth/register", server.base))
            .json(&serde_json::json!({
                "client_name": "Example DCR Client",
                "redirect_uris": [redirect_uri],
                "grant_types": ["authorization_code", "refresh_token"],
                "token_endpoint_auth_method": "none",
            }))
            .send()
            .await
            .expect("register");
        assert_eq!(response.status(), 201);
        let body: serde_json::Value = response.json().await.expect("JSON");
        Self::new(
            server,
            body["client_id"].as_str().expect("client_id"),
            redirect_uri,
        )
    }

    /// Discovery as an MCP client does it: an unauthenticated `/mcp` request,
    /// the `resource_metadata` URL of its 401, that document, then the
    /// authorization server's metadata.
    pub async fn discover(&self) -> Discovered {
        let unauthorized = self
            .http
            .post(format!("{}/mcp", self.base))
            .send()
            .await
            .expect("POST /mcp");
        assert_eq!(unauthorized.status(), 401);
        let challenge = unauthorized.headers()["www-authenticate"]
            .to_str()
            .expect("header")
            .to_string();
        let url = challenge
            .split("resource_metadata=\"")
            .nth(1)
            .and_then(|rest| rest.split('"').next())
            .expect("resource_metadata in WWW-Authenticate")
            .to_string();
        let prm: serde_json::Value = self.get_json(&url).await;
        let issuer = prm["authorization_servers"][0]
            .as_str()
            .expect("an authorization server");
        let metadata = self
            .get_json(&format!("{issuer}/.well-known/oauth-authorization-server"))
            .await;
        Discovered {
            resource: prm["resource"].as_str().expect("resource").to_string(),
            metadata,
        }
    }

    async fn get_json(&self, url: &str) -> serde_json::Value {
        let response = self.http.get(url).send().await.expect("GET");
        assert_eq!(response.status(), 200, "{url}");
        response.json().await.expect("JSON")
    }

    /// Authorize and consent with `sign_in_code`: the step-1 page, the code
    /// POST, then Approve from the summary it shows.
    ///
    /// # Returns
    /// The `okfac_` authorization code.
    ///
    /// # Panics
    /// If any step answers something other than the happy path.
    pub async fn authorize(&self, sign_in_code: &str, resource: &str) -> String {
        let query = form(&[
            ("response_type", "code"),
            ("client_id", &self.client_id),
            ("redirect_uri", &self.redirect_uri),
            ("state", "e2e-state"),
            ("code_challenge", &self.challenge),
            ("code_challenge_method", "S256"),
            ("resource", resource),
            // The admin-minted sign-in code path; without it the browser is
            // sent to `/signin` first.
            ("signin", "code"),
        ]);
        let page = self
            .http
            .get(format!("{}/oauth/authorize?{query}", self.base))
            .send()
            .await
            .expect("authorize");
        assert_eq!(page.status(), 200);
        let page = page.text().await.expect("page");
        let pending = hidden(&page, "pending");
        let csrf = hidden(&page, "csrf");
        let summary = self
            .consent(&[
                ("pending", &pending),
                ("csrf", &csrf),
                ("action", "code"),
                ("sign_in_code", sign_in_code),
            ])
            .await;
        assert_eq!(summary.status(), 200);
        let summary = summary.text().await.expect("summary page");
        let approved = self
            .consent(&[
                ("pending", &pending),
                ("csrf", &csrf),
                ("summary", &hidden(&summary, "summary")),
                ("action", "approve"),
            ])
            .await;
        assert_eq!(approved.status(), 303);
        let location = url::Url::parse(approved.headers()["location"].to_str().expect("location"))
            .expect("URL");
        assert!(location.as_str().starts_with(&self.redirect_uri));
        let param = |name: &str| {
            location
                .query_pairs()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.into_owned())
        };
        assert_eq!(param("state").as_deref(), Some("e2e-state"));
        assert_eq!(param("iss").as_deref(), Some(self.base.as_str()));
        param("code").expect("a code")
    }

    async fn consent(&self, pairs: &[(&str, &str)]) -> reqwest::Response {
        self.http
            .post(format!("{}/oauth/authorize", self.base))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(form(pairs))
            .send()
            .await
            .expect("consent POST")
    }

    async fn token(&self, pairs: &[(&str, &str)]) -> TokenAnswer {
        let response = self
            .http
            .post(format!("{}/oauth/token", self.base))
            .header("content-type", "application/x-www-form-urlencoded")
            .body(form(pairs))
            .send()
            .await
            .expect("token POST");
        let status = response.status();
        TokenAnswer {
            status,
            body: response.json().await.expect("JSON"),
        }
    }

    /// Exchanges an authorization code with this client's verifier.
    pub async fn exchange(&self, code: &str, resource: &str) -> TokenAnswer {
        self.token(&[
            ("grant_type", "authorization_code"),
            ("code", code),
            ("client_id", &self.client_id),
            ("redirect_uri", &self.redirect_uri),
            ("code_verifier", &self.verifier),
            ("resource", resource),
        ])
        .await
    }

    /// Refreshes with `refresh_token`.
    pub async fn refresh(&self, refresh_token: &str) -> TokenAnswer {
        self.token(&[
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", &self.client_id),
        ])
        .await
    }
}

/// The `value` of the hidden input `name` in a consent page.
fn hidden(page: &str, name: &str) -> String {
    let marker = format!("name=\"{name}\" value=\"");
    let start = page.find(&marker).expect("hidden input") + marker.len();
    let end = start + page[start..].find('"').expect("closing quote");
    page[start..end].to_string()
}

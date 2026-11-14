//! End-to-end OAuth (spec "Testing"): discovery, consent, tokens and an MCP
//! tool call with an OAuth access token, over real HTTP with a real rmcp
//! client, beside a static key on the same server.

mod support;

use chrono::{TimeDelta, Utc};
use okf_mcp::keys::{self, ACCESS_TOKEN_PREFIX};
use okf_mcp::store::keys::{self as key_rows, Access};
use okf_mcp::store::oauth::tokens::{self as token_rows, NewToken, TokenKind};
use support::oauth::{sign_in_code, start_oauth_server, CimdHost, OAuthClient, TokenAnswer};
use support::server::{call, files, structured, TestServer};

/// Never fetched: the consent POST's redirect is read, not followed.
const DCR_REDIRECT_URI: &str = "http://127.0.0.1:9/callback";

async fn server() -> TestServer {
    start_oauth_server(vec![
        ("main", files(&[("index.md", "# Main\n")])),
        ("other", files(&[("index.md", "# Other\n")])),
    ])
    .await
}

/// The catalogs a token's connection reaches, by `list_catalogs`.
async fn catalogs_of(server: &TestServer, token: &str) -> Vec<String> {
    let client = server
        .client(token, None)
        .await
        .expect("the token connects");
    let result = call(&client, "list_catalogs", serde_json::json!({})).await;
    let mut names: Vec<String> = structured(&result)["catalogs"]
        .as_array()
        .expect("catalogs")
        .iter()
        .map(|catalog| catalog["name"].as_str().expect("name").to_string())
        .collect();
    names.sort();
    names
}

/// A DCR client that has been through discovery, consent and the token
/// endpoint with a code for `access` on `catalogs`.
async fn signed_in(
    server: &TestServer,
    access: Access,
    catalogs: &[&str],
) -> (OAuthClient, TokenAnswer) {
    let client = OAuthClient::register(server, DCR_REDIRECT_URI).await;
    let resource = client.discover().await.resource;
    let code = client
        .authorize(&sign_in_code(server, access, catalogs).await, &resource)
        .await;
    let tokens = client.exchange(&code, &resource).await;
    assert_eq!(tokens.status, 200, "{}", tokens.body);
    (client, tokens)
}

/// The grant an access token acts for.
async fn grant_of(server: &TestServer, access: &str) -> String {
    let oauth = server.app.oauth().expect("OAuth is on");
    oauth
        .access
        .verify(&server.app, &oauth.settings.resource, access)
        .await
        .expect("verify")
        .expect("a principal")
        .key_id
        .clone()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cimd_client_discovers_consents_and_calls_a_tool() {
    let server = server().await;
    let host = CimdHost::start().await;
    let client = OAuthClient::new(&server, &host.client_id, &host.redirect_uri);

    let found = client.discover().await;
    assert_eq!(found.resource, format!("{}/mcp", server.base));
    assert_eq!(found.metadata["issuer"], server.base.as_str());
    assert_eq!(
        found.metadata["client_id_metadata_document_supported"],
        true
    );
    assert_eq!(
        found.metadata["code_challenge_methods_supported"][0],
        "S256"
    );
    assert_eq!(
        found.metadata["token_endpoint"],
        format!("{}/oauth/token", server.base).as_str()
    );

    let code = client
        .authorize(
            &sign_in_code(&server, Access::Read, &["main"]).await,
            &found.resource,
        )
        .await;
    let tokens = client.exchange(&code, &found.resource).await;
    assert_eq!(tokens.status, 200, "{}", tokens.body);

    assert_eq!(
        catalogs_of(&server, &tokens.access()).await,
        vec!["main".to_string()]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_cimd_client_with_a_portless_loopback_redirect_uri_connects_on_its_port() {
    // Claude Code's metadata document lists `http://127.0.0.1/callback`
    // (and `http://localhost/callback`) with no port, then sends the port
    // it is listening on (RFC 8252 section 7.3).
    let server = server().await;
    let host = CimdHost::start_registering("http://127.0.0.1/callback").await;
    let client = OAuthClient::new(&server, &host.client_id, &host.redirect_uri);
    let resource = client.discover().await.resource;
    assert!(
        url::Url::parse(&host.redirect_uri)
            .expect("URL")
            .port()
            .is_some(),
        "the request names a port the document does not"
    );

    // `authorize` checks the redirect goes to `host.redirect_uri`, port
    // included; `exchange` repeats that URI exactly.
    let code = client
        .authorize(
            &sign_in_code(&server, Access::Read, &["main"]).await,
            &resource,
        )
        .await;
    let tokens = client.exchange(&code, &resource).await;
    assert_eq!(tokens.status, 200, "{}", tokens.body);
    assert_eq!(
        catalogs_of(&server, &tokens.access()).await,
        vec!["main".to_string()]
    );

    // The token request may not drop the port the code was issued for.
    let code = client
        .authorize(
            &sign_in_code(&server, Access::Read, &["main"]).await,
            &resource,
        )
        .await;
    let refused = client
        .exchange_at(&code, &resource, "http://127.0.0.1/callback")
        .await;
    assert_eq!(refused.status, 400, "{}", refused.body);
    assert_eq!(refused.body["error"], "invalid_grant");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_dcr_client_does_the_same() {
    let server = server().await;

    let (_, tokens) = signed_in(&server, Access::Write, &["main", "other"]).await;

    assert_eq!(
        catalogs_of(&server, &tokens.access()).await,
        vec!["main".to_string(), "other".to_string()]
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_token_for_another_resource_is_refused() {
    let server = server().await;
    let (_, tokens) = signed_in(&server, Access::Read, &["main"]).await;
    let grant_id = grant_of(&server, &tokens.access()).await;
    // A live token of the same grant, but issued for another resource: what
    // a token stolen from another MCP server would look like here.
    let foreign = keys::generate_with(ACCESS_TOKEN_PREFIX).expect("token");
    let mut conn = server.db.pool.acquire().await.expect("connection");
    token_rows::insert(
        &mut conn,
        &NewToken {
            sha256: keys::hash(&foreign),
            grant_id: &grant_id,
            kind: TokenKind::Access,
            family_id: "foreign",
            resource: "https://other.example.com/mcp",
            expires_at: Utc::now() + TimeDelta::hours(1),
        },
        Utc::now(),
    )
    .await
    .expect("insert");
    drop(conn);

    assert!(
        server.client(&foreign, None).await.is_err(),
        "wrong audience"
    );
    assert!(
        server.client(&tokens.access(), None).await.is_ok(),
        "right audience"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn refresh_works_and_reusing_the_old_refresh_token_revokes_the_family() {
    let server = server().await;
    let (client, first) = signed_in(&server, Access::Read, &["main"]).await;

    let second = client.refresh(&first.refresh()).await;
    assert_eq!(second.status, 200, "{}", second.body);
    assert_eq!(
        catalogs_of(&server, &second.access()).await,
        vec!["main".to_string()]
    );

    let replay = client.refresh(&first.refresh()).await;
    assert_eq!(replay.status, 400);
    assert_eq!(replay.body["error"], "invalid_grant");
    assert!(
        server.client(&second.access(), None).await.is_err(),
        "family revoked"
    );
    assert!(
        server.client(&first.access(), None).await.is_err(),
        "the first one too"
    );
    assert_eq!(client.refresh(&second.refresh()).await.status, 400);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_static_key_still_works_and_its_401_names_the_metadata() {
    let server = server().await;
    let key = server
        .key("example-key", Access::Read, &["main"], Some("main"))
        .await;

    assert_eq!(catalogs_of(&server, &key).await, vec!["main".to_string()]);
    let refused = reqwest::Client::new()
        .post(server.mcp_url())
        .bearer_auth("okf_not-a-key")
        .send()
        .await
        .expect("POST /mcp");
    assert_eq!(refused.status(), 401);
    assert_eq!(
        refused.headers()["www-authenticate"],
        format!(
            "Bearer resource_metadata=\"{}/.well-known/oauth-protected-resource\"",
            server.base
        )
        .as_str()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_revoked_grant_stops_its_tokens_at_the_next_poll() {
    let server = server().await;
    let (client, tokens) = signed_in(&server, Access::Read, &["main"]).await;
    let grant_id = grant_of(&server, &tokens.access()).await;

    key_rows::revoke_by_id(&server.db.pool, &grant_id, Utc::now())
        .await
        .expect("revoke");
    server.app.poll().await.expect("poll");

    assert!(server.client(&tokens.access(), None).await.is_err());
    assert_eq!(client.refresh(&tokens.refresh()).await.status, 400);
}

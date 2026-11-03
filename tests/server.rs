//! End-to-end tests of the HTTP server (design "Testing"): keys, hosts,
//! `/healthz` and catalog selection, over real HTTP with rmcp's client.

mod support;

use okf_mcp::config::Config;
use okf_mcp::store::keys::{self, Access};
use support::server::{call, files, structured, Client, TestServer};

const INITIALIZE: &str = r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25","capabilities":{},"clientInfo":{"name":"t","version":"0"}}}"#;

/// POSTs an `initialize` to `/mcp`, with a bearer key and a `Host`, if given.
async fn post_initialize(
    server: &TestServer,
    key: Option<&str>,
    host: Option<&str>,
) -> reqwest::StatusCode {
    let mut request = reqwest::Client::new()
        .post(server.mcp_url())
        .header("content-type", "application/json")
        .header("accept", "application/json, text/event-stream")
        .body(INITIALIZE);
    if let Some(key) = key {
        request = request.bearer_auth(key);
    }
    if let Some(host) = host {
        request = request.header("host", host);
    }
    request.send().await.unwrap().status()
}

/// The connection's selection, as `list_catalogs` reports it.
async fn selection(client: Client) -> serde_json::Value {
    let result = call(&client, "list_catalogs", serde_json::json!({})).await;
    structured(&result)["selection"].clone()
}

/// One catalog, `pg`, with one note.
fn pg() -> Vec<(&'static str, okf_mcp::archive::Files)> {
    vec![("pg", files(&[("a.md", "---\ntype: Note\n---\n# A\n")]))]
}

#[tokio::test(flavor = "multi_thread")]
async fn healthz_answers_without_a_key() {
    let server = TestServer::start(pg(), Config::default()).await;

    let response = reqwest::get(format!("{}/healthz", server.base))
        .await
        .unwrap();

    assert_eq!(response.status(), 200);
    let body: serde_json::Value = response.json().await.unwrap();
    assert_eq!(body, serde_json::json!({"database": "ok", "catalogs": 1}));
}

#[tokio::test(flavor = "multi_thread")]
async fn mcp_refuses_a_missing_or_unknown_key() {
    let server = TestServer::start(pg(), Config::default()).await;
    let key = server.key("reader", Access::Read, &[], None).await;

    assert_eq!(post_initialize(&server, None, None).await, 401);
    assert_eq!(post_initialize(&server, Some("not-a-key"), None).await, 401);
    assert_eq!(post_initialize(&server, Some(&key), None).await, 200);
    assert!(server.client("not-a-key", None).await.is_err());
}

#[tokio::test(flavor = "multi_thread")]
async fn a_host_outside_allowed_hosts_is_refused() {
    let server = TestServer::start(pg(), Config::default()).await;
    let key = server.key("reader", Access::Read, &[], None).await;

    assert_eq!(
        post_initialize(&server, Some(&key), Some("evil.example")).await,
        403
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_revoked_key_stops_working_after_a_poll() {
    let server = TestServer::start(pg(), Config::default()).await;
    let key = server.key("reader", Access::Read, &[], Some("pg")).await;
    let client = server.client(&key, None).await.unwrap();
    structured(&call(&client, "list_catalogs", serde_json::json!({})).await);

    keys::revoke(&server.db.pool, "reader", chrono::Utc::now())
        .await
        .unwrap();
    server.app.poll().await.unwrap();

    let arguments = serde_json::json!({}).as_object().unwrap().clone();
    // Bounded: if the client retried a 401 instead of failing, the test
    // must fail rather than hang.
    let call = client.call_tool(
        rmcp::model::CallToolRequestParams::new("list_catalogs").with_arguments(arguments),
    );
    let after = tokio::time::timeout(std::time::Duration::from_secs(10), call)
        .await
        .expect("the call answers within 10 s");
    assert!(after.is_err(), "a revoked key still worked: {after:?}");
    // The server's side, independent of how the client reports it.
    assert_eq!(post_initialize(&server, Some(&key), None).await, 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn selection_takes_the_header_then_the_key_default_then_the_server_default() {
    let catalogs = vec![
        ("pg", files(&[])),
        ("work", files(&[])),
        ("home", files(&[])),
    ];
    let config = Config {
        default_catalog: Some("home".to_string()),
        ..Config::default()
    };
    let server = TestServer::start(catalogs, config).await;
    let with_default = server.key("a", Access::Read, &[], Some("work")).await;
    let without_default = server.key("b", Access::Read, &[], None).await;
    let only_pg = server.key("c", Access::Read, &["pg"], Some("pg")).await;

    let header = selection(server.client(&with_default, Some("pg,work")).await.unwrap()).await;
    let key_default = selection(server.client(&with_default, None).await.unwrap()).await;
    let server_default = selection(server.client(&without_default, None).await.unwrap()).await;
    let client = server.client(&only_pg, None).await.unwrap();
    let listed = call(&client, "list_catalogs", serde_json::json!({})).await;

    assert_eq!(header, serde_json::json!(["pg", "work"]));
    assert_eq!(key_default, serde_json::json!(["work"]));
    assert_eq!(server_default, serde_json::json!(["home"]));
    assert_eq!(structured(&listed)["catalogs"].as_array().unwrap().len(), 1);
}

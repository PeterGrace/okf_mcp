use super::*;
use crate::{
    investigator::{contract::ALLOWED, types::AllowedTool},
    mcp::OkfServer,
};
use axum::{
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use serde_json::json;
use std::sync::{
    atomic::{AtomicUsize, Ordering},
    Arc, Mutex,
};

struct Fake {
    endpoint: String,
    state: Arc<FakeState>,
    task: tokio::task::JoinHandle<()>,
}
struct FakeState {
    calls: Mutex<Vec<Value>>,
    result: Mutex<Value>,
    status: Mutex<Option<StatusCode>>,
    malformed_init: Mutex<bool>,
    init_status: Mutex<Option<StatusCode>>,
    redirect: Mutex<Option<String>>,
    pagination: Mutex<&'static str>,
    page_delay: Mutex<bool>,
    lists: AtomicUsize,
    credentials: AtomicUsize,
    delay: Mutex<bool>,
    sse: Mutex<bool>,
}
impl Drop for Fake {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fake {
    async fn start() -> Self {
        let state = Arc::new(FakeState {
            calls: Mutex::new(vec![]),
            result: Mutex::new(json!({"content":[],"structuredContent":{"hits":[],"skipped":[]}})),
            status: Mutex::new(None),
            malformed_init: Mutex::new(false),
            init_status: Mutex::new(None),
            redirect: Mutex::new(None),
            pagination: Mutex::new("none"),
            page_delay: Mutex::new(false),
            lists: AtomicUsize::new(0),
            credentials: AtomicUsize::new(0),
            delay: Mutex::new(false),
            sse: Mutex::new(false),
        });
        let router = Router::new()
            .route("/mcp", post(handle))
            .with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self {
            endpoint,
            state,
            task,
        }
    }
    async fn client(&self) -> Result<McpClient, RunnerError> {
        McpClient::loopback(&self.endpoint, "pg", "test-token").await
    }
}
async fn handle(
    State(state): State<Arc<FakeState>>,
    headers: HeaderMap,
    Json(request): Json<Value>,
) -> Response {
    if headers.contains_key("authorization") {
        state.credentials.fetch_add(1, Ordering::SeqCst);
    }
    if let Some(location) = state.redirect.lock().unwrap().clone() {
        return (StatusCode::TEMPORARY_REDIRECT, [("location", location)]).into_response();
    }
    let method = request["method"].as_str().unwrap_or("");
    if method == "tools/call" {
        state.calls.lock().unwrap().push(request.clone());
    }
    if let Some(status) = *state.status.lock().unwrap() {
        return (status, "REMOTE_SECRET").into_response();
    }
    if method == "initialize" {
        if let Some(status) = *state.init_status.lock().unwrap() {
            return (status, "REMOTE_SECRET").into_response();
        }
    }
    if method == "initialize" && *state.malformed_init.lock().unwrap() {
        return Json(json!({"jsonrpc":"2.0","id":request["id"],"result":{"protocolVersion":17}}))
            .into_response();
    }
    let result = match method {
        "discovery/discover" | "discover" => return Json(json!({"jsonrpc":"2.0","id":request["id"],"error":{"code":-32601,"message":"Method not found"}})).into_response(),
        "initialize" => json!({"protocolVersion":"2025-03-26","capabilities":{"tools":{}},"serverInfo":{"name":"fake","version":"1"}}),
        "notifications/initialized" | "notifications/cancelled" => return StatusCode::ACCEPTED.into_response(),
        "tools/list" => {
            let index = state.lists.fetch_add(1, Ordering::SeqCst);
            let delay = *state.page_delay.lock().unwrap();
            if delay && index > 0 { tokio::time::sleep(std::time::Duration::from_secs(4)).await; }
            let tools = OkfServer::tools().list_all();
            match *state.pagination.lock().unwrap() {
                "pages" if index == 0 => json!({"tools":&tools[..20],"nextCursor":"next"}),
                "pages" => json!({"tools":&tools[20..]}),
                "cycle" => json!({"tools":[],"nextCursor":"same"}),
                "endless" => json!({"tools":[],"nextCursor":format!("page{index}")}),
                _ => json!({"tools":tools}),
            }
        }
        "tools/call" => {
            let delay = *state.delay.lock().unwrap();
            if delay { tokio::time::sleep(std::time::Duration::from_secs(30)).await; }
            state.result.lock().unwrap().clone()
        }
        _ => return StatusCode::BAD_REQUEST.into_response(),
    };
    let response = json!({"jsonrpc":"2.0","id":request["id"],"result":result});
    if method == "tools/call" && *state.sse.lock().unwrap() {
        return (
            [("content-type", "text/event-stream")],
            format!("data: {response}\n\n"),
        )
            .into_response();
    }
    if method == "initialize" {
        return ([("mcp-session-id", "test-session")], Json(response)).into_response();
    }
    Json(response).into_response()
}
#[tokio::test]
async fn investigator_client_paginated_setup_can_be_dropped_at_deadline_or_cancel() {
    for cancelled in [false, true] {
        let fake = Fake::start().await;
        *fake.state.pagination.lock().unwrap() = "pages";
        *fake.state.page_delay.lock().unwrap() = true;
        let cancel = tokio_util::sync::CancellationToken::new();
        let signal = cancel.clone();
        let state = fake.state.clone();
        let trigger = tokio::spawn(async move {
            while state.lists.load(Ordering::SeqCst) < 2 {
                tokio::task::yield_now().await;
            }
            if cancelled {
                signal.cancel();
            }
        });
        let start = tokio::time::Instant::now();
        let result = tokio::select! {
            _ = cancel.cancelled() => Err(RunnerError::Cancelled),
            _ = tokio::time::sleep(std::time::Duration::from_millis(300)) => Err(RunnerError::Budget),
            result = fake.client() => result.map(|_| ()),
        };
        assert_eq!(
            result,
            Err(if cancelled {
                RunnerError::Cancelled
            } else {
                RunnerError::Budget
            })
        );
        assert!(start.elapsed() < std::time::Duration::from_secs(1));
        assert_eq!(fake.state.lists.load(Ordering::SeqCst), 2);
        assert!(fake.state.calls.lock().unwrap().is_empty());
        tokio::time::timeout(std::time::Duration::from_secs(1), trigger)
            .await
            .unwrap()
            .unwrap();
    }
}

fn request(tool: AllowedTool, args: Value) -> ToolRequest {
    ToolRequest { tool, args }
}
fn search() -> ToolRequest {
    request(AllowedTool::Search, json!({"query":"catalog"}))
}

#[tokio::test]
async fn investigator_client_rejects_redirects_and_cross_catalog() {
    let foreign = Fake::start().await;
    let fake = Fake::start().await;
    *fake.state.redirect.lock().unwrap() = Some(foreign.endpoint.clone());
    assert!(fake.client().await.is_err());
    assert_eq!(foreign.state.credentials.load(Ordering::SeqCst), 0);
    *fake.state.redirect.lock().unwrap() = None;
    let mut client = fake.client().await.unwrap();
    assert!(client.call(search()).await.is_ok());
    assert_eq!(
        fake.state.calls.lock().unwrap()[0]["params"]["arguments"]["catalogs"],
        json!(["pg"])
    );
    *fake.state.result.lock().unwrap() = json!({"content":[],"structuredContent":{"hits":[{"catalog":"other","path":"/x.md"}],"skipped":[]}});
    assert_eq!(client.call(search()).await, Err(RunnerError::Contract));
}

#[test]
fn investigator_client_has_no_write_dispatch() {
    let allowed_tool_names: Vec<_> = ALLOWED.iter().map(AllowedTool::name).collect();
    assert_eq!(allowed_tool_names.len(), 9);
    for name in [
        "publish",
        "maintenance_dispose",
        "edit_document",
        "open_changeset",
    ] {
        assert!(!allowed_tool_names.contains(&name));
        assert!(serde_json::from_value::<AllowedTool>(json!(name)).is_err());
    }
}

#[tokio::test]
async fn investigator_client_pagination_and_cycle_cap() {
    for (mode, ok, pages) in [
        ("pages", true, 2),
        ("cycle", false, 2),
        ("endless", false, 10),
    ] {
        let fake = Fake::start().await;
        *fake.state.pagination.lock().unwrap() = mode;
        assert_eq!(fake.client().await.is_ok(), ok, "{mode}");
        assert_eq!(fake.state.lists.load(Ordering::SeqCst), pages);
    }
}

#[tokio::test]
async fn investigator_client_auth_revocation_and_no_mutation_retry() {
    for status in [StatusCode::UNAUTHORIZED, StatusCode::FORBIDDEN] {
        let fake = Fake::start().await;
        *fake.state.status.lock().unwrap() = Some(status);
        assert!(matches!(fake.client().await, Err(RunnerError::Permission)));
        *fake.state.status.lock().unwrap() = None;
        let mut client = fake.client().await.unwrap();
        *fake.state.status.lock().unwrap() = Some(status);
        assert_eq!(client.call(search()).await, Err(RunnerError::Permission));
        assert_eq!(client.call(search()).await, Err(RunnerError::Permission));
        assert_eq!(fake.state.calls.lock().unwrap().len(), 1);
    }
    for status in [StatusCode::NOT_FOUND, StatusCode::SERVICE_UNAVAILABLE] {
        let fake = Fake::start().await;
        let mut client = fake.client().await.unwrap();
        *fake.state.status.lock().unwrap() = Some(status);
        let expected = if status == StatusCode::NOT_FOUND {
            RunnerError::Contract
        } else {
            RunnerError::Transport
        };
        for attempt in 1..=2 {
            assert_eq!(
                client
                    .call(request(
                        AllowedTool::Claim,
                        json!({"idempotency_key":"key"})
                    ))
                    .await,
                Err(expected)
            );
            let expected_posts = if status == StatusCode::NOT_FOUND {
                1
            } else {
                attempt
            };
            assert_eq!(fake.state.calls.lock().unwrap().len(), expected_posts);
        }
        let calls = fake.state.calls.lock().unwrap();
        if calls.len() == 2 {
            assert_eq!(calls[0]["params"]["name"], calls[1]["params"]["name"]);
            assert_eq!(
                calls[0]["params"]["arguments"],
                calls[1]["params"]["arguments"]
            );
        }
    }
}

#[tokio::test]
async fn investigator_client_init_refusal_is_transport() {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    drop(listener);
    assert!(matches!(
        McpClient::loopback(&endpoint, "pg", "synthetic").await,
        Err(RunnerError::Transport)
    ));
}

#[tokio::test]
async fn investigator_client_init_http_failure_classification() {
    for legacy_phase in [false, true] {
        for (status, expected) in [
            (StatusCode::SERVICE_UNAVAILABLE, RunnerError::Transport),
            (StatusCode::TOO_MANY_REQUESTS, RunnerError::Transport),
            (StatusCode::UNAUTHORIZED, RunnerError::Permission),
            (StatusCode::FORBIDDEN, RunnerError::Permission),
        ] {
            let fake = Fake::start().await;
            if legacy_phase {
                *fake.state.init_status.lock().unwrap() = Some(status);
            } else {
                *fake.state.status.lock().unwrap() = Some(status);
            }
            assert_eq!(
                fake.client().await.err(),
                Some(expected),
                "{status}, legacy={legacy_phase}"
            );
            assert_eq!(
                fake.state.credentials.load(Ordering::SeqCst),
                1,
                "no implicit retry"
            );
            *fake.state.status.lock().unwrap() = None;
            *fake.state.init_status.lock().unwrap() = None;
            *fake.state.malformed_init.lock().unwrap() = true;
            assert_eq!(fake.client().await.err(), Some(RunnerError::Contract));
            *fake.state.malformed_init.lock().unwrap() = false;
            let mut client = fake.client().await.unwrap();
            assert!(client.call(search()).await.is_ok());
        }
    }
}

#[test]
fn investigator_client_init_final_error_is_causal() {
    fn transport() -> ClientInitializeError {
        ClientInitializeError::TransportError {
            error: rmcp::transport::DynamicTransportError::from_parts(
                "test",
                std::any::TypeId::of::<()>(),
                Box::new(StreamableHttpError::Client(RunnerError::Transport)),
            ),
            context: "test".into(),
        }
    }
    for fallback in [
        ClientInitializeError::ExpectedInitResult(None),
        ClientInitializeError::ConnectionClosed("untrusted remote detail".into()),
    ] {
        let error = ClientInitializeError::LegacyFallbackFailed {
            discover: Box::new(transport()),
            fallback: Box::new(fallback),
        };
        assert_eq!(initialization_failure(&error), RunnerError::Contract);
    }
    let error = ClientInitializeError::LegacyFallbackFailed {
        discover: Box::new(ClientInitializeError::ExpectedInitResult(None)),
        fallback: Box::new(transport()),
    };
    assert_eq!(initialization_failure(&error), RunnerError::Transport);
}

#[tokio::test]
async fn investigator_client_transient_does_not_override_later_outcome() {
    for outcome in ["success", "protocol", "auth", "closure"] {
        let fake = Fake::start().await;
        let mut client = fake.client().await.unwrap();
        *fake.state.status.lock().unwrap() = Some(StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(client.call(search()).await, Err(RunnerError::Transport));
        *fake.state.status.lock().unwrap() = None;
        match outcome {
            "protocol" => {
                *fake.state.result.lock().unwrap() =
                    json!({"content":[],"structuredContent":{"hits":"wrong","skipped":[]}})
            }
            "auth" => *fake.state.status.lock().unwrap() = Some(StatusCode::UNAUTHORIZED),
            "closure" => {
                *fake.state.sse.lock().unwrap() = true;
                *fake.state.result.lock().unwrap() = json!({"content":[],"structuredContent":{"hits":[{"catalog":"pg","path":"/x.md","snippet":"x".repeat(1_048_576)}],"skipped":[]}});
            }
            _ => {}
        }
        let result = client.call(search()).await;
        if outcome == "success" {
            assert!(result.is_ok());
            assert!(client.call(search()).await.is_ok());
            assert_eq!(fake.state.calls.lock().unwrap().len(), 3);
        } else {
            let expected = if outcome == "auth" {
                RunnerError::Permission
            } else {
                RunnerError::Contract
            };
            assert_eq!(result, Err(expected));
            assert_eq!(client.call(search()).await, Err(expected));
            assert_eq!(fake.state.calls.lock().unwrap().len(), 2);
        }
    }
}

#[tokio::test]
async fn investigator_client_structured_only_and_payload_caps() {
    let fake = Fake::start().await;
    for (result, sse) in [
        (
            json!({"content":[{"type":"text","text":"{\"hits\":[],\"skipped\":[]}"}]}),
            false,
        ),
        (
            json!({"content":[],"structuredContent":{"hits":"bad","skipped":[]}}),
            false,
        ),
        (
            json!({"content":[],"isError":true,"structuredContent":{"hits":[],"skipped":[]}}),
            false,
        ),
        (
            json!({"content":[],"structuredContent":{"hits":[{"catalog":"pg","path":"/x.md","snippet":"x".repeat(1_048_576)}],"skipped":[]}}),
            false,
        ),
        (
            json!({"content":[],"structuredContent":{"hits":[{"catalog":"pg","path":"/x.md","snippet":"x".repeat(1_048_576)}],"skipped":[]}}),
            true,
        ),
    ] {
        let mut client = fake.client().await.unwrap();
        *fake.state.result.lock().unwrap() = result;
        *fake.state.sse.lock().unwrap() = sse;
        assert!(client.call(search()).await.is_err());
    }
}

#[tokio::test]
async fn investigator_client_sse_cap_is_fatal_without_retry_or_timeout() {
    let fake = Fake::start().await;
    let mut client = fake.client().await.unwrap();
    *fake.state.sse.lock().unwrap() = true;
    assert!(
        client.call(search()).await.is_ok(),
        "small SSE response is supported"
    );
    *fake.state.result.lock().unwrap() = json!({"content":[],"structuredContent":{"hits":[{"catalog":"pg","path":"/x.md","snippet":"x".repeat(1_048_576)}],"skipped":[]}});
    let start = std::time::Instant::now();
    assert_eq!(client.call(search()).await, Err(RunnerError::Contract));
    assert!(start.elapsed() < std::time::Duration::from_secs(2));
    assert_eq!(client.call(search()).await, Err(RunnerError::Contract));
    assert_eq!(fake.state.calls.lock().unwrap().len(), 2);
}

#[test]
fn investigator_client_proxy_environment_is_not_inherited() {
    let result = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "investigator::client::tests::investigator_client_proxy_child",
        ])
        .env("HTTP_PROXY", "http://127.0.0.1:1")
        .env("http_proxy", "http://127.0.0.1:1")
        .env("HTTPS_PROXY", "http://127.0.0.1:1")
        .env("https_proxy", "http://127.0.0.1:1")
        .env("ALL_PROXY", "http://127.0.0.1:1")
        .env("all_proxy", "http://127.0.0.1:1")
        .env("NO_PROXY", "")
        .env("no_proxy", "")
        .env("OKF_MAINTENANCE_TOKEN", "synthetic-test-token")
        .env("ANTHROPIC_API_KEY", "synthetic-unused-provider-key")
        .output()
        .unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stdout)
    );
}

#[tokio::test]
#[ignore = "run by isolated proxy-environment parent"]
async fn investigator_client_proxy_child() {
    let secrets = Secrets::from_env().unwrap();
    let mut config: RunnerConfig = serde_json::from_value(json!({
        "endpoint":"https://localhost:1/mcp", "catalog":"pg", "claude_binary":"/unused/claude",
        "expected_claude_version":"test", "model":"unused", "state_dir":"/unused/state"
    }))
    .unwrap();
    assert!(matches!(
        connect(&config, &secrets).await,
        Err(RunnerError::Config)
    ));
    config.allow_model_upload = true;
    config.acknowledge_inflight_budget_overshoot = true;
    for endpoint in [
        "http://127.0.0.1:1/mcp",
        "https://u:p@localhost:1/mcp",
        "https://localhost:1/mcp#fragment",
    ] {
        config.endpoint = endpoint.into();
        assert!(matches!(
            connect(&config, &secrets).await,
            Err(RunnerError::Config)
        ));
    }
    let fake = Fake::start().await;
    let mut client = fake.client().await.unwrap();
    assert!(client.call(search()).await.is_ok());
}

#[tokio::test]
async fn investigator_client_protocol_fields_not_quoted_catalogs() {
    let fake = Fake::start().await;
    let mut client = fake.client().await.unwrap();
    for args in [
        json!({"query":"q","catalogs":["other"]}),
        json!({"query":"q","catalog":"pg"}),
        json!({"query":"q","actor":"caller"}),
        json!({"query":"q","changeset":"draft"}),
    ] {
        assert!(client
            .call(request(AllowedTool::Search, args))
            .await
            .is_err());
    }
    assert!(fake.state.calls.lock().unwrap().is_empty());
    *fake.state.result.lock().unwrap() = json!({"content":[],"structuredContent":{"catalog":"pg","path":"/x.md","body":"catalog: other","frontmatter":{"catalog":"quoted-other","nested":{"catalogs":["other"]}}}});
    assert!(client
        .call(request(AllowedTool::GetConcept, json!({"path":"/x.md"})))
        .await
        .is_ok());
    *fake.state.result.lock().unwrap() = json!({"content":[],"structuredContent":{"catalog":"pg","pin":{"path":"/x.md","revision":0},"content":"catalog: other","outdated":false}});
    assert!(client
        .call(request(
            AllowedTool::ReadRevision,
            json!({"path":"/x.md","revision":1})
        ))
        .await
        .is_err());
}

#[tokio::test]
async fn investigator_client_https_verifies_certificate_without_provider_panic() {
    use std::process::Stdio;
    let directory = tempfile::tempdir().unwrap();
    let cert = rcgen::generate_simple_self_signed(vec!["localhost".into()]).unwrap();
    std::fs::write(directory.path().join("cert.pem"), cert.cert.pem()).unwrap();
    std::fs::write(
        directory.path().join("key.pem"),
        cert.signing_key.serialize_pem(),
    )
    .unwrap();
    let reservation = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = reservation.local_addr().unwrap();
    drop(reservation);
    let mut server = tokio::process::Command::new("openssl")
        .args([
            "s_server",
            "-accept",
            &address.to_string(),
            "-cert",
            "cert.pem",
            "-key",
            "key.pem",
            "-www",
            "-quiet",
        ])
        .current_dir(directory.path())
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .expect("openssl TLS test prerequisite");
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            assert!(server.try_wait().unwrap().is_none(), "TLS server exited");
            if tokio::net::TcpStream::connect(address).await.is_ok() {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .unwrap();
    let endpoint = format!("https://localhost:{}/mcp", address.port());
    let error = http_client()
        .unwrap()
        .get(&endpoint)
        .send()
        .await
        .unwrap_err();
    assert!(error.is_connect());
    let details = format!("{error:?}");
    assert!(
        details.contains("UnknownIssuer") || details.contains("unknown issuer"),
        "{details}"
    );
    assert!(McpClient::open(&endpoint, "pg", "synthetic").await.is_err());
    server.kill().await.unwrap();
    server.wait().await.unwrap();
}

#[tokio::test]
async fn investigator_client_test_http_gate_is_loopback_only() {
    for endpoint in [
        "http://example.com/mcp",
        "http://127.0.0.1/mcp#fragment",
        "http://u:p@127.0.0.1/mcp",
        "https://127.0.0.1/mcp",
    ] {
        assert!(matches!(
            McpClient::loopback(endpoint, "pg", "synthetic").await,
            Err(RunnerError::Config)
        ));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn investigator_client_real_slice1_http_nine_tools() {
    use crate::{
        config::Config,
        mcp::testing,
        store::keys::{self, Access, Grants, NewKey},
    };
    let text =
        "---\ntype: Note\ntitle: Alpha\ncatalog: quoted-other\n---\n# Alpha\nSynthetic evidence.\n";
    let app = testing::app_with(
        &[("pg", &[("a.md", text)]), ("other", &[("b.md", text)])],
        Config::default(),
    )
    .await;
    let key = keys::create(
        app.app.pool(),
        &NewKey {
            name: "reporter",
            access: Access::Read,
            maintenance_report: true,
            grants: Grants::Catalogs(&["pg".into()]),
            default_catalog: None,
            actor: "reporter",
        },
        chrono::Utc::now(),
    )
    .await
    .unwrap();
    app.app.poll().await.unwrap();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let endpoint = format!("http://{}/mcp", listener.local_addr().unwrap());
    let shutdown = tokio_util::sync::CancellationToken::new();
    let server = tokio::spawn(crate::http::serve(
        app.app.clone(),
        listener,
        shutdown.clone(),
    ));
    let mut client = McpClient::loopback(&endpoint, "pg", &key).await.unwrap();
    let tools = client.list_tools().await.unwrap();
    assert_eq!(tools.len(), 40);
    assert!(client.call(search()).await.is_ok());
    assert!(client
        .call(request(
            AllowedTool::GetConcept,
            json!({"path":"/a.md","links":true,"raw":true})
        ))
        .await
        .is_ok());
    assert!(client
        .call(request(AllowedTool::History, json!({"path":"/a.md"})))
        .await
        .is_ok());
    let claimed = client
        .call(request(
            AllowedTool::Claim,
            json!({"idempotency_key":"claim","batch_size":1}),
        ))
        .await
        .unwrap();
    let lease = claimed["leases"][0].clone();
    assert_eq!(lease["pin"]["revision"], 1);
    client
        .call(request(
            AllowedTool::Renew,
            json!({"work_id":lease["work_id"],"token":lease["token"],"idempotency_key":"renew"}),
        ))
        .await
        .unwrap();
    let pinned = client
        .call(request(AllowedTool::ReadRevision, lease["pin"].clone()))
        .await
        .unwrap();
    assert_eq!(pinned["content"], text);
    let submitted = client.call(request(AllowedTool::Submit, json!({"work_id":lease["work_id"],"token":lease["token"],"idempotency_key":"submit", "submission":{
        "outcome":"inconclusive","assessment":"unverifiable","summary":"Needs a person.",
        "evidence":[{"pin":lease["pin"],"excerpt":"Synthetic evidence."}],"findings":[]
    }}))).await.unwrap();
    assert_eq!(submitted["work_id"], lease["work_id"]);
    client.call(request(AllowedTool::Close, json!({"run_id":claimed["run_id"],"state":"completed","rationale":"Report only.","idempotency_key":"close"}))).await.unwrap();
    let report = client
        .call(request(
            AllowedTool::GetRun,
            json!({"run_id":claimed["run_id"]}),
        ))
        .await
        .unwrap();
    assert_eq!(report["run"]["state"], "completed");
    assert_eq!(report["work"][0]["assessment"], "unverifiable");
    keys::revoke(app.app.pool(), "reporter", chrono::Utc::now())
        .await
        .unwrap();
    app.app.poll().await.unwrap();
    assert_eq!(client.call(search()).await, Err(RunnerError::Permission));
    assert_eq!(client.call(search()).await, Err(RunnerError::Permission));
    drop(client);
    shutdown.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(10), server)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
}

#[tokio::test]
async fn investigator_client_timeout() {
    let fake = Fake::start().await;
    let mut client = fake.client().await.unwrap();
    *fake.state.delay.lock().unwrap() = true;
    let start = std::time::Instant::now();
    assert_eq!(client.call(search()).await, Err(RunnerError::Transport));
    assert!(start.elapsed() < std::time::Duration::from_secs(12));
    assert_eq!(fake.state.calls.lock().unwrap().len(), 1);
}

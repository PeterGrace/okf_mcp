//! The CIMD fetcher ignores proxy settings in the environment: a proxy would
//! resolve and connect to the document's host itself, around the fetcher's
//! guarded resolver. A test binary of its own, because it sets process-wide
//! environment variables that would reach every other test's HTTP clients.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::routing::get;
use axum::Router;
use okf_mcp::oauth::clients::CimdFetcher;

#[tokio::test(flavor = "multi_thread")]
async fn the_cimd_fetcher_ignores_proxy_environment_variables() {
    // A "proxy" that counts the connections it is offered and drops them.
    let proxy = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the proxy");
    let proxy_url = format!("http://{}", proxy.local_addr().expect("address"));
    let offered = Arc::new(AtomicUsize::new(0));
    let counter = Arc::clone(&offered);
    tokio::spawn(async move {
        while let Ok((socket, _)) = proxy.accept().await {
            counter.fetch_add(1, Ordering::SeqCst);
            drop(socket);
        }
    });
    let host = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind the host");
    let document_url = format!("http://{}/client.json", host.local_addr().expect("address"));
    let router = Router::new().route("/client.json", get(|| async { "{}" }));
    tokio::spawn(async move {
        axum::serve(host, router).await.expect("serve");
    });
    // Set before the fetcher is built: reqwest reads proxies at build time.
    // (Edition 2021, so `set_var` is a safe call; nothing else in this
    // binary reads the environment concurrently.)
    for name in ["HTTP_PROXY", "http_proxy", "ALL_PROXY", "all_proxy"] {
        std::env::set_var(name, &proxy_url);
    }
    for name in ["NO_PROXY", "no_proxy"] {
        std::env::remove_var(name);
    }
    let fetcher = CimdFetcher::new(true, true).expect("fetcher");

    let fetched = fetcher.fetch(&document_url.parse().expect("url")).await;

    assert!(fetched.is_ok(), "{fetched:?}");
    assert_eq!(offered.load(Ordering::SeqCst), 0, "the proxy was not used");
}

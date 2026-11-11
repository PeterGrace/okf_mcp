//! The OAuth consent page in a real browser: step 1, step 2, Approve, and
//! the redirect to the client, with no console errors or CSP violations.
//! The client's redirect URI is on another port, so the CSP's
//! `form-action` must name its origin for the hand-off to work.

use axum::response::Html;
use axum::routing::get;
use axum::Router;
use chrono::{TimeDelta, Utc};
use okf_mcp::config::Config;
use okf_mcp::oauth::clients::MAX_UNBOUND_DCR;
use okf_mcp::store::keys::{Access, Grants};
use okf_mcp::store::oauth::clients::{self as client_rows, NewDcrClient};
use okf_mcp::store::oauth::codes::{self as code_rows, NewSignInCode};
use rustwright::GotoOptions;

use crate::harness::drive;
use crate::support::server::{files, TestServer};

/// Serves a page at `/callback` standing in for the client, on its own port.
///
/// # Returns
/// Its base URL, `http://127.0.0.1:<port>`.
async fn callback_host() -> String {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let base = format!("http://{}", listener.local_addr().expect("address"));
    let router = Router::new().route(
        "/callback",
        get(|| async {
            Html("<!DOCTYPE html><title>client</title><p id=\"done\">back at the client</p>")
        }),
    );
    // Runs until the test's runtime shuts down.
    tokio::spawn(async move { axum::serve(listener, router).await });
    base
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_person_enters_a_code_approves_and_lands_back_at_the_client() {
    let server =
        TestServer::start_with(vec![("main", files(&[("index.md", "# Main\n")]))], |base| {
            Config {
                public_url: Some(base.to_string()),
                ..Config::default()
            }
        })
        .await;
    let client_base = callback_host().await;
    let redirect_uri = format!("{client_base}/callback");
    // `insert_dcr` takes the cap of unbound clients and returns the stored
    // client (whose id Postgres picks), or `None` at the cap.
    let redirect_uris = [redirect_uri.clone()];
    let metadata = serde_json::json!({ "redirect_uris": [redirect_uri] });
    let client = client_rows::insert_dcr(
        &server.db.pool,
        &NewDcrClient {
            client_name: Some("Example Client"),
            redirect_uris: &redirect_uris,
            metadata: &metadata,
        },
        Utc::now(),
        MAX_UNBOUND_DCR,
    )
    .await
    .expect("register the client")
    .expect("under the cap");
    let catalogs = ["main".to_string()];
    let code = code_rows::create(
        &server.db.pool,
        &NewSignInCode {
            name: "journey",
            access: Access::Write,
            grants: Grants::Catalogs(&catalogs),
            default_catalog: Some("main"),
            actor: "example-actor",
            expires_at: Utc::now() + TimeDelta::hours(1),
        },
        Utc::now(),
    )
    .await
    .expect("create a sign-in code")
    .code;
    let resource = format!("{}/mcp", server.base);
    let query = form_urlencoded::Serializer::new(String::new())
        .extend_pairs([
            ("response_type", "code"),
            ("client_id", client.client_id.as_str()),
            ("redirect_uri", redirect_uri.as_str()),
            ("state", "xyz"),
            // RFC 7636 appendix B's challenge: a published vector.
            (
                "code_challenge",
                "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
            ),
            ("code_challenge_method", "S256"),
            ("resource", resource.as_str()),
        ])
        .finish();
    let authorize = format!("{}/oauth/authorize?{query}", server.base);

    let ran = drive(server.base.clone(), move |ui| {
        ui.page
            .goto(&authorize, GotoOptions::default())
            .expect("open the consent page");
        ui.wait_text("#consent-client", "Example Client");
        ui.fill("#sign-in-code", &code);
        ui.click("#consent-continue");
        ui.wait_text("#consent-summary", "write");
        assert!(ui.text("#consent-summary").contains("example-actor"));
        ui.click("#consent-approve");
        ui.wait_visible("#done");
        let href = ui.eval("() => location.href", serde_json::json!(null));
        let href = href.as_str().unwrap_or_default();
        assert!(
            href.starts_with(&redirect_uri),
            "back at the client: {href}"
        );
        assert!(href.contains("code=okfac_"), "with a code: {href}");
        assert!(href.contains("state=xyz"), "with the state: {href}");
    })
    .await;
    if !ran {
        return;
    }
    // The approval made one OAuth grant, now in the server's key cache.
    let grants: Vec<_> = okf_mcp::store::keys::list(&server.db.pool)
        .await
        .expect("list keys")
        .into_iter()
        .filter(|row| row.kind == okf_mcp::store::keys::KeyKind::OAuth)
        .collect();
    assert_eq!(grants.len(), 1);
    assert_eq!(
        grants[0].client_id.as_deref(),
        Some(client.client_id.as_str())
    );
    assert!(server.app.keys().find_grant(&grants[0].id).is_some());
}

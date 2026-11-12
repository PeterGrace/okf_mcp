//! The OAuth consent page in a real browser, with no console errors or CSP
//! violations: the sign-in code path (step 1, step 2, Approve), and a
//! person who signs in on the way and approves catalogs from their
//! ceiling. Both end with the redirect to the client. The client's redirect
//! URI is on another port, so the CSP's `form-action` must name its origin
//! for the hand-off to work.

use axum::response::Html;
use axum::routing::get;
use axum::Router;
use chrono::{TimeDelta, Utc};
use okf_mcp::config::Config;
use okf_mcp::oauth::clients::MAX_UNBOUND_DCR;
use okf_mcp::store::ceiling;
use okf_mcp::store::keys::{self as key_rows, Access, Grants, KeyKind, KeyRecord};
use okf_mcp::store::oauth::clients::{self as client_rows, NewDcrClient};
use okf_mcp::store::oauth::codes::{self as code_rows, NewSignInCode};
use rustwright::GotoOptions;

use crate::harness::{drive, Ui};
use crate::support::server::{files, TestServer};

const PASSWORD: &str = "violet-lantern-orbit-42";

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

/// A server whose `public_url` is its own address, a registered DCR client
/// whose redirect URI is [`callback_host`]'s, and the authorize URL for it.
struct Journey {
    server: TestServer,
    client_id: String,
    redirect_uri: String,
}

impl Journey {
    async fn start() -> Self {
        let server = TestServer::start_with(
            vec![
                ("main", files(&[("index.md", "# Main\n")])),
                ("other", files(&[("index.md", "# Other\n")])),
            ],
            |base| Config {
                public_url: Some(base.to_string()),
                ..Config::default()
            },
        )
        .await;
        let redirect_uri = format!("{}/callback", callback_host().await);
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
        Self {
            server,
            client_id: client.client_id,
            redirect_uri,
        }
    }

    /// The authorize URL, with `extra` pairs after the standard ones.
    fn authorize(&self, extra: &[(&str, &str)]) -> String {
        let resource = format!("{}/mcp", self.server.base);
        let query = form_urlencoded::Serializer::new(String::new())
            .extend_pairs([
                ("response_type", "code"),
                ("client_id", self.client_id.as_str()),
                ("redirect_uri", self.redirect_uri.as_str()),
                ("state", "xyz"),
                // RFC 7636 appendix B's challenge: a published vector.
                (
                    "code_challenge",
                    "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
                ),
                ("code_challenge_method", "S256"),
                ("resource", resource.as_str()),
            ])
            .extend_pairs(extra)
            .finish();
        format!("{}/oauth/authorize?{query}", self.server.base)
    }

    /// The OAuth grants, as stored.
    async fn grants(&self) -> Vec<KeyRecord> {
        key_rows::list(&self.server.db.pool)
            .await
            .expect("list keys")
            .into_iter()
            .filter(|row| row.kind == KeyKind::OAuth)
            .collect()
    }
}

/// Waits for the client's page and checks the redirect carried a code and
/// the state.
fn assert_back_at_the_client(ui: &Ui, redirect_uri: &str) {
    ui.wait_visible("#done");
    let href = ui.eval("() => location.href", serde_json::json!(null));
    let href = href.as_str().unwrap_or_default();
    assert!(href.starts_with(redirect_uri), "back at the client: {href}");
    assert!(href.contains("code=okfac_"), "with a code: {href}");
    assert!(href.contains("state=xyz"), "with the state: {href}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_person_enters_a_code_approves_and_lands_back_at_the_client() {
    let journey = Journey::start().await;
    let catalogs = ["main".to_string()];
    let code = code_rows::create(
        &journey.server.db.pool,
        &NewSignInCode {
            name: "journey",
            access: Access::Write,
            grants: Grants::Catalogs(&catalogs),
            default_catalog: Some("main"),
            actor: "example-actor",
            expires_at: Utc::now() + TimeDelta::hours(1),
            user: crate::support::server::OWNER,
        },
        Utc::now(),
    )
    .await
    .expect("create a sign-in code")
    .code;
    // The admin-minted sign-in code path, with no session.
    let authorize = journey.authorize(&[("signin", "code")]);
    let redirect_uri = journey.redirect_uri.clone();

    let ran = drive(journey.server.base.clone(), move |ui| {
        ui.page
            .goto(&authorize, GotoOptions::default())
            .expect("open the consent page");
        ui.wait_text("#consent-client", "Example Client");
        ui.fill("#sign-in-code", &code);
        ui.click("#consent-continue");
        ui.wait_text("#consent-summary", "write");
        assert!(ui.text("#consent-summary").contains("example-actor"));
        ui.click("#consent-approve");
        assert_back_at_the_client(ui, &redirect_uri);
    })
    .await;
    if !ran {
        return;
    }
    // The approval made one OAuth grant, now in the server's key cache.
    let grants = journey.grants().await;
    assert_eq!(grants.len(), 1);
    assert_eq!(
        grants[0].client_id.as_deref(),
        Some(journey.client_id.as_str())
    );
    assert!(!grants[0].self_issued, "a code's grant is admin-issued");
    assert!(journey
        .server
        .app
        .keys()
        .find_grant(&grants[0].id)
        .is_some());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_person_signs_in_on_the_way_and_approves_from_their_ceiling() {
    let journey = Journey::start().await;
    let pool = &journey.server.db.pool;
    journey
        .server
        .user_with_password("alice", "alice@example.com", PASSWORD)
        .await;
    for (catalog, access) in [("main", Access::Write), ("other", Access::Read)] {
        ceiling::entitle(pool, "alice", catalog, access, Utc::now())
            .await
            .expect("entitle");
    }
    journey.server.app.poll().await.expect("poll");
    let authorize = journey.authorize(&[]);
    let redirect_uri = journey.redirect_uri.clone();

    let ran = drive(journey.server.base.clone(), move |ui| {
        ui.page
            .goto(&authorize, GotoOptions::default())
            .expect("open the authorize URL");
        // No session yet: the sign-in page, offering the code path too.
        ui.wait_visible("#signin-form");
        ui.wait_visible("#signin-use-code");
        ui.fill("#signin-email", "alice@example.com");
        ui.fill("#signin-password", PASSWORD);
        ui.click("#signin-submit");
        // Signed in, back at the request: alice's own ceiling.
        ui.wait_text("#consent-user", "alice");
        ui.wait_text("#consent-client", "Example Client");
        ui.click("#consent-catalog-main");
        ui.select("#consent-level-main", "write");
        ui.select("#consent-default", "main");
        ui.click("#consent-approve");
        assert_back_at_the_client(ui, &redirect_uri);
    })
    .await;
    if !ran {
        return;
    }
    // One self-issued grant of alice's: write on main only, now cached.
    let grants = journey.grants().await;
    assert_eq!(grants.len(), 1);
    let grant = &grants[0];
    assert!(grant.self_issued);
    assert_eq!(grant.user.as_deref(), Some("alice"));
    assert_eq!(grant.client_id.as_deref(), Some(journey.client_id.as_str()));
    assert_eq!(
        grant.catalog_access,
        [("main".to_string(), Access::Write)].into_iter().collect()
    );
    assert_eq!(grant.default_catalog.as_deref(), Some("main"));
    let principal = journey
        .server
        .app
        .keys()
        .find_grant(&grant.id)
        .expect("cached grant");
    assert_eq!(principal.level("main"), Some(Access::Write));
    assert_eq!(principal.level("other"), None);
}

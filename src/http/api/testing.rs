//! Test helpers for the `/api/v1` routes: a two-catalog app, real keys at
//! every access level, a request helper over the whole router (auth
//! included), and the access-matrix assertion every area uses.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use chrono::Utc;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

use crate::config::Config;
use crate::http::router;
use crate::mcp::testing::{app_with, TestApp};
use crate::store::keys::{self as key_rows, Access, Grants, NewKey};

/// The synthetic `main` catalog every area's tests read.
pub(crate) const MAIN: &[(&str, &str)] = &[
    ("index.md", "# Main\n\n- [Alpha](systems/alpha.md): the first system\n"),
    ("log.md", "# Log\n"),
    ("AGENTS.md", "# Agents\n"),
    (
        "systems/alpha.md",
        "---\ntype: system\ntitle: Alpha\n---\n# Alpha\n\nAlpha runs [Beta](beta.md).\n\n## Config\n\nNothing yet.\n",
    ),
    ("systems/beta.md", "---\ntype: system\ntitle: Beta\n---\n# Beta\n\nBeta.\n"),
    ("systems/index.md", "# Systems\n\n- [Alpha](alpha.md)\n- [Beta](beta.md)\n"),
];

/// An app holding `main` (from [`MAIN`]) and a one-file `other` catalog.
pub(crate) async fn app() -> TestApp {
    app_with(
        &[("main", MAIN), ("other", &[("index.md", "# Other\n")])],
        Config::default(),
    )
    .await
}

/// Secrets of live keys at each level. `read`, `write` and `admin` reach
/// `main` only; `stranger` is an admin key that reaches only `other`.
pub(crate) struct Keys {
    /// A read key on `main`.
    pub read: String,
    /// A write key on `main`.
    pub write: String,
    /// An admin key on `main`.
    pub admin: String,
    /// An admin key on `other` only.
    pub stranger: String,
}

/// Creates [`Keys`] in the test database and polls, so the server knows them.
pub(crate) async fn keys(test: &TestApp) -> Keys {
    let make = |name: &'static str, access: Access, catalog: &'static str| {
        let pool = test.app.pool().clone();
        async move {
            let grants = [catalog.to_owned()];
            let new = NewKey {
                name,
                access,
                maintenance_report: false,
                grants: Grants::Catalogs(&grants),
                default_catalog: Some(catalog),
                actor: name,
                user: crate::db::TEST_USER,
                self_issued: false,
                expires_at: None,
                levels: &[],
            };
            key_rows::create_with_id(&pool, &new, Utc::now())
                .await
                .expect("create a test key")
                .key
        }
    };
    let keys = Keys {
        read: make("reader", Access::Read, "main").await,
        write: make("writer", Access::Write, "main").await,
        admin: make("admin", Access::Admin, "main").await,
        stranger: make("stranger", Access::Admin, "other").await,
    };
    test.app.poll().await.expect("poll the new keys in");
    keys
}

/// Creates an admin key, named `root`, that reaches every catalog, and
/// polls it in. [`Keys`] has none: its `admin` reaches only `main`, which is
/// what the scoped-admin cases need. Create it once per test, since a name
/// and actor cannot be taken twice.
///
/// # Returns
/// Its secret.
pub(crate) async fn every_catalog_admin(test: &TestApp) -> String {
    let new = NewKey {
        name: "root",
        access: Access::Admin,
        maintenance_report: false,
        grants: Grants::All,
        default_catalog: Some("main"),
        actor: "root",
        user: crate::db::TEST_USER,
        self_issued: false,
        expires_at: None,
        levels: &[],
    };
    let key = key_rows::create_with_id(test.app.pool(), &new, Utc::now())
        .await
        .expect("create the every-catalog admin key")
        .key;
    test.app.poll().await.expect("poll the key in");
    key
}

/// Sends one request through the whole router (auth included).
///
/// # Returns
/// The status and the body as JSON (`Null` when it is not JSON, such as
/// the 401's text body or an export).
pub(crate) async fn send(
    test: &TestApp,
    method: &str,
    uri: &str,
    key: Option<&str>,
    body: Option<serde_json::Value>,
) -> (StatusCode, serde_json::Value) {
    let mut request = Request::builder().method(method).uri(uri);
    if let Some(key) = key {
        request = request.header("authorization", format!("Bearer {key}"));
    }
    let request = match body {
        Some(json) => request
            .header("content-type", "application/json")
            .body(Body::from(json.to_string())),
        None => request.body(Body::empty()),
    }
    .expect("a valid request");
    let response = router(Arc::clone(&test.app), &CancellationToken::new())
        .oneshot(request)
        .await
        .expect("the router answers");
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .expect("read the body");
    (
        status,
        serde_json::from_slice(&bytes).unwrap_or(serde_json::Value::Null),
    )
}

/// One endpoint's expected status per caller.
pub(crate) struct Row {
    /// HTTP method.
    pub method: &'static str,
    /// Full URI, `/api/v1/...`.
    pub uri: String,
    /// JSON body, if the endpoint takes one.
    pub body: Option<serde_json::Value>,
    /// Expected status for the read key.
    pub read: u16,
    /// ... the write key.
    pub write: u16,
    /// ... the admin key on `main`.
    pub admin: u16,
    /// ... the admin key that does not reach `main`.
    pub stranger: u16,
}

/// Asserts every row with fresh [`Keys`]. See [`assert_matrix_with`].
pub(crate) async fn assert_matrix(test: &TestApp, rows: &[Row]) {
    let keys = keys(test).await;
    assert_matrix_with(test, &keys, rows).await;
}

/// Asserts every row with `keys`: 401 without a key, then each caller's
/// status. Use it when the rows need state owned by those keys (a
/// changeset opened by the write key, say), since one name and actor
/// cannot be created twice. Callers run in the order read, write,
/// stranger, admin, so a mutating row that succeeds only for admin runs
/// it last.
pub(crate) async fn assert_matrix_with(test: &TestApp, keys: &Keys, rows: &[Row]) {
    for row in rows {
        let (status, _) = send(test, row.method, &row.uri, None, row.body.clone()).await;
        assert_eq!(
            status.as_u16(),
            401,
            "{} {} without a key",
            row.method,
            row.uri
        );
        for (who, key, want) in [
            ("read", &keys.read, row.read),
            ("write", &keys.write, row.write),
            ("stranger", &keys.stranger, row.stranger),
            ("admin", &keys.admin, row.admin),
        ] {
            let (status, body) =
                send(test, row.method, &row.uri, Some(key), row.body.clone()).await;
            assert_eq!(
                status.as_u16(),
                want,
                "{} {} as {who}: {body}",
                row.method,
                row.uri
            );
        }
    }
}

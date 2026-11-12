//! Test helpers for the `/api/v1` routes: a two-catalog app, real keys at
//! every access level, a request helper over the whole router (auth
//! included), the access-matrix assertion every area uses, and a
//! cookie-keeping [`Browser`] for console sessions (plan Decision 27).

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use axum::body::Body;
use axum::http::{HeaderMap, Request, StatusCode};
use chrono::Utc;
use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use tower::ServiceExt;

use crate::config::Config;
use crate::http::console_session::{start_session, COOKIE, CSRF_HEADER};
use crate::http::router;
use crate::mcp::testing::{app_with, TestApp};
use crate::store::keys::{self as key_rows, Access, Grants, NewKey};
use crate::store::passwords;
use crate::store::users::{self, UserAuth};

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

/// A password every test user can be given: long enough and not common.
pub(crate) const PASSWORD: &str = "violet-lantern-orbit-42";

/// Settings with `public_url`, so the login routes exist (plan Decision 18).
/// The `https` origin also makes every cookie `Secure`.
pub(crate) fn login_config() -> Config {
    Config {
        public_url: Some("https://okf.example.com".to_string()),
        ..Config::default()
    }
}

/// [`app`], with [`login_config`].
pub(crate) async fn login_app() -> TestApp {
    app_with(
        &[("main", MAIN), ("other", &[("index.md", "# Other\n")])],
        login_config(),
    )
    .await
}

/// Creates a user with an email and, optionally, a password, then polls so
/// the key cache knows it.
///
/// # Returns
/// The user.
pub(crate) async fn user_with_password(
    test: &TestApp,
    name: &str,
    email: &str,
    password: Option<&str>,
) -> UserAuth {
    let pool = test.app.pool();
    users::create(
        pool,
        &users::NewUser {
            name,
            display_name: "",
        },
        Utc::now(),
    )
    .await
    .expect("create the user");
    users::set_email(pool, name, Some(email))
        .await
        .expect("set the email");
    let user = users::find_by_email(pool, email)
        .await
        .expect("find the user")
        .expect("the user exists");
    if let Some(password) = password {
        passwords::set(pool, &user.id, password, Utc::now())
            .await
            .expect("set the password");
    }
    test.app.poll().await.expect("poll the user in");
    user
}

/// A request body for [`Browser::send`].
pub(crate) enum Payload {
    /// No body.
    Empty,
    /// `application/json`.
    Json(serde_json::Value),
    /// `application/x-www-form-urlencoded`, as an HTML form posts.
    Form(Vec<(&'static str, String)>),
}

/// One answer to [`Browser::send`].
pub(crate) struct Reply {
    /// The status.
    pub status: StatusCode,
    /// The headers (`Set-Cookie` and `Location` included).
    pub headers: HeaderMap,
    /// The body bytes.
    pub body: Vec<u8>,
}

impl Reply {
    /// The body as JSON, or `Null` when it is not JSON.
    pub(crate) fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).unwrap_or(serde_json::Value::Null)
    }

    /// The status and the body as JSON: the pair [`send`] answers, for
    /// tests written against it.
    pub(crate) fn parts(&self) -> (StatusCode, serde_json::Value) {
        (self.status, self.json())
    }

    /// The body as text.
    pub(crate) fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The `Location` header, or `""`.
    pub(crate) fn location(&self) -> &str {
        self.headers
            .get("location")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
    }

    /// The whole `Set-Cookie` line for `name`, if the reply set it.
    pub(crate) fn set_cookie(&self, name: &str) -> Option<String> {
        let prefix = format!("{name}=");
        self.headers
            .get_all("set-cookie")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .find(|line| line.starts_with(&prefix))
            .map(str::to_string)
    }
}

/// What a browser keeps between requests: its cookies, the console's CSRF
/// token (sent as `x-okf-csrf` on every request but a GET), and optionally
/// a bearer key (sent on every request). Requests go through the whole
/// router with `oneshot`, so they carry no client address. No `Debug`: it
/// holds a session id and a CSRF token.
#[derive(Default)]
pub(crate) struct Browser {
    cookies: BTreeMap<String, String>,
    /// The session's CSRF token.
    pub csrf: Option<String>,
    /// A bearer key.
    pub bearer: Option<String>,
}

impl Browser {
    /// A browser holding one cookie.
    pub(crate) fn with_cookie(name: &str, value: &str) -> Self {
        let mut browser = Self::default();
        browser.cookies.insert(name.to_string(), value.to_string());
        browser
    }

    /// Adds (or replaces) one cookie, as a second tab or a stale jar would
    /// hold it.
    pub(crate) fn add_cookie(&mut self, name: &str, value: &str) {
        self.cookies.insert(name.to_string(), value.to_string());
    }

    /// A cookie's current value.
    pub(crate) fn cookie(&self, name: &str) -> Option<&str> {
        self.cookies.get(name).map(String::as_str)
    }

    /// Sends one request and applies the reply's `Set-Cookie` lines.
    pub(crate) async fn send(
        &mut self,
        test: &TestApp,
        method: &str,
        uri: &str,
        payload: Payload,
    ) -> Reply {
        let mut request = Request::builder().method(method).uri(uri);
        if !self.cookies.is_empty() {
            let cookies: Vec<String> = self
                .cookies
                .iter()
                .map(|(name, value)| format!("{name}={value}"))
                .collect();
            request = request.header("cookie", cookies.join("; "));
        }
        if let Some(csrf) = self.csrf.as_deref().filter(|_| method != "GET") {
            request = request.header(CSRF_HEADER, csrf);
        }
        if let Some(key) = &self.bearer {
            request = request.header("authorization", format!("Bearer {key}"));
        }
        let request = match payload {
            Payload::Empty => request.body(Body::empty()),
            Payload::Json(json) => request
                .header("content-type", "application/json")
                .body(Body::from(json.to_string())),
            Payload::Form(pairs) => {
                let encoded = form_urlencoded::Serializer::new(String::new())
                    .extend_pairs(pairs.iter().map(|(name, value)| (*name, value.as_str())))
                    .finish();
                request
                    .header("content-type", "application/x-www-form-urlencoded")
                    .body(Body::from(encoded))
            }
        }
        .expect("a valid request");
        let response = router(Arc::clone(&test.app), &CancellationToken::new())
            .oneshot(request)
            .await
            .expect("the router answers");
        let status = response.status();
        let headers = response.headers().clone();
        for line in headers
            .get_all("set-cookie")
            .iter()
            .filter_map(|value| value.to_str().ok())
        {
            let pair = line.split(';').next().unwrap_or_default();
            if let Some((name, value)) = pair.split_once('=') {
                if value.is_empty() || line.contains("Max-Age=0;") {
                    self.cookies.remove(name);
                } else {
                    self.cookies.insert(name.to_string(), value.to_string());
                }
            }
        }
        let body = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("read the body")
            .to_vec();
        Reply {
            status,
            headers,
            body,
        }
    }

    /// Signs in with an email and password as `/signin` (Task 7b) does,
    /// without going through the page: the password is checked, then a
    /// session is started. The CSRF token is kept for writes, as the console
    /// keeps it from `GET /api/v1/session`.
    ///
    /// # Panics
    /// If the password is refused.
    pub(crate) async fn signed_in(test: &TestApp, email: &str, password: &str) -> Self {
        let outcome = passwords::verify(test.app.pool(), email, password, Utc::now())
            .await
            .expect("check the password");
        let passwords::SignInOutcome::Ok(passwords::Verified { user, .. }) = outcome else {
            panic!("the password for {email} was refused");
        };
        Self::for_user(test, &user).await
    }

    /// A browser signed in as the existing user called `name`, with no
    /// password (tests of signed-in routes, Tasks 10a to 12). Polls first,
    /// so the key cache knows the user and its ceiling.
    ///
    /// # Panics
    /// If the user does not exist or the session cannot be stored.
    pub(crate) async fn session_for(test: &TestApp, name: &str) -> Self {
        test.app.poll().await.expect("poll the user in");
        let id: String = sqlx::query_scalar("SELECT id FROM users WHERE name = $1")
            .bind(name)
            .fetch_one(test.app.pool())
            .await
            .expect("the user exists");
        let user = users::find_auth(test.app.pool(), &id)
            .await
            .expect("read the user")
            .expect("the user exists");
        Self::for_user(test, &user).await
    }

    /// A browser holding a new console session for `user`.
    async fn for_user(test: &TestApp, user: &UserAuth) -> Self {
        let (_, session) = start_session(&test.app, user, None, "okf-test")
            .await
            .expect("start a console session");
        let mut browser = Self::with_cookie(COOKIE, &session.id);
        // `session.id` was only borrowed above, so the token can move out.
        browser.csrf = Some(session.csrf);
        browser
    }
}

/// Waits (up to 10 s) until `want` backends of this database wait on a
/// lock.
pub(crate) async fn wait_for_lock_waiters(pool: &PgPool, want: i64) {
    for _ in 0..200 {
        let waiting: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pg_stat_activity \
              WHERE datname = current_database() AND wait_event_type = 'Lock'",
        )
        .fetch_one(pool)
        .await
        .expect("count the lock waiters");
        if waiting >= want {
            return;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    panic!("{want} backends never waited on a lock");
}

/// Makes every statement matching `event` (`BEFORE DELETE ON
/// user_sessions`, ...) fail, until [`allow`] drops it.
pub(crate) async fn fail_on(pool: &PgPool, event: &str) {
    sqlx::query(
        "CREATE FUNCTION okf_test_fail() RETURNS trigger LANGUAGE plpgsql AS \
         $$ BEGIN RAISE EXCEPTION 'injected failure'; END $$",
    )
    .execute(pool)
    .await
    .expect("create the failing function");
    // `AssertSqlSafe`: `event` is a literal of the calling test.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "CREATE TRIGGER okf_test_fail {event} EXECUTE FUNCTION okf_test_fail()"
    )))
    .execute(pool)
    .await
    .expect("create the failing trigger");
}

/// Drops [`fail_on`]'s trigger.
pub(crate) async fn allow(pool: &PgPool, table: &str) {
    // `AssertSqlSafe`: `table` is a literal of the calling test.
    sqlx::query(sqlx::AssertSqlSafe(format!(
        "DROP TRIGGER okf_test_fail ON {table}"
    )))
    .execute(pool)
    .await
    .expect("drop the failing trigger");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test(flavor = "multi_thread")]
    async fn the_browser_posts_forms_follows_cookies_and_reads_redirects() {
        let test = login_app().await;
        let mut browser = Browser::default();

        let redirect = browser.send(&test, "GET", "/", Payload::Empty).await;
        let token = browser
            .send(
                &test,
                "POST",
                "/oauth/token",
                Payload::Form(vec![("grant_type", "no-such-grant".to_string())]),
            )
            .await;
        let (status, body) = token.parts();

        assert!(redirect.status.is_redirection());
        assert_eq!(redirect.location(), "/ui/");
        // The token endpoint read the form: it names the grant type it refused.
        assert_eq!(status, StatusCode::BAD_REQUEST, "{}", token.text());
        assert!(body["error"].is_string(), "{body}");
    }
}

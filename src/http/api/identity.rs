//! `GET /api/v1/whoami`: the calling key, grant or console session, so the
//! console can greet it and hide what it cannot do. Never includes the key
//! or its hash. `GET /api/v1/session` and `DELETE /api/v1/session`: the
//! signed-in console session, and signing out (plan Decision 22).

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::Serialize;
use sqlx::PgPool;

use crate::app::App;
use crate::auth::{CredentialKind, Principal, Reach};
use crate::clock::format_timestamp;
use crate::error::OkfError;
use crate::http::console_session::{self, ConsoleSession};
use crate::http::AppError;
use crate::store::keys::{self as key_rows, Access};
use crate::store::{user_sessions, users};

/// The calling key, as the console sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct WhoAmI {
    /// The key's immutable id (`session:<user>` for a console session).
    pub key_id: String,
    /// Its grouping name (`None` for a session, or if the row vanished
    /// meanwhile).
    pub name: Option<String>,
    /// The actor its writes are recorded as.
    pub actor: String,
    /// `read`, `write` or `admin`.
    pub access: &'static str,
    /// Whether it may submit maintenance reports.
    pub maintenance_report: bool,
    /// Whether it reaches every catalog.
    pub all_catalogs: bool,
    /// The loaded catalogs it reaches, sorted.
    pub catalogs: Vec<String>,
    /// Its default catalog.
    pub default_catalog: Option<String>,
    /// `key`, `oauth` for a grant, or `session` for a console session.
    pub kind: &'static str,
    /// A grant's client, as it names itself.
    pub client_name: Option<String>,
    /// The user it belongs to (`None` when unassigned, or the row vanished).
    pub user: Option<String>,
    /// That user's teams, sorted (empty without a user, or once the user
    /// is deleted).
    pub teams: Vec<String>,
}

/// The signed-in console session, as the console reads it at boot.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub(crate) struct SessionView {
    /// The user's name.
    pub user: String,
    /// The user's email.
    pub email: Option<String>,
    /// Whether the user is an admin (the session acts as one).
    pub admin: bool,
    /// The token every write must send as `X-OKF-CSRF`.
    pub csrf: String,
    /// The user's ceiling: catalog name to `read` or `write`.
    pub ceiling: BTreeMap<String, &'static str>,
    /// When the session ends regardless of activity.
    pub expires_at: String,
}

// By hand, not derived, so a stray `{:?}` never prints the CSRF token.
impl std::fmt::Debug for SessionView {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("SessionView")
            .field("user", &self.user)
            .field("admin", &self.admin)
            .field("csrf", &"<redacted>")
            .field("ceiling", &self.ceiling)
            .field("expires_at", &self.expires_at)
            .finish_non_exhaustive()
    }
}

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
        .route("/whoami", get(whoami))
        .route("/session", get(session).delete(sign_out))
}

/// The teams of the user called `name`, sorted.
///
/// # Arguments
/// * `pool` - the database.
/// * `name` - the user's name.
///
/// # Returns
/// Its teams, or none once the user is gone: a user deleted between
/// reading the key and reading the user is not an error here.
///
/// # Errors
/// [`OkfError::Database`].
async fn teams_of(pool: &PgPool, name: &str) -> Result<Vec<String>, OkfError> {
    match users::find(pool, name).await {
        Ok(record) => Ok(record.teams),
        Err(OkfError::UnknownName { .. }) => Ok(Vec::new()),
        Err(other) => Err(other),
    }
}

/// `GET /whoami`.
///
/// # Errors
/// 500 if the key list or the user cannot be read.
async fn whoami(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
) -> Result<Json<WhoAmI>, AppError> {
    let (name, user) = match &principal.kind {
        // A session has no key row: it is its user.
        CredentialKind::Session { .. } => (None, principal.user.clone()),
        CredentialKind::ApiKey | CredentialKind::OAuth { .. } => {
            // The user comes from the row read now, not the principal, in
            // case it was reassigned since the last poll. `unzip` splits an
            // `Option<(a, b)>` into `(Option<a>, Option<b>)`.
            let (name, user) = key_rows::list(app.pool())
                .await?
                .into_iter()
                .find(|record| record.id == principal.key_id)
                .map(|record| (record.name, record.user))
                .unzip();
            // `flatten` turns the `Option<Option<String>>` (row found, user
            // set) into one `Option<String>`.
            (name, user.flatten())
        }
    };
    let teams = match &user {
        Some(name) => teams_of(app.pool(), name).await?,
        None => Vec::new(),
    };
    // `Catalogs` is a BTreeMap, so its keys come out sorted.
    let catalogs = app
        .catalogs()
        .keys()
        .filter(|catalog| principal.reaches(catalog))
        .cloned()
        .collect();
    // `match` on a reference: `client_name` binds a `&Option<String>`.
    let (kind, client_name) = match &principal.kind {
        CredentialKind::ApiKey => ("key", None),
        CredentialKind::OAuth { client_name, .. } => ("oauth", client_name.clone()),
        CredentialKind::Session { .. } => ("session", None),
    };
    Ok(Json(WhoAmI {
        key_id: principal.key_id.clone(),
        name,
        actor: principal.actor.clone(),
        access: principal.access.as_str(),
        maintenance_report: principal.maintenance_report,
        all_catalogs: matches!(principal.reach, Reach::All),
        catalogs,
        default_catalog: principal.default_catalog.clone(),
        kind,
        client_name,
        user,
        teams,
    }))
}

/// The calling session's user id and session, or 403 for a key or grant.
///
/// # Errors
/// [`OkfError::Forbidden`] when the request is not a console session.
fn console(
    principal: &Principal,
    session: Option<Extension<ConsoleSession>>,
) -> Result<(String, ConsoleSession), OkfError> {
    match (&principal.kind, session) {
        (CredentialKind::Session { user_id }, Some(Extension(session))) => {
            Ok((user_id.clone(), session))
        }
        // A key or a grant (or, impossibly, a session principal without its
        // session): there is no console session here.
        _ => Err(OkfError::Forbidden {
            reason: "this request is not a console session".to_string(),
        }),
    }
}

/// `GET /session`: the signed-in user, their ceiling and the CSRF token.
/// The admin flag and the ceiling are the principal's, which the middleware
/// built from the key cache for this request.
///
/// # Errors
/// 403 for a key or grant; 401 once the user is gone; 500 on a lookup failure.
async fn session(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    session: Option<Extension<ConsoleSession>>,
) -> Result<Json<SessionView>, AppError> {
    let (user_id, session) = console(&principal, session)?;
    let user = console_session::session_user(&app, &user_id)
        .await?
        .ok_or_else(|| OkfError::Unauthenticated {
            reason: "this session's user no longer exists".to_string(),
        })?;
    let admin = principal.access == Access::Admin;
    // A non-admin's ceiling is exactly what it reaches; an admin reaches
    // everything, so its own ceiling is shown for information.
    let ceiling = match &principal.reach {
        Reach::Catalogs(levels) => levels.clone(),
        Reach::All => app.keys().ceiling_of(&user_id),
    };
    Ok(Json(SessionView {
        user: user.name,
        email: user.email,
        admin,
        csrf: session.csrf,
        ceiling: ceiling
            .into_iter()
            .map(|(catalog, access)| (catalog, access.as_str()))
            .collect(),
        expires_at: format_timestamp(session.expires_at),
    }))
}

/// `DELETE /session`: signs out (the session is deleted, the cookie
/// cleared). The middleware has already checked the CSRF header.
///
/// # Errors
/// 403 for a key or grant; 500 if the delete fails.
async fn sign_out(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    session: Option<Extension<ConsoleSession>>,
) -> Result<Response, AppError> {
    let (user_id, session) = console(&principal, session)?;
    user_sessions::revoke(app.pool(), &session.id).await?;
    tracing::info!(user = %user_id, "console session signed out");
    Ok((StatusCode::NO_CONTENT, console_session::clear_cookie(&app)).into_response())
}

#[cfg(test)]
mod tests {
    use crate::http::api::testing::{app, assert_matrix, keys, send, Row};
    // `oneshot` sends one request through a router without a socket.
    use tower::ServiceExt;

    #[tokio::test]
    async fn whoami_describes_the_calling_key() {
        let test = app().await;
        let keys = keys(&test).await;
        let (status, body) = send(&test, "GET", "/api/v1/whoami", Some(&keys.read), None).await;
        assert_eq!(status.as_u16(), 200);
        assert_eq!(body["name"], "reader");
        assert_eq!(body["actor"], "reader");
        assert_eq!(body["access"], "read");
        assert_eq!(body["all_catalogs"], false);
        assert_eq!(body["catalogs"], serde_json::json!(["main"]));
        assert_eq!(body["default_catalog"], "main");
        assert_eq!(body["kind"], "key");
        assert_eq!(body["client_name"], serde_json::Value::Null);
        assert!(body.get("sha256").is_none(), "never a hash or secret");
    }

    #[tokio::test]
    async fn whoami_names_the_keys_user_and_its_teams() {
        let test = app().await;
        let keys = keys(&test).await;

        let (status, body) = send(&test, "GET", "/api/v1/whoami", Some(&keys.read), None).await;

        assert_eq!(status.as_u16(), 200);
        assert_eq!(body["user"], crate::db::TEST_USER);
        assert_eq!(body["teams"], serde_json::json!([crate::db::TEST_TEAM]));
    }

    #[tokio::test]
    async fn whoami_of_an_unassigned_key_has_no_user() {
        let test = app().await;
        let key = crate::store::keys::insert_legacy_test_key(
            test.app.pool(),
            "legacy",
            crate::store::keys::Access::Read,
            "legacy",
            chrono::Utc::now(),
        )
        .await;
        // The legacy insert bumps no generation, so a bare poll would skip
        // it; creating the fixture keys bumps it and polls.
        keys(&test).await;

        let (status, body) = send(&test, "GET", "/api/v1/whoami", Some(&key), None).await;

        assert_eq!(status.as_u16(), 200);
        assert_eq!(body["user"], serde_json::Value::Null);
        assert_eq!(body["teams"], serde_json::json!([]));
    }

    #[tokio::test]
    async fn a_deleted_user_has_no_teams_not_an_error() {
        let test = app().await;

        let teams = super::teams_of(test.app.pool(), "no-such-user").await;

        assert_eq!(teams.expect("not an error"), Vec::<String>::new());
    }

    #[tokio::test]
    async fn whoami_needs_any_key() {
        let test = app().await;
        assert_matrix(
            &test,
            &[Row {
                method: "GET",
                uri: "/api/v1/whoami".into(),
                body: None,
                read: 200,
                write: 200,
                admin: 200,
                stranger: 200,
            }],
        )
        .await;
    }

    #[tokio::test]
    async fn api_responses_are_not_cached_or_sniffed() {
        let test = app().await;
        let keys = keys(&test).await;
        let valid = format!("Bearer {}", keys.read);
        // A good key, no key, an unknown key, and an unknown path: the
        // refusals carry the headers too, since `require_key` runs inside them.
        for (uri, authorization, want) in [
            ("/api/v1/whoami", Some(valid.as_str()), 200),
            ("/api/v1/whoami", None, 401),
            ("/api/v1/whoami", Some("Bearer okf_not-a-real-key"), 401),
            ("/api/v1/nothing-here", None, 404),
        ] {
            let mut request = axum::http::Request::builder().uri(uri);
            if let Some(value) = authorization {
                request = request.header("authorization", value);
            }
            let request = request.body(axum::body::Body::empty()).expect("request");
            let response = crate::http::router(
                std::sync::Arc::clone(&test.app),
                &tokio_util::sync::CancellationToken::new(),
            )
            .oneshot(request)
            .await
            .expect("response");
            let headers = response.headers();
            let case = format!("{uri} with {authorization:?}");
            assert_eq!(response.status().as_u16(), want, "{case}");
            assert_eq!(headers["cache-control"], "no-store", "{case}");
            assert_eq!(headers["x-content-type-options"], "nosniff", "{case}");
            if want == 401 {
                assert_eq!(headers["www-authenticate"], "Bearer", "{case}");
            }
        }
    }

    #[test]
    fn a_session_view_never_debug_prints_its_csrf_token() {
        let view = super::SessionView {
            user: "alice".to_string(),
            email: Some("alice@example.com".to_string()),
            admin: false,
            csrf: "csrf-token-secret".to_string(),
            ceiling: std::collections::BTreeMap::new(),
            expires_at: "2026-01-01T00:00:00Z".to_string(),
        };

        let printed = format!("{view:?}");

        assert!(!printed.contains("csrf-token-secret"), "{printed}");
        assert!(printed.contains("alice"), "{printed}");
    }

    #[tokio::test]
    async fn an_unknown_api_path_is_404_not_401() {
        let test = app().await;
        let (status, _) = send(&test, "GET", "/api/v1/nothing-here", None, None).await;
        assert_eq!(status.as_u16(), 404);
    }
}

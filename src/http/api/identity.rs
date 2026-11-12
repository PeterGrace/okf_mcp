//! `GET /api/v1/whoami`: the calling key, so the console can greet it and
//! hide what it cannot do. Never includes the key or its hash.

use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::Serialize;
use sqlx::PgPool;

use crate::app::App;
use crate::auth::{CredentialKind, Principal, Reach};
use crate::error::OkfError;
use crate::http::AppError;
use crate::store::keys as key_rows;
use crate::store::users;

/// The calling key, as the console sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub(crate) struct WhoAmI {
    /// The key's immutable id.
    pub key_id: String,
    /// Its grouping name (`None` if the row vanished meanwhile).
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
    /// `key`, or `oauth` for a grant.
    pub kind: &'static str,
    /// A grant's client, as it names itself.
    pub client_name: Option<String>,
    /// The user it belongs to (`None` when unassigned, or the row vanished).
    pub user: Option<String>,
    /// That user's teams, sorted (empty without a user, or once the user
    /// is deleted).
    pub teams: Vec<String>,
}

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new().route("/whoami", get(whoami))
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
    // The user comes from the row read now, not the principal, in case it
    // was reassigned since the last poll. `unzip` splits an
    // `Option<(a, b)>` into `(Option<a>, Option<b>)`.
    let (name, user) = key_rows::list(app.pool())
        .await?
        .into_iter()
        .find(|record| record.id == principal.key_id)
        .map(|record| (record.name, record.user))
        .unzip();
    // `flatten` turns the `Option<Option<String>>` (row found, user set)
    // into one `Option<String>`.
    let user = user.flatten();
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

    #[tokio::test]
    async fn an_unknown_api_path_is_404_not_401() {
        let test = app().await;
        let (status, _) = send(&test, "GET", "/api/v1/nothing-here", None, None).await;
        assert_eq!(status.as_u16(), 404);
    }
}

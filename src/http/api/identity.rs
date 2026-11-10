//! `GET /api/v1/whoami`: the calling key, so the console can greet it and
//! hide what it cannot do. Never includes the key or its hash.

use std::sync::Arc;

use axum::extract::State;
use axum::routing::get;
use axum::{Extension, Json, Router};
use serde::Serialize;

use crate::app::App;
use crate::auth::{Principal, Reach};
use crate::http::AppError;
use crate::store::keys as key_rows;

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
}

/// This area's routes.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new().route("/whoami", get(whoami))
}

/// `GET /whoami`.
///
/// # Errors
/// 500 if the key list cannot be read.
async fn whoami(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
) -> Result<Json<WhoAmI>, AppError> {
    let name = key_rows::list(app.pool())
        .await?
        .into_iter()
        .find(|record| record.id == principal.key_id)
        .map(|record| record.name);
    // `Catalogs` is a BTreeMap, so its keys come out sorted.
    let catalogs = app
        .catalogs()
        .keys()
        .filter(|catalog| principal.reaches(catalog))
        .cloned()
        .collect();
    Ok(Json(WhoAmI {
        key_id: principal.key_id.clone(),
        name,
        actor: principal.actor.clone(),
        access: principal.access.as_str(),
        maintenance_report: principal.maintenance_report,
        all_catalogs: matches!(principal.reach, Reach::All),
        catalogs,
        default_catalog: principal.default_catalog.clone(),
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
        assert!(body.get("sha256").is_none(), "never a hash or secret");
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

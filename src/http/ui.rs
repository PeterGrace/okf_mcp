//! The web console's static files under `/ui/` (web console design, "Front
//! end"), embedded at build time by `build.rs`, plus the security headers
//! and the page manifest the client router reads.

use axum::extract::Path;
use axum::http::{header, HeaderValue, StatusCode};
use axum::middleware;
use axum::response::{IntoResponse, Redirect, Response};
use axum::routing::get;
use axum::{Json, Router};

// Pastes build.rs's generated `const ASSETS: &[(&str, &[u8])]` here, as if
// written in this file (like a C `#include`).
include!(concat!(env!("OUT_DIR"), "/ui_assets.rs"));

/// The console's Content-Security-Policy: scripts and styles only from this
/// origin (Google Fonts' stylesheet and fonts excepted), no framing.
pub(crate) const CONTENT_SECURITY_POLICY: &str = "default-src 'self'; script-src 'self'; \
    style-src 'self' https://fonts.googleapis.com; font-src https://fonts.gstatic.com; \
    img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'; base-uri 'none'; \
    form-action 'self'";

/// The console's routes. They need no key: the files hold no catalog data.
pub(crate) fn routes<S>() -> Router<S>
where
    S: Clone + Send + Sync + 'static,
{
    Router::new()
        .route("/", get(|| async { Redirect::to("/ui/") }))
        .route("/ui", get(|| async { Redirect::permanent("/ui/") }))
        .route("/ui/", get(|| async { asset("index.html") }))
        .route("/ui/manifest.json", get(manifest))
        .route(
            "/ui/{*path}",
            get(|Path(path): Path<String>| async move { asset(&path) }),
        )
        .layer(middleware::map_response(security_headers))
}

/// The pages this build embeds (`js/pages/<name>.js`), by name.
fn page_names() -> Vec<&'static str> {
    ASSETS
        .iter()
        .filter_map(|(path, _)| path.strip_prefix("js/pages/")?.strip_suffix(".js"))
        .collect()
}

/// `GET /ui/manifest.json`: `{"pages": [...]}`.
async fn manifest() -> Json<serde_json::Value> {
    Json(serde_json::json!({ "pages": page_names() }))
}

/// The media type for an embedded file, by extension.
fn content_type(path: &str) -> &'static str {
    match path.rsplit_once('.').map(|(_, extension)| extension) {
        Some("html") => "text/html; charset=utf-8",
        Some("css") => "text/css; charset=utf-8",
        Some("js") => "text/javascript; charset=utf-8",
        Some("svg") => "image/svg+xml",
        Some("woff2") => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// One embedded file, revalidated on every load (`no-cache`), or 404.
fn asset(path: &str) -> Response {
    match ASSETS.iter().find(|(name, _)| *name == path) {
        Some((name, body)) => (
            [
                (header::CONTENT_TYPE, content_type(name)),
                (header::CACHE_CONTROL, "no-cache"),
            ],
            *body,
        )
            .into_response(),
        None => StatusCode::NOT_FOUND.into_response(),
    }
}

/// Adds the CSP, `nosniff` and `no-referrer` to every console response.
pub(crate) async fn security_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::CONTENT_SECURITY_POLICY,
        HeaderValue::from_static(CONTENT_SECURITY_POLICY),
    );
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    headers.insert(
        header::REFERRER_POLICY,
        HeaderValue::from_static("no-referrer"),
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::http::api::testing::app;
    use axum::body::Body;
    use axum::http::Request;
    use tower::ServiceExt;

    async fn get(path: &str) -> Response {
        let test = app().await;
        crate::http::router(
            std::sync::Arc::clone(&test.app),
            &tokio_util::sync::CancellationToken::new(),
        )
        .oneshot(
            Request::builder()
                .uri(path)
                .body(Body::empty())
                .expect("request"),
        )
        .await
        .expect("response")
    }

    #[tokio::test]
    async fn the_shell_is_served_without_a_key_with_security_headers() {
        let response = get("/ui/").await;
        assert_eq!(response.status(), StatusCode::OK);
        let headers = response.headers();
        assert_eq!(headers["content-type"], "text/html; charset=utf-8");
        assert_eq!(headers["content-security-policy"], CONTENT_SECURITY_POLICY);
        assert_eq!(headers["x-content-type-options"], "nosniff");
        assert_eq!(headers["referrer-policy"], "no-referrer");
    }

    #[tokio::test]
    async fn assets_have_their_content_type_and_unknown_ones_are_404() {
        assert_eq!(
            get("/ui/js/main.js").await.headers()["content-type"],
            "text/javascript; charset=utf-8"
        );
        assert_eq!(
            get("/ui/styles.css").await.headers()["content-type"],
            "text/css; charset=utf-8"
        );
        assert_eq!(get("/ui/nope.js").await.status(), StatusCode::NOT_FOUND);
    }

    #[tokio::test]
    async fn root_and_bare_ui_redirect_to_the_shell() {
        let root = get("/").await;
        assert_eq!(root.status(), StatusCode::SEE_OTHER);
        assert_eq!(root.headers()["location"], "/ui/");
        let bare = get("/ui").await;
        assert_eq!(bare.status(), StatusCode::PERMANENT_REDIRECT);
        assert_eq!(bare.headers()["location"], "/ui/");
    }

    #[test]
    fn the_manifest_lists_embedded_pages_only() {
        let pages = page_names();
        assert!(pages
            .iter()
            .all(|name| !name.contains('/') && !name.ends_with(".js")));
        // Task 1 embeds no page; later tasks add theirs (keys, health, ...).
        assert_eq!(
            pages.len(),
            ASSETS
                .iter()
                .filter(|(p, _)| p.starts_with("js/pages/"))
                .count()
        );
    }

    #[test]
    fn content_types_follow_the_extension() {
        assert_eq!(content_type("a/b.css"), "text/css; charset=utf-8");
        assert_eq!(content_type("x.svg"), "image/svg+xml");
        assert_eq!(content_type("README"), "application/octet-stream");
    }
}

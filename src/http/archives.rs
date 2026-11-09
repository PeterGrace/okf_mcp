//! The archive routes (design "Process and routes"): a catalog's export as a
//! `.tar.gz`, and an import from one.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, FromRequest, Path, Query, Request, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;

use crate::app::App;
use crate::archive::read_tarball;
use crate::auth::Principal;
use crate::error::OkfError;
use crate::http::{timeout_layer, AppError, REQUEST_TIMEOUT};
use crate::mcp::blocking;
use crate::store::export::export;
use crate::store::import::{import, ImportOptions, ImportScope};
use crate::store::keys::Access;

/// The two routes, for the keyed part of the router.
///
/// # Arguments
/// * `app` - the shared state (for `max_import_bytes`).
///
/// # Returns
/// A router over `Arc<App>` state: the export behind the request timeout,
/// the import with its body cap (and its own deadline, see
/// [`import_catalog`]).
pub(crate) fn routes(app: &App) -> Router<Arc<App>> {
    let limit = body_limit(app.config().max_import_bytes.as_u64());
    Router::new()
        // The same timeout as `/healthz` (503 after `REQUEST_TIMEOUT`). A
        // layer on one method router applies to that route only.
        .route(
            "/catalogs/{name}/export",
            get(export_catalog).layer(timeout_layer()),
        )
        // No timeout layer: one firing after the import commits would answer
        // 503 for an import that happened (and a retry would get 409). The
        // handler bounds the upload itself instead.
        .route(
            "/catalogs/{name}/import",
            post(import_catalog).layer(DefaultBodyLimit::max(limit)),
        )
}

/// The request-body cap for an import whose decompressed tar stream may be
/// `max_import_bytes`. gzip makes incompressible input slightly larger (5
/// bytes per stored 64 KiB block, plus an 18-byte header and trailer), so the
/// cap adds 0.1% and 64 KiB: a tarball the CLI would accept is never refused
/// here with 413. `read_tarball` still enforces the exact limit on the
/// decompressed stream.
///
/// # Arguments
/// * `max_import_bytes` - the configured limit.
///
/// # Returns
/// The body cap, saturating at `usize::MAX`.
fn body_limit(max_import_bytes: u64) -> usize {
    let slack = max_import_bytes / 1000 + 64 * 1024;
    // `try_from` fails only for a `u64` that does not fit a `usize` (a
    // 32-bit target), which is larger than any body anyway.
    usize::try_from(max_import_bytes.saturating_add(slack)).unwrap_or(usize::MAX)
}

/// `GET /catalogs/{name}/export`: the published documents as a `.tar.gz`,
/// for a key with `read` on the catalog.
///
/// # Arguments
/// * `app` - the shared state.
/// * `principal` - the caller, from the auth layer.
/// * `name` - the catalog, from the path.
///
/// # Returns
/// The tarball, as `application/gzip` with a download file name.
///
/// # Errors
/// 404 ([`OkfError::CatalogNotSelected`]) for a catalog that does not exist
/// or that the key does not reach; 500 for a database or archive failure.
// Each argument is an extractor: axum fills it from the request (the state,
// the auth layer's principal, the `{name}` path segment), much as a Python
// web framework injects arguments by their annotations.
async fn export_catalog(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(name): Path<String>,
) -> Result<Response, AppError> {
    // `?` returns early with the error, converted to `AppError` by `From`.
    principal.require(Access::Read, &name)?;
    // M3's export builds the whole tarball in memory (a `Vec<u8>`).
    // The generation is for the command line's `import --expect-generation`.
    let (bytes, _generation) = export(app.pool(), &name, Vec::new()).await?;
    // `name` is a catalog that exists, so it is `[a-z0-9-]+` and safe in a header.
    let disposition = format!("attachment; filename=\"{name}.tar.gz\"");
    let headers = [
        (header::CONTENT_TYPE, "application/gzip".to_string()),
        (header::CONTENT_DISPOSITION, disposition),
    ];
    Ok((headers, bytes).into_response())
}

/// `?replace=true` on an import.
#[derive(Debug, Default, Deserialize)]
struct ImportQuery {
    /// Replace a non-empty catalog's documents.
    #[serde(default)]
    replace: bool,
}

/// `POST /catalogs/{name}/import[?replace=true]`: imports the uploaded
/// `.tar.gz` as the key's actor, for a key with `admin` on the catalog, and
/// swaps the new snapshot in before answering, so the next read sees it.
///
/// The key is checked before the body is read, so a key that may not import
/// cannot make the server buffer an upload. Reading and checking the upload
/// must finish within [`REQUEST_TIMEOUT`] (else 503, nothing imported); the
/// import and the refresh after it have no deadline, so a committed import
/// is never answered with a timeout.
///
/// # Arguments
/// * `app` - the shared state.
/// * `principal` - the caller, from the auth layer.
/// * `name` - the catalog, from the path.
/// * `query` - `?replace=true`, if given.
/// * `request` - the rest of the request: its body is the tarball.
///
/// # Returns
/// The import's report as JSON; 413 past [`body_limit`]; 503 if the upload
/// is not read and checked within [`REQUEST_TIMEOUT`].
///
/// # Errors
/// 404 for a catalog that does not exist or that the key does not reach;
/// 403 ([`OkfError::Forbidden`]) for a key below `admin`; 400
/// ([`OkfError::ImportRefused`]) for an unreadable or rule-breaking archive;
/// 409 ([`OkfError::Refused`]) for a non-empty catalog without
/// `?replace=true`; 500 for a database failure. A refresh that fails after
/// the import committed is logged, not returned.
async fn import_catalog(
    State(app): State<Arc<App>>,
    Extension(principal): Extension<Arc<Principal>>,
    Path(name): Path<String>,
    Query(query): Query<ImportQuery>,
    // The whole request, unread, must come last: reading its body consumes
    // it. Taking `Request` rather than `Bytes` defers the read until after
    // the key check.
    request: Request,
) -> Result<Response, AppError> {
    principal.require(Access::Admin, &name)?;
    let max_bytes = app.config().max_import_bytes.as_u64();
    // An `async` block is a future that runs only when awaited (here, under
    // the deadline). Its error is the response to send instead.
    let upload = async {
        // `Bytes::from_request` is what a `Bytes` handler argument runs: it
        // honours the route's `DefaultBodyLimit`, and its rejection is a
        // ready-made response (413 past the cap).
        let body = Bytes::from_request(request, &())
            .await
            .map_err(IntoResponse::into_response)?;
        // Decompressing and checking every entry is CPU work, so it runs on
        // a blocking thread; `move` hands `body` (a cheap reference-counted
        // buffer) to the closure.
        blocking(move || read_tarball(body.as_ref(), max_bytes))
            .await
            .map_err(|error| AppError(error).into_response())
    };
    // `timeout` gives `Err(Elapsed)` if the deadline passes first; dropping
    // the unfinished upload then cancels it, and nothing is imported.
    let files = match tokio::time::timeout(REQUEST_TIMEOUT, upload).await {
        Ok(Ok(files)) => files,
        Ok(Err(refusal)) => return Ok(refusal),
        Err(_elapsed) => return Ok(upload_timed_out()),
    };
    let options = ImportOptions {
        replace: query.replace,
        scope: ImportScope::Everything,
        expect_generation: None,
        actor: &principal.actor,
        now: app.clock().now(),
    };
    let report = import(app.pool(), &name, files, &options)
        .await
        // Without `expect_generation`, `store::import`'s only refusal is a
        // non-empty catalog without `replace`; its hint names the CLI flag,
        // so say what HTTP needs.
        .map_err(|error| match error {
            OkfError::Refused { .. } => OkfError::Refused {
                reason: format!(
                    "catalog {name:?} is not empty; add ?replace=true to replace its contents"
                ),
            },
            other => other,
        })?;
    // Swap the new snapshot in now, not at the next poll. The import has
    // committed, so a failed refresh must not turn it into an error (a
    // retry would then get 409): log it and leave it to the poll loop. A
    // catalog the app has not loaded yet arrives with the next poll. So does
    // a committed import whose client disconnects (dropping this future)
    // during the refresh; a disconnect during `import` itself drops its
    // transaction, which rolls back, and nothing is imported.
    if let Some(state) = app.catalog(&name) {
        if let Err(error) = state.refresh(app.pool()).await {
            tracing::error!(
                catalog = %name,
                %error,
                "refresh after import failed; the poll loop retries"
            );
        }
    }
    // The key's id, never the key itself.
    tracing::info!(catalog = %name, key = %principal.key_id, "imported");
    Ok(Json(report).into_response())
}

/// The answer to an upload not read and checked within [`REQUEST_TIMEOUT`]:
/// 503 with the `{error, message}` JSON every other refusal of the route
/// has. Built here rather than as an [`OkfError`] variant, which only this
/// route would ever return.
///
/// # Returns
/// The response.
fn upload_timed_out() -> Response {
    let body = serde_json::json!({
        "error": "Timeout",
        "message": format!(
            "the upload was not received and checked within {} s; nothing was imported",
            REQUEST_TIMEOUT.as_secs()
        ),
    });
    (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response()
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    // `oneshot` sends one request through a router without a socket.
    use tower::ServiceExt;

    use crate::config::Config;
    use crate::mcp::testing::{app_with, principal};

    /// An import of `size` zero bytes into `pg` by `caller`, through the
    /// routes alone (the auth layer's principal added by hand), with a 1 KiB
    /// `max_import_bytes`.
    async fn import_status(caller: Arc<Principal>, size: usize) -> StatusCode {
        let config = Config {
            max_import_bytes: bytesize::ByteSize::b(1024),
            ..Config::default()
        };
        let test = app_with(&[("pg", &[])], config).await;
        let router = routes(&test.app)
            .layer(Extension(caller))
            .with_state(Arc::clone(&test.app));
        let request = Request::post("/catalogs/pg/import")
            .body(Body::from(vec![0u8; size]))
            .expect("request");
        router.oneshot(request).await.unwrap().status()
    }

    #[tokio::test]
    async fn an_upload_timeout_is_503_with_the_error_json() {
        let response = upload_timed_out();

        assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        let body: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(body["error"], "Timeout");
        assert_eq!(
            body["message"],
            "the upload was not received and checked within 120 s; nothing was imported"
        );
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_key_is_checked_before_the_body_is_read() {
        let reader = principal(&[], None);
        let admin = Arc::new(Principal {
            access: Access::Admin,
            ..(*principal(&[], None)).clone()
        });
        let oversized = body_limit(1024) + 1;

        let as_reader = import_status(reader, oversized).await;
        let as_admin = import_status(admin, oversized).await;

        // A body read first would have made both 413.
        assert_eq!(as_reader, StatusCode::FORBIDDEN);
        assert_eq!(as_admin, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[test]
    fn the_body_limit_leaves_room_for_gzip_overhead() {
        let zero = body_limit(0);
        let default = body_limit(64 << 20);
        let huge = body_limit(u64::MAX);

        assert_eq!(zero, 64 * 1024);
        assert_eq!(default, (64 << 20) + (64 << 20) / 1000 + 64 * 1024);
        assert_eq!(huge, usize::MAX);
    }
}

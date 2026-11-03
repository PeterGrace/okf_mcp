//! The archive routes (design "Process and routes"): a catalog's export as a
//! `.tar.gz`, and an import from one.

use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Extension, Json, Router};
use serde::Deserialize;

use crate::app::App;
use crate::archive::read_tarball;
use crate::auth::Principal;
use crate::error::OkfError;
use crate::http::{timeout_layer, AppError};
use crate::mcp::blocking;
use crate::store::export::export;
use crate::store::import::{import, ImportOptions, ImportReport};
use crate::store::keys::Access;

/// The two routes, for the keyed part of the router.
///
/// # Arguments
/// * `app` - the shared state (for `max_import_bytes`).
///
/// # Returns
/// A router over `Arc<App>` state, with the import's body cap and the
/// request timeout applied.
pub(crate) fn routes(app: &App) -> Router<Arc<App>> {
    let limit = body_limit(app.config().max_import_bytes.as_u64());
    Router::new()
        .route("/catalogs/{name}/export", get(export_catalog))
        .route(
            "/catalogs/{name}/import",
            // A layer on one method router: only the import's body is capped.
            post(import_catalog).layer(DefaultBodyLimit::max(limit)),
        )
        // The same timeout as `/healthz` (503 after `REQUEST_TIMEOUT`).
        .layer(timeout_layer())
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
    let bytes = export(app.pool(), &name, Vec::new()).await?;
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
/// # Arguments
/// * `app` - the shared state.
/// * `principal` - the caller, from the auth layer.
/// * `name` - the catalog, from the path.
/// * `query` - `?replace=true`, if given.
/// * `body` - the tarball (413 past [`body_limit`], before this runs).
///
/// # Returns
/// The import's report.
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
    // The body extractor must come last: it consumes the request.
    body: Bytes,
) -> Result<Json<ImportReport>, AppError> {
    principal.require(Access::Admin, &name)?;
    let max_bytes = app.config().max_import_bytes.as_u64();
    // Decompressing and checking every entry is CPU work, so it runs on a
    // blocking thread; `move` hands `body` (a cheap reference-counted
    // buffer) to the closure.
    let files = blocking(move || read_tarball(body.as_ref(), max_bytes)).await?;
    let options = ImportOptions {
        replace: query.replace,
        actor: &principal.actor,
        now: app.clock().now(),
    };
    let report = import(app.pool(), &name, files, &options)
        .await
        // `store::import`'s only refusal is a non-empty catalog without
        // `replace`; its hint names the CLI flag, so say what HTTP needs.
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
    // catalog the app has not loaded yet arrives with the next poll.
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
    Ok(Json(report))
}

#[cfg(test)]
mod tests {
    use super::*;

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

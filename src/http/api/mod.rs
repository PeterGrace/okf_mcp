//! The JSON REST API under `/api/v1` (web console design, "API"). Handlers
//! are thin adapters: they build the same [`ToolContext`] the MCP tools build
//! and call the same functions, or the `store` functions the CLI calls, so an
//! endpoint behaves exactly like its tool or command.

use std::sync::Arc;

use axum::http::{header, HeaderValue};
use axum::middleware;
use axum::response::Response;
use axum::Router;

use crate::app::App;
use crate::auth::Principal;
use crate::error::OkfError;
use crate::mcp::context::ToolContext;
use crate::store::keys::Access;

mod browse;
mod catalogs;
mod changesets;
mod health;
mod identity;
mod keys;
mod maintenance;
mod search;
#[cfg(test)]
pub(crate) mod testing;

/// Every API route, for nesting at `/api/v1` inside the keyed router.
///
/// # Arguments
/// * `app` - the application (the import route's body limit comes from its config).
///
/// # Returns
/// A router whose responses all carry `Cache-Control: no-store` and `nosniff`.
pub(crate) fn routes(app: &App) -> Router<Arc<App>> {
    Router::new()
        .merge(identity::routes())
        .merge(keys::routes())
        .merge(health::routes())
        .merge(browse::routes())
        .merge(search::routes())
        .merge(catalogs::routes(app))
        .merge(maintenance::routes())
        .merge(changesets::routes())
        .layer(middleware::map_response(api_headers))
}

/// Adds the headers every API response carries: never cached (keys and
/// catalog content are private), never content-sniffed.
async fn api_headers(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
}

/// The tool context for a request, as the MCP layer builds it, with no
/// connection catalogs: each handler passes its catalog as the tool's
/// `catalog` parameter, which wins over every other selection.
///
/// # Arguments
/// * `app` - the application.
/// * `principal` - the calling key, from `require_key`.
///
/// # Returns
/// A context acting as `principal`.
// Its first callers arrive with Tasks 3 to 7; until then the compiler would
// warn that it is unused. Whichever of them merges first removes this attribute.
#[allow(dead_code)]
pub(crate) fn context(app: &Arc<App>, principal: Arc<Principal>) -> ToolContext {
    ToolContext::new(Arc::clone(app), principal, None)
}

/// Refuses a key below `admin`, for the admin routes that name no catalog
/// (keys, catalog creation).
///
/// # Arguments
/// * `principal` - the calling key.
///
/// # Errors
/// [`OkfError::Forbidden`] for a read or write key.
// Its first callers arrive with Tasks 2 and 5; until then the compiler would
// warn that it is unused. Whichever of them merges first removes this attribute.
#[allow(dead_code)]
pub(crate) fn require_admin(principal: &Principal) -> Result<(), OkfError> {
    if principal.access >= Access::Admin {
        Ok(())
    } else {
        Err(OkfError::Forbidden {
            reason: "this needs an admin key".into(),
        })
    }
}

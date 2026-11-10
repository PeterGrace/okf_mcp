//! `/api/v1/catalogs/{catalog}/tree, concepts, links, history and revisions` (web console design, "API": Browse). Filled in by M9 Task 4.

use std::sync::Arc;

use axum::Router;

use crate::app::App;

/// This area's routes.
///
/// # Returns
/// An empty router until Task 4 fills it in.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
}

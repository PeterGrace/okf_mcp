//! `/api/v1/catalogs/{catalog}/changesets` (web console design, "API": Changesets). Filled in by M9 Task 7.

use std::sync::Arc;

use axum::Router;

use crate::app::App;

/// This area's routes.
///
/// # Returns
/// An empty router until Task 7 fills it in.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
}

//! `/api/v1/catalogs/{catalog}/health and reports` (web console design, "API": Health). Filled in by M9 Task 3.

use std::sync::Arc;

use axum::Router;

use crate::app::App;

/// This area's routes.
///
/// # Returns
/// An empty router until Task 3 fills it in.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
}

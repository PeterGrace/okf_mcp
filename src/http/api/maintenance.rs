//! `/api/v1/catalogs/{catalog}/maintenance` (web console design, "API": Maintenance). Filled in by M9 Task 6.

use std::sync::Arc;

use axum::Router;

use crate::app::App;

/// This area's routes.
///
/// # Returns
/// An empty router until Task 6 fills it in.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
}

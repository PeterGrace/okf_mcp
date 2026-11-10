//! `/api/v1/keys` (web console design, "API": Keys). Filled in by M9 Task 2.

use std::sync::Arc;

use axum::Router;

use crate::app::App;

/// This area's routes.
///
/// # Returns
/// An empty router until Task 2 fills it in.
pub(crate) fn routes() -> Router<Arc<App>> {
    Router::new()
}

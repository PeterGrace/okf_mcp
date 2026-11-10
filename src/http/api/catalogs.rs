//! `/api/v1/catalogs` (web console design, "API": Catalogs). Filled in by M9 Task 5.

use std::sync::Arc;

use axum::Router;

use crate::app::App;

/// This area's routes.
///
/// # Returns
/// An empty router until Task 5 fills it in.
pub(crate) fn routes(_app: &App) -> Router<Arc<App>> {
    Router::new()
}

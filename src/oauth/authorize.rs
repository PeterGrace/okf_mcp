//! GET and POST /oauth/authorize: request validation and the consent flow. Filled in by Task 5.

use std::sync::Arc;

use axum::Router;

use crate::app::App;

/// This area's routes (none yet; Task 5).
///
/// # Returns
/// An empty router.
pub fn routes() -> Router<Arc<App>> {
    Router::new()
}

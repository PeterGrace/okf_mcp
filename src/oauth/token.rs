//! POST /oauth/token and POST /oauth/revoke. Filled in by Task 6.

use std::sync::Arc;

use axum::Router;

use crate::app::App;

/// This area's routes (none yet; Task 6).
///
/// # Returns
/// An empty router.
pub fn routes() -> Router<Arc<App>> {
    Router::new()
}

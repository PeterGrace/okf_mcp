//! The well-known documents: Protected Resource Metadata (RFC 9728) and
//! Authorization Server Metadata (RFC 8414). Filled in by Task 3.

use std::sync::Arc;

use axum::Router;

use crate::app::App;

/// This area's routes (none yet; Task 3).
///
/// # Returns
/// An empty router.
pub fn routes() -> Router<Arc<App>> {
    Router::new()
}

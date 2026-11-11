//! Client resolution: dynamic registration (RFC 7591) and the Client ID
//! Metadata Document fetcher. Filled in by Task 3.

use std::sync::Arc;

use axum::Router;

use crate::app::App;
use crate::error::OkfError;

/// Fetches and caches Client ID Metadata Documents (Task 3 gives it its
/// HTTP client and in-flight set).
#[derive(Debug)]
pub struct CimdFetcher {
    // Read by Task 3's fetch; stored now so `OAuth::new` is final.
    #[allow(dead_code)] // Task 3 removes this.
    allow_private: bool,
    #[allow(dead_code)] // Task 3 removes this.
    allow_http: bool,
}

impl CimdFetcher {
    /// A fetcher.
    ///
    /// # Arguments
    /// * `allow_private` - fetch documents on private and loopback addresses
    ///   (`oauth.cimd_allow_private`; test rigs only).
    /// * `allow_http` - also fetch `http://` document URLs (test rigs only:
    ///   set when private addresses are allowed and the issuer is `http://`).
    ///
    /// # Returns
    /// The fetcher.
    ///
    /// # Errors
    /// None yet; Task 3's HTTP client build can fail ([`OkfError::Config`]).
    pub fn new(allow_private: bool, allow_http: bool) -> Result<Self, OkfError> {
        Ok(Self {
            allow_private,
            allow_http,
        })
    }
}

/// This area's routes (none yet; Task 3 adds `POST /oauth/register`).
///
/// # Returns
/// An empty router.
pub fn routes() -> Router<Arc<App>> {
    Router::new()
}

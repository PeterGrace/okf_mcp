//! The OAuth endpoints' errors (RFC 6749 section 5.2, RFC 7591 section 3.2.2,
//! RFC 8707 section 2): `{"error": "<code>", "error_description": "<text>"}`.

use axum::http::{header, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::Json;

use crate::error::OkfError;

/// An OAuth error. Each variant's text is safe to show the client: it never
/// holds a secret. [`OAuthError::Server`] hides its detail.
// `thiserror` writes `Display` from the `#[error]` lines: `{0}` is the
// variant's one field, like a Python f-string.
#[derive(Debug, thiserror::Error)]
pub enum OAuthError {
    /// A parameter is missing, repeated or malformed.
    #[error("{0}")]
    InvalidRequest(String),
    /// The client is unknown or not allowed this flow.
    #[error("{0}")]
    InvalidClient(String),
    /// The code, refresh token or PKCE verifier is wrong, used or expired.
    #[error("{0}")]
    InvalidGrant(String),
    /// The client may not use this grant type.
    #[error("{0}")]
    UnauthorizedClient(String),
    /// A grant type other than `authorization_code` or `refresh_token`.
    #[error("{0}")]
    UnsupportedGrantType(String),
    /// A response type other than `code`.
    #[error("{0}")]
    UnsupportedResponseType(String),
    /// A `resource` other than this server's (RFC 8707).
    #[error("{0}")]
    InvalidTarget(String),
    /// Registration metadata that cannot be accepted (RFC 7591).
    #[error("{0}")]
    InvalidClientMetadata(String),
    /// A registration redirect URI that cannot be accepted (RFC 7591).
    #[error("{0}")]
    InvalidRedirectUri(String),
    /// The person denied the request, or the sign-in failed for good.
    #[error("{0}")]
    AccessDenied(String),
    /// Rate-limited; retry later.
    #[error("too many requests; retry later")]
    SlowDown,
    /// Anything internal (the database). Logged; the client sees only
    /// "internal error".
    // `#[from]` writes `From<OkfError> for OAuthError`, so `?` on a store
    // call inside an OAuth handler converts automatically.
    #[error("internal error")]
    Server(#[from] OkfError),
}

impl OAuthError {
    /// The `error` code.
    ///
    /// # Returns
    /// The RFC's spelling, e.g. `"invalid_grant"`.
    pub fn code(&self) -> &'static str {
        match self {
            OAuthError::InvalidRequest(_) => "invalid_request",
            OAuthError::InvalidClient(_) => "invalid_client",
            OAuthError::InvalidGrant(_) => "invalid_grant",
            OAuthError::UnauthorizedClient(_) => "unauthorized_client",
            OAuthError::UnsupportedGrantType(_) => "unsupported_grant_type",
            OAuthError::UnsupportedResponseType(_) => "unsupported_response_type",
            OAuthError::InvalidTarget(_) => "invalid_target",
            OAuthError::InvalidClientMetadata(_) => "invalid_client_metadata",
            OAuthError::InvalidRedirectUri(_) => "invalid_redirect_uri",
            OAuthError::AccessDenied(_) => "access_denied",
            OAuthError::SlowDown => "slow_down",
            OAuthError::Server(_) => "server_error",
        }
    }

    /// The HTTP status: 401 for `invalid_client`, 429 for `slow_down`, 500
    /// for `server_error`, else 400 (spec "Errors").
    ///
    /// # Returns
    /// The status.
    pub fn status(&self) -> StatusCode {
        match self {
            OAuthError::InvalidClient(_) => StatusCode::UNAUTHORIZED,
            OAuthError::SlowDown => StatusCode::TOO_MANY_REQUESTS,
            OAuthError::Server(_) => StatusCode::INTERNAL_SERVER_ERROR,
            OAuthError::InvalidRequest(_)
            | OAuthError::InvalidGrant(_)
            | OAuthError::UnauthorizedClient(_)
            | OAuthError::UnsupportedGrantType(_)
            | OAuthError::UnsupportedResponseType(_)
            | OAuthError::InvalidTarget(_)
            | OAuthError::InvalidClientMetadata(_)
            | OAuthError::InvalidRedirectUri(_)
            | OAuthError::AccessDenied(_) => StatusCode::BAD_REQUEST,
        }
    }

    /// The body: `{"error", "error_description"}`.
    ///
    /// # Returns
    /// The JSON value.
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "error": self.code(),
            "error_description": self.to_string(),
        })
    }
}

impl IntoResponse for OAuthError {
    fn into_response(self) -> Response {
        if let OAuthError::Server(error) = &self {
            // `%error` logs it with `Display`; an `OkfError` never holds a secret.
            tracing::error!(%error, "oauth request failed");
        } else {
            tracing::debug!(code = self.code(), "oauth request refused");
        }
        let mut response = (self.status(), Json(self.to_json())).into_response();
        let headers = response.headers_mut();
        headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
        if matches!(self, OAuthError::SlowDown) {
            headers.insert(header::RETRY_AFTER, HeaderValue::from_static("10"));
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codes_and_statuses_follow_the_rfcs() {
        let cases = [
            (
                OAuthError::InvalidRequest("x".into()),
                "invalid_request",
                400,
            ),
            (OAuthError::InvalidClient("x".into()), "invalid_client", 401),
            (OAuthError::InvalidGrant("x".into()), "invalid_grant", 400),
            (
                OAuthError::UnauthorizedClient("x".into()),
                "unauthorized_client",
                400,
            ),
            (
                OAuthError::UnsupportedGrantType("x".into()),
                "unsupported_grant_type",
                400,
            ),
            (
                OAuthError::UnsupportedResponseType("x".into()),
                "unsupported_response_type",
                400,
            ),
            (OAuthError::InvalidTarget("x".into()), "invalid_target", 400),
            (
                OAuthError::InvalidClientMetadata("x".into()),
                "invalid_client_metadata",
                400,
            ),
            (
                OAuthError::InvalidRedirectUri("x".into()),
                "invalid_redirect_uri",
                400,
            ),
            (OAuthError::AccessDenied("x".into()), "access_denied", 400),
            (OAuthError::SlowDown, "slow_down", 429),
        ];
        for (error, code, status) in cases {
            assert_eq!(error.code(), code);
            assert_eq!(error.status().as_u16(), status, "{code}");
        }
    }

    #[tokio::test]
    async fn the_response_is_json_uncached_and_hides_internal_detail() {
        let server = OAuthError::Server(OkfError::Refused {
            reason: "a detail the client must not see".into(),
        });

        let response = server.into_response();

        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let body = axum::body::to_bytes(response.into_body(), 1 << 16)
            .await
            .unwrap();
        let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(
            json,
            serde_json::json!({"error": "server_error", "error_description": "internal error"})
        );
    }

    #[tokio::test]
    async fn slow_down_says_when_to_retry() {
        let response = OAuthError::SlowDown.into_response();

        assert_eq!(response.status(), StatusCode::TOO_MANY_REQUESTS);
        assert_eq!(response.headers()["retry-after"], "10");
    }
}

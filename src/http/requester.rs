//! Who is asking, beyond the principal: the client address and whether
//! the request came from a console session. The audit log records both.

use std::net::IpAddr;
use std::sync::Arc;

use axum::extract::FromRequestParts;
use axum::http::request::Parts;

use crate::app::App;
use crate::auth::Principal;
use crate::error::OkfError;
use crate::http::console_session::ConsoleSession;
use crate::http::AppError;
use crate::store::audit::{AuditActor, Source};

/// The client address of a request, as `client_ip` read it; inserted by
/// `require_principal` and `require_key`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ClientIp(pub Option<IpAddr>);

/// A request's address and source.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Requester {
    /// The client address, when known.
    pub ip: Option<IpAddr>,
    /// `Console` for a session cookie, else `Api`.
    pub source: Source,
}

impl Requester {
    /// The requester of a request's parts.
    ///
    /// # Arguments
    /// * `parts` - the request's head, after `require_principal` or
    ///   `require_key` ran.
    ///
    /// # Returns
    /// Its address (from [`ClientIp`]; `None` without one) and source.
    pub fn of(parts: &Parts) -> Self {
        Self {
            // `and_then` flattens the two layers of `Option`: no extension,
            // or an extension holding no address.
            ip: parts.extensions.get::<ClientIp>().and_then(|ip| ip.0),
            source: if parts.extensions.get::<ConsoleSession>().is_some() {
                Source::Console
            } else {
                Source::Api
            },
        }
    }
}

// An extractor: a handler argument `requester: Requester` is filled from
// the request (Python frameworks inject arguments the same way).
impl FromRequestParts<Arc<App>> for Requester {
    // `Infallible`: this extractor never refuses a request.
    type Rejection = std::convert::Infallible;

    async fn from_request_parts(parts: &mut Parts, _: &Arc<App>) -> Result<Self, Self::Rejection> {
        Ok(Self::of(parts))
    }
}

// A handler argument `actor: AuditActor` is the caller, as entries record it.
impl FromRequestParts<Arc<App>> for AuditActor {
    // A route outside the credential middleware has no principal: refused,
    // never recorded as nobody.
    type Rejection = AppError;

    async fn from_request_parts(parts: &mut Parts, _: &Arc<App>) -> Result<Self, Self::Rejection> {
        let principal = parts.extensions.get::<Arc<Principal>>().ok_or_else(|| {
            AppError(OkfError::Forbidden {
                reason: "no credential".to_string(),
            })
        })?;
        Ok(AuditActor::of(principal, &Requester::of(parts)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::ConnectInfo;
    use axum::http::Request;
    use axum::middleware::from_fn_with_state;
    use axum::routing::get;
    use axum::Router;
    // `oneshot` sends one request through a router without a socket.
    use tower::ServiceExt;

    use crate::auth::require_key;
    use crate::http::api::testing::{keys, Browser};
    use crate::http::console_session::{require_principal, COOKIE};
    use crate::store::audit::Via;

    /// A route that answers what the extractors saw. The caller adds the
    /// real middleware and the state.
    fn echo() -> Router<Arc<App>> {
        Router::new().route(
            "/",
            get(|requester: Requester, actor: AuditActor| async move {
                format!(
                    "{:?} {:?} {:?} {}",
                    requester.ip,
                    requester.source,
                    actor.via,
                    actor.user_id.is_some()
                )
            }),
        )
    }

    /// Sends `GET /` from the peer `192.0.2.5` with these headers.
    async fn body_of(router: Router, headers: &[(&str, String)]) -> String {
        let mut request = Request::builder().uri("/");
        for (name, value) in headers {
            request = request.header(*name, value);
        }
        let mut request = request.body(Body::empty()).unwrap();
        let peer: std::net::SocketAddr = "192.0.2.5:4000".parse().unwrap();
        request.extensions_mut().insert(ConnectInfo(peer));
        let response = router.oneshot(request).await.unwrap();
        let bytes = axum::body::to_bytes(response.into_body(), 1 << 16)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn both_middlewares_record_the_client_ip_and_the_credentials_user() {
        let test = crate::http::api::testing::app().await;
        let bearer = vec![(
            "authorization",
            format!("Bearer {}", keys(&test).await.admin),
        )];
        let browser = Browser::session_for(&test, crate::db::TEST_USER).await;
        let cookie = vec![(
            "cookie",
            format!("{COOKIE}={}", browser.cookie(COOKIE).unwrap()),
        )];
        // `from_fn_with_state` wraps the real middleware around the route;
        // `with_state` hands both the shared `App`.
        let api = || {
            echo()
                .route_layer(from_fn_with_state(Arc::clone(&test.app), require_principal))
                .with_state(Arc::clone(&test.app))
        };
        let mcp = echo()
            .route_layer(from_fn_with_state(Arc::clone(&test.app), require_key))
            .with_state(Arc::clone(&test.app));

        let api_key = body_of(api(), &bearer).await;
        let console = body_of(api(), &cookie).await;
        let mcp_key = body_of(mcp, &bearer).await;

        let ip = "Some(192.0.2.5)";
        assert_eq!(api_key, format!("{ip} Api {:?} true", Via::Key));
        assert_eq!(console, format!("{ip} Console {:?} true", Via::Session));
        assert_eq!(mcp_key, format!("{ip} Api {:?} true", Via::Key));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn the_requester_reads_the_ip_and_tells_a_console_session_from_a_key() {
        let test = crate::http::api::testing::app().await;
        let ip: IpAddr = "192.0.2.9".parse().unwrap();
        let (mut keyed, ()) = Request::builder()
            .extension(ClientIp(Some(ip)))
            .body(())
            .unwrap()
            .into_parts();
        let (mut console, ()) = Request::builder()
            .extension(ClientIp(None))
            .extension(ConsoleSession::for_test())
            .body(())
            .unwrap()
            .into_parts();
        let (mut bare, ()) = Request::builder().body(()).unwrap().into_parts();

        let keyed = Requester::from_request_parts(&mut keyed, &test.app)
            .await
            .unwrap();
        let console = Requester::from_request_parts(&mut console, &test.app)
            .await
            .unwrap();
        let actor = AuditActor::from_request_parts(&mut bare, &test.app).await;

        assert_eq!(
            keyed,
            Requester {
                ip: Some(ip),
                source: Source::Api
            }
        );
        assert_eq!(
            console,
            Requester {
                ip: None,
                source: Source::Console
            }
        );
        assert!(
            matches!(actor, Err(AppError(OkfError::Forbidden { .. }))),
            "no principal, no actor"
        );
    }
}

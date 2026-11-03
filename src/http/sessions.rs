//! MCP sessions bound to the key that opened them (M5 Decision 2).
//!
//! rmcp knows a session only by its `Mcp-Session-Id`, and each request is
//! authorized by its own key, so without this a second key that learned a
//! session id could post on the session or attach to its `GET /mcp` stream.
//! [`bind_session`] records the key of the request that opens a session and
//! answers any later request on it with another key, or on a session it
//! never bound, with rmcp's own 404 for an unknown session.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use axum::extract::{Request, State};
use axum::http::{Method, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};

use crate::app::App;
use crate::auth::Principal;
use crate::selection::SESSION_HEADER;

/// The body rmcp answers an unknown session with; a refused key gets the
/// same, so it learns nothing about the session.
const SESSION_NOT_FOUND: &str = "Not Found: Session not found";

/// One session's key, and when it was last used.
#[derive(Debug)]
struct Binding {
    key_id: String,
    seen: Instant,
}

/// Every live session's key, by session id.
#[derive(Debug)]
pub struct SessionKeys {
    idle: Duration,
    // A `std` mutex: held only for a map lookup or insert, never across an
    // `.await`, so it never blocks an async thread for long.
    bindings: Mutex<HashMap<String, Binding>>,
}

impl SessionKeys {
    /// An empty map whose bindings expire after `idle` without use.
    ///
    /// # Arguments
    /// * `idle` - `session_idle_timeout`, the same bound rmcp's sessions have.
    ///
    /// # Returns
    /// The map.
    pub fn new(idle: Duration) -> Self {
        Self {
            idle,
            bindings: Mutex::new(HashMap::new()),
        }
    }

    /// Records that `key_id` opened `session`, first dropping every binding
    /// idle past the timeout (so the map holds live sessions only).
    ///
    /// # Arguments
    /// * `session` - the `Mcp-Session-Id` rmcp issued.
    /// * `key_id` - the key of the request that opened it.
    /// * `now` - the time of that request.
    pub fn bind(&self, session: &str, key_id: &str, now: Instant) {
        let mut bindings = self.lock();
        // `retain` keeps the entries the closure accepts: Python's
        // `{k: v for k, v in d.items() if keep(v)}`, in place.
        bindings.retain(|_, binding| now.saturating_duration_since(binding.seen) <= self.idle);
        bindings.insert(
            session.to_string(),
            Binding {
                key_id: key_id.to_string(),
                seen: now,
            },
        );
    }

    /// May `key_id` use `session`? Using it restarts its idle clock.
    ///
    /// # Arguments
    /// * `session` - the request's `Mcp-Session-Id`.
    /// * `key_id` - the request's key.
    /// * `now` - the time of the request.
    ///
    /// # Returns
    /// `true` only if `key_id` opened the session and it has not idled out;
    /// an idled-out binding is dropped.
    pub fn admits(&self, session: &str, key_id: &str, now: Instant) -> bool {
        let mut bindings = self.lock();
        let Some(binding) = bindings.get_mut(session) else {
            return false;
        };
        if now.saturating_duration_since(binding.seen) > self.idle {
            bindings.remove(session);
            return false;
        }
        if binding.key_id != key_id {
            return false;
        }
        binding.seen = now;
        true
    }

    /// Forgets a session (after its `DELETE /mcp`).
    ///
    /// # Arguments
    /// * `session` - its id.
    pub fn release(&self, session: &str) {
        self.lock().remove(session);
    }

    /// How many sessions are bound.
    #[cfg(test)]
    fn len(&self) -> usize {
        self.lock().len()
    }

    fn lock(&self) -> MutexGuard<'_, HashMap<String, Binding>> {
        // Poisoned only if a thread panicked while holding it; nothing here
        // can panic with it held.
        self.bindings.lock().expect("session binding lock poisoned")
    }
}

/// The `/mcp` middleware of M5 Decision 2. It runs after `require_key`, so
/// the request carries its [`Principal`].
///
/// - A request naming a session is refused with 404 unless that session is
///   bound to this request's key.
/// - A response that issues a session (`Mcp-Session-Id` on the answer to a
///   request that named none) binds it to the request's key.
/// - A successful `DELETE` releases the binding.
///
/// # Arguments
/// * `app` - the shared state ([`App::sessions`]).
/// * `request` - the request.
/// * `next` - the rest of the stack (rmcp's service).
///
/// # Returns
/// rmcp's response, or 404.
pub async fn bind_session(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    let key_id = request
        .extensions()
        .get::<Arc<Principal>>()
        .map(|principal| principal.key_id.clone());
    // Never logged: a session id lets its holder use the session.
    let session = request
        .headers()
        .get(SESSION_HEADER)
        .map(|value| String::from_utf8_lossy(value.as_bytes()).into_owned());
    let now = Instant::now();
    if let Some(session) = &session {
        let admitted = key_id
            .as_deref()
            .is_some_and(|key| app.sessions().admits(session, key, now));
        if !admitted {
            tracing::debug!("refused a request on another key's or an unbound MCP session");
            return (StatusCode::NOT_FOUND, SESSION_NOT_FOUND).into_response();
        }
    }
    let closing = request.method() == Method::DELETE;
    let response = next.run(request).await;
    match (&session, &key_id) {
        (None, Some(key)) => {
            if let Some(issued) = response.headers().get(SESSION_HEADER) {
                app.sessions()
                    .bind(&String::from_utf8_lossy(issued.as_bytes()), key, now);
            }
        }
        (Some(session), _) if closing && response.status().is_success() => {
            app.sessions().release(session);
        }
        // A request on a bound session (already refreshed by `admits`), or
        // one without a key (only a router without `require_key`).
        _ => {}
    }
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    const IDLE: Duration = Duration::from_secs(60);

    #[test]
    fn a_session_admits_only_the_key_that_opened_it() {
        let keys = SessionKeys::new(IDLE);
        let start = Instant::now();

        keys.bind("s1", "alice", start);

        assert!(keys.admits("s1", "alice", start + Duration::from_secs(1)));
        assert!(!keys.admits("s1", "bob", start + Duration::from_secs(1)));
        assert!(!keys.admits("unknown", "alice", start));
    }

    #[test]
    fn use_keeps_a_binding_and_idling_drops_it() {
        let keys = SessionKeys::new(IDLE);
        let start = Instant::now();
        keys.bind("busy", "alice", start);
        keys.bind("idle", "alice", start);

        // `busy` is used just inside the timeout, which restarts its clock.
        let used = keys.admits("busy", "alice", start + IDLE);
        let later = start + IDLE * 2;
        let busy_later = keys.admits("busy", "alice", later);
        let idle_later = keys.admits("idle", "alice", later);

        assert!(used);
        assert!(busy_later);
        assert!(!idle_later, "an idle binding admits no one");
    }

    #[test]
    fn binding_a_new_session_prunes_idle_ones_and_release_drops_one() {
        let keys = SessionKeys::new(IDLE);
        let start = Instant::now();
        keys.bind("old", "alice", start);
        keys.bind("closed", "alice", start + IDLE * 2);

        keys.bind("new", "bob", start + IDLE * 2);
        keys.release("closed");

        assert_eq!(keys.len(), 1);
        assert!(keys.admits("new", "bob", start + IDLE * 2));
    }
}

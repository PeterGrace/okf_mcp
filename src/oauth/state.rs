//! What the authorization server keeps in memory between requests (spec
//! "In memory"): pending authorizations (a consent page in progress) and
//! authorization codes (minted on Approve, exchanged once at `/oauth/token`).

use std::time::Duration;

use crate::store::ceiling::Ceiling;
use crate::store::oauth::clients::ClientRecord;

/// How long a consent page may stay open.
pub const PENDING_TTL: Duration = Duration::from_secs(10 * 60);

/// How long an authorization code may wait to be exchanged.
pub const AUTHORIZATION_CODE_TTL: Duration = Duration::from_secs(60);

/// Wrong sign-in codes a pending authorization survives.
pub const MAX_FAILED_ATTEMPTS: u8 = 5;

/// The most pending authorizations held at once (they are made by an
/// unauthenticated GET, so the map is bounded).
pub const MAX_PENDING: usize = 10_000;

/// The most unexchanged authorization codes held at once.
pub const MAX_AUTHORIZATION_CODES: usize = 10_000;

/// A validated `GET /oauth/authorize`, waiting for the person's decision.
/// Keyed in `OAuth::pending` by a random id the consent form carries.
#[derive(Clone)]
pub struct PendingAuthorization {
    /// The CSRF token every consent form must echo.
    pub csrf: String,
    /// The client, resolved (CIMD or DCR).
    pub client: ClientRecord,
    /// The redirect URI, already matched exactly against the client's.
    pub redirect_uri: String,
    /// The client's `state`, returned unchanged.
    pub state: Option<String>,
    /// The PKCE S256 challenge.
    pub code_challenge: String,
    /// The resource (always this server's `/mcp`).
    pub resource: String,
    /// The sign-in code accepted at step 1 and the step-2 page showing it,
    /// awaiting Approve.
    pub accepted: Option<AcceptedCode>,
    /// Wrong codes entered so far.
    pub failed_attempts: u8,
    /// Set when a signed-in person's consent page was shown (the user
    /// path); `None` on the sign-in code path.
    pub consent: Option<UserConsent>,
    /// The request's validated parameters as a query string (no `?`, no
    /// `signin`), for the sign-in code link when the page is shown again.
    pub query: String,
}

// Written by hand, not derived, so a stray `{:?}` never prints the CSRF token.
impl std::fmt::Debug for PendingAuthorization {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PendingAuthorization")
            .field("csrf", &"<redacted>")
            .field("client_id", &self.client.client_id)
            .field("redirect_uri", &self.redirect_uri)
            .field("accepted", &self.accepted)
            .field("failed_attempts", &self.failed_attempts)
            .field("consent", &self.consent)
            .finish_non_exhaustive()
    }
}

/// A sign-in code accepted at step 1, bound to the one step-2 summary that
/// shows it. Entering another code (in another copy of the step-1 form)
/// replaces both, so an older summary's Approve no longer matches and
/// cannot approve a code that page did not show.
#[derive(Clone, PartialEq, Eq)]
pub struct AcceptedCode {
    /// The code's id (never the code).
    pub code_id: String,
    /// A random token in that summary's Approve form, compared in constant
    /// time on Approve.
    pub summary_nonce: String,
}

// Written by hand, not derived, so a stray `{:?}` never prints the nonce.
impl std::fmt::Debug for AcceptedCode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("AcceptedCode")
            .field("code_id", &self.code_id)
            .field("summary_nonce", &"<redacted>")
            .finish()
    }
}

/// A signed-in person's consent page: who it was shown to, what it
/// offered, and the random token its Approve must echo. Each Approve spends
/// the token (it is replaced under the pending map's lock), so one page
/// approves at most once.
#[derive(Clone, PartialEq, Eq)]
pub struct UserConsent {
    /// The `users.id` the page was rendered for; the POST's session must
    /// still be this user.
    pub user_id: String,
    /// A random token in the page's form (its hidden `summary` input),
    /// compared in constant time.
    pub nonce: String,
    /// The ceiling the page offered. The POST may choose only from it, and
    /// the store checks the choice against the ceiling again at insert.
    pub offered: Ceiling,
}

// Written by hand, not derived, so a stray `{:?}` never prints the nonce.
impl std::fmt::Debug for UserConsent {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("UserConsent")
            .field("user_id", &self.user_id)
            .field("nonce", &"<redacted>")
            .field("offered", &self.offered)
            .finish()
    }
}

/// An authorization code's binding, keyed in `OAuth::codes` by the hex
/// SHA-256 of the `okfac_` code (the code itself is never stored).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizationCode {
    /// The grant the approval created.
    pub grant_id: String,
    /// The client it was issued to.
    pub client_id: String,
    /// The redirect URI it was issued with.
    pub redirect_uri: String,
    /// The PKCE S256 challenge the verifier must match.
    pub code_challenge: String,
    /// The resource the tokens will be issued for.
    pub resource: String,
    /// The token family its tokens join.
    pub family_id: String,
    /// Whether it was exchanged. A used code stays in the map until it
    /// expires, so presenting it again is seen as a replay (RFC 6749
    /// section 4.1.2: Task 6 then revokes its family).
    pub used: bool,
    /// Whether it was presented again after it was used. The exchange that
    /// used it checks this after its tokens commit, so a replay racing that
    /// exchange still revokes the tokens it mints (Task 6).
    pub replayed: bool,
}

/// The key of `OAuth::codes` for an authorization code: the hex SHA-256 of
/// the `okfac_` code, so the map never holds a usable code.
///
/// # Arguments
/// * `code` - the authorization code, as minted or presented.
///
/// # Returns
/// 64 lowercase hex characters.
pub fn code_key(code: &str) -> String {
    // `fold` with `write!` appends each byte's two hex digits to one
    // preallocated `String` (Python: `hashlib.sha256(...).hexdigest()`),
    // with no `String` per byte.
    use std::fmt::Write as _;
    crate::keys::hash(code)
        .iter()
        .fold(String::with_capacity(64), |mut hex, byte| {
            // Writing to a `String` cannot fail.
            let _ = write!(hex, "{byte:02x}");
            hex
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::oauth::clients::ClientKind;

    #[test]
    fn code_key_is_the_hex_sha256() {
        // The FIPS 180-2 test vector for "abc" (as in `keys::tests`).
        assert_eq!(
            code_key("abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn debug_never_prints_the_csrf_token() {
        let pending = PendingAuthorization {
            csrf: "csrf-value-that-must-not-leak".into(),
            client: ClientRecord {
                client_id: "https://client.example.com/client.json".into(),
                kind: ClientKind::Cimd,
                client_name: None,
                redirect_uris: vec!["https://client.example.com/callback".into()],
                metadata: serde_json::json!({}),
                fetched_at: None,
                cache_expires_at: None,
                created_at: chrono::Utc::now(),
            },
            redirect_uri: "https://client.example.com/callback".into(),
            state: None,
            code_challenge: "c".repeat(43),
            resource: "https://okf.example.com/mcp".into(),
            accepted: Some(AcceptedCode {
                code_id: "code-id".into(),
                summary_nonce: "nonce-value-that-must-not-leak".into(),
            }),
            failed_attempts: 0,
            consent: Some(UserConsent {
                user_id: "user-id".into(),
                nonce: "consent-nonce-that-must-not-leak".into(),
                offered: Ceiling::new(),
            }),
            query: "state=state-that-stays-out".into(),
        };

        let printed = format!("{pending:?}");

        assert!(!printed.contains("csrf-value-that-must-not-leak"));
        assert!(!printed.contains("nonce-value-that-must-not-leak"));
        assert!(!printed.contains("consent-nonce-that-must-not-leak"));
        assert!(!printed.contains("state-that-stays-out"));
        assert!(printed.contains("user-id"));
        assert!(printed.contains("code-id"));
        assert!(printed.contains("<redacted>"));
    }
}

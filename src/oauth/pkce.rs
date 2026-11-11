//! PKCE (RFC 7636), S256 only: the authorize endpoint checks a challenge's
//! shape, the token endpoint checks a verifier against it.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine as _;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// The length of an S256 challenge: 32 bytes of SHA-256 in unpadded base64url.
const CHALLENGE_LEN: usize = 43;

/// The shortest and longest verifiers RFC 7636 section 4.1 allows.
const VERIFIER_LEN: std::ops::RangeInclusive<usize> = 43..=128;

/// Is `byte` a base64url character (`A-Z a-z 0-9 - _`)?
fn is_base64url(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')
}

/// Is `byte` an RFC 7636 "unreserved" character (`A-Z a-z 0-9 - . _ ~`)?
fn is_unreserved(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~')
}

/// Could `challenge` be an S256 code challenge?
///
/// # Arguments
/// * `challenge` - the `code_challenge` parameter.
///
/// # Returns
/// `true` for exactly 43 base64url characters.
pub fn is_valid_challenge(challenge: &str) -> bool {
    challenge.len() == CHALLENGE_LEN && challenge.bytes().all(is_base64url)
}

/// Does `verifier` match `challenge` under S256?
///
/// # Arguments
/// * `challenge` - the `code_challenge` stored with the authorization code.
/// * `verifier` - the `code_verifier` the token request carries.
///
/// # Returns
/// `true` only if `verifier` is 43 to 128 unreserved characters and
/// `BASE64URL(SHA256(verifier))` equals `challenge`, compared in constant time.
pub fn verify(challenge: &str, verifier: &str) -> bool {
    let well_formed = VERIFIER_LEN.contains(&verifier.len()) && verifier.bytes().all(is_unreserved);
    if !well_formed {
        return false;
    }
    let computed = URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()));
    // `ct_eq` on two byte slices of different lengths is simply false; its
    // `Choice` result converts to `bool` with `into()`.
    computed.as_bytes().ct_eq(challenge.as_bytes()).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 7636 appendix B: a published test vector, not a secret.
    const RFC_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    const RFC_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

    /// The S256 challenge of a verifier, computed independently of `verify`.
    fn s256(verifier: &str) -> String {
        URL_SAFE_NO_PAD.encode(Sha256::digest(verifier.as_bytes()))
    }

    #[test]
    fn the_rfc_vector_verifies() {
        assert!(is_valid_challenge(RFC_CHALLENGE));
        assert!(verify(RFC_CHALLENGE, RFC_VERIFIER));
    }

    #[test]
    fn a_different_verifier_fails() {
        let other = format!("{}A", &RFC_VERIFIER[..42]);
        assert!(!verify(RFC_CHALLENGE, &other));
    }

    #[test]
    fn malformed_verifiers_fail_without_hashing() {
        assert!(!verify(RFC_CHALLENGE, &RFC_VERIFIER[..42]), "too short");
        assert!(!verify(RFC_CHALLENGE, &"a".repeat(129)), "too long");
        assert!(
            !verify(RFC_CHALLENGE, &format!("{}+", &RFC_VERIFIER[..42])),
            "'+' is reserved"
        );
    }

    #[test]
    fn challenges_must_be_43_base64url_characters() {
        assert!(!is_valid_challenge(""));
        assert!(!is_valid_challenge(&RFC_CHALLENGE[..42]));
        assert!(!is_valid_challenge(&format!("{RFC_CHALLENGE}A")));
        assert!(!is_valid_challenge(&format!("{}=", &RFC_CHALLENGE[..42])));
        // A plain-method "challenge" is often the verifier itself: 43 chars
        // of the unreserved set, here with a '.', which base64url lacks.
        assert!(!is_valid_challenge(&format!("{}.", &RFC_CHALLENGE[..42])));
    }

    #[hegel::test]
    fn verify_accepts_exactly_the_matching_verifier(tc: hegel::TestCase) {
        use hegel::generators;
        let verifier: String = tc.draw(generators::from_regex("[A-Za-z0-9._~-]{43,128}"));
        let other: String = tc.draw(generators::from_regex("[A-Za-z0-9._~-]{43,128}"));
        let challenge = s256(&verifier);

        assert!(is_valid_challenge(&challenge));
        assert!(verify(&challenge, &verifier));
        // SHA-256 collisions are out of reach, so a different verifier fails.
        assert_eq!(verify(&challenge, &other), verifier == other);
    }

    #[hegel::test]
    fn verify_never_panics_on_arbitrary_input(tc: hegel::TestCase) {
        use hegel::generators;
        let challenge: String = tc.draw(generators::text());
        let verifier: String = tc.draw(generators::text());

        // Any answer is fine; a malformed verifier must be `false`.
        let accepted = verify(&challenge, &verifier);
        if !VERIFIER_LEN.contains(&verifier.len()) {
            assert!(!accepted);
        }
    }
}

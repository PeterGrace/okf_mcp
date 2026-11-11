//! Secrets: API keys, OAuth access and refresh tokens, sign-in codes and
//! authorization codes. Each is a recognizable prefix plus 32 random bytes.
//! A secret is shown once, when created; only its SHA-256 is stored, and
//! secrets are never logged.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
// `Engine as _` imports the trait for its `encode` method without binding a
// name: in Rust a trait's methods are callable only while the trait is in scope.
use base64::Engine as _;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::error::OkfError;

/// Every API key starts with this, so people and secret scanners can recognize one.
pub const KEY_PREFIX: &str = "okf_";

/// Every OAuth access token starts with this (the bearer dispatch in `auth`).
pub const ACCESS_TOKEN_PREFIX: &str = "okfat_";

/// Every OAuth refresh token starts with this.
pub const REFRESH_TOKEN_PREFIX: &str = "okfrt_";

/// Every admin-issued sign-in code starts with this.
pub const SIGN_IN_CODE_PREFIX: &str = "okfsc_";

/// Every OAuth authorization code starts with this.
pub const AUTHORIZATION_CODE_PREFIX: &str = "okfac_";

/// Generates a new API key: [`generate_with`] with [`KEY_PREFIX`].
///
/// # Returns
/// The key. Show it once; store only [`hash`] of it.
///
/// # Errors
/// [`OkfError::Io`] if the OS random source fails.
pub fn generate() -> Result<String, OkfError> {
    generate_with(KEY_PREFIX)
}

/// Generates a new secret: `prefix` plus 32 random bytes in unpadded
/// base64url (43 characters).
///
/// # Arguments
/// * `prefix` - one of this module's `*_PREFIX` constants.
///
/// # Returns
/// The secret. Show it once; store only [`hash`] of it.
///
/// # Errors
/// [`OkfError::Io`] if the OS random source fails.
pub fn generate_with(prefix: &str) -> Result<String, OkfError> {
    // A fixed-size array on the stack, filled in place (`&mut` lends it out
    // for writing), like `bytearray(32)` passed to `os.urandom`'s C side.
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| OkfError::Io {
        path: "<os random source>".to_string(),
        source: std::io::Error::other(error),
    })?;
    Ok(format!("{prefix}{}", URL_SAFE_NO_PAD.encode(bytes)))
}

/// The SHA-256 of a key's text, as stored in `api_keys.sha256`.
///
/// # Arguments
/// * `key` - the key as presented.
///
/// # Returns
/// The 32-byte digest.
pub fn hash(key: &str) -> [u8; 32] {
    // `digest` returns a fixed-size array type of its own; `.into()` turns it
    // into a plain `[u8; 32]`.
    Sha256::digest(key.as_bytes()).into()
}

/// Does a presented key match a stored hash? Compared in constant time, so
/// the comparison's duration says nothing about how much of it matched.
///
/// # Arguments
/// * `stored` - the hash from `api_keys.sha256`.
/// * `presented` - the key a client sent.
///
/// # Returns
/// `true` only if `stored` is exactly the SHA-256 of `presented`; a stored
/// value of the wrong length is `false`, not a panic.
pub fn matches(stored: &[u8], presented: &str) -> bool {
    // `ct_eq` returns a `Choice` (a constant-time boolean); `.into()` makes
    // it a `bool` once the comparison is done.
    stored.ct_eq(&hash(presented)).into()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keys_are_prefixed_url_safe_and_unique() {
        let key = generate().unwrap();

        assert!(key.starts_with(KEY_PREFIX));
        assert_eq!(key.len(), KEY_PREFIX.len() + 43);
        assert!(key[KEY_PREFIX.len()..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
        assert_ne!(key, generate().unwrap());
    }

    #[test]
    fn hash_is_sha256() {
        // The FIPS 180-2 test vector for "abc".
        let expected: [u8; 32] = [
            0xba, 0x78, 0x16, 0xbf, 0x8f, 0x01, 0xcf, 0xea, 0x41, 0x41, 0x40, 0xde, 0x5d, 0xae,
            0x22, 0x23, 0xb0, 0x03, 0x61, 0xa3, 0x96, 0x17, 0x7a, 0x9c, 0xb4, 0x10, 0xff, 0x61,
            0xf2, 0x00, 0x15, 0xad,
        ];

        assert_eq!(hash("abc"), expected);
    }

    #[test]
    fn matches_compares_hashes() {
        let key = generate().unwrap();
        let stored = hash(&key);

        assert!(matches(&stored, &key));
        assert!(!matches(&stored, "okf_wrong"));
        assert!(
            !matches(&stored[..31], &key),
            "a short hash is false, not a panic"
        );
    }

    #[test]
    fn every_secret_kind_has_its_own_prefix() {
        for prefix in PREFIXES {
            let secret = generate_with(prefix).unwrap();

            assert!(secret.starts_with(prefix), "{prefix}");
            assert_eq!(secret.len(), prefix.len() + 43, "{prefix}");
        }
        // No prefix is a prefix of another, so dispatch by prefix is exact.
        for (index, first) in PREFIXES.iter().enumerate() {
            for second in &PREFIXES[index + 1..] {
                assert!(!first.starts_with(second), "{first} {second}");
                assert!(!second.starts_with(first), "{first} {second}");
            }
        }
    }

    /// Every kind of secret, by prefix. `KEY_PREFIX` (`okf_`) differs from the
    /// others at its fourth byte, so no prefix is a prefix of another.
    const PREFIXES: [&str; 5] = [
        KEY_PREFIX,
        ACCESS_TOKEN_PREFIX,
        REFRESH_TOKEN_PREFIX,
        SIGN_IN_CODE_PREFIX,
        AUTHORIZATION_CODE_PREFIX,
    ];

    #[hegel::test]
    fn a_prefixed_secret_is_exactly_one_kind(tc: hegel::TestCase) {
        use hegel::generators;
        let prefix: &str = tc.draw(generators::sampled_from(PREFIXES.to_vec()));
        let rest: String = tc.draw(generators::text());
        let token = format!("{prefix}{rest}");

        // The bearer dispatch (`auth::authenticate`) routes on
        // `starts_with`, so a token must never look like two kinds.
        let kinds = PREFIXES
            .iter()
            .filter(|known| token.starts_with(**known))
            .count();
        assert_eq!(kinds, 1, "{token:?}");
    }

    #[hegel::test]
    fn generated_secrets_keep_their_prefix_and_alphabet(tc: hegel::TestCase) {
        use hegel::generators;
        let prefix: &str = tc.draw(generators::sampled_from(PREFIXES.to_vec()));

        let secret = generate_with(prefix).expect("random source");

        assert!(secret.starts_with(prefix));
        assert_eq!(secret.len(), prefix.len() + 43);
        assert!(secret[prefix.len()..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_'));
    }
}

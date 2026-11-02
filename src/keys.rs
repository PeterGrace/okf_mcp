//! API keys: generation, hashing and constant-time comparison. A key is
//! shown once, when created; only its SHA-256 is stored, and keys are never
//! logged.

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
// `Engine as _` imports the trait for its `encode` method without binding a
// name: in Rust a trait's methods are callable only while the trait is in scope.
use base64::Engine as _;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::error::OkfError;

/// Every key starts with this, so people and secret scanners can recognize one.
pub const KEY_PREFIX: &str = "okf_";

/// Generates a new key: [`KEY_PREFIX`] plus 32 random bytes in unpadded
/// base64url (43 characters).
///
/// # Returns
/// The key. Show it once; store only [`hash`] of it.
///
/// # Errors
/// [`OkfError::Io`] if the OS random source fails.
pub fn generate() -> Result<String, OkfError> {
    // A fixed-size array on the stack, filled in place (`&mut` lends it out
    // for writing), like `bytearray(32)` passed to `os.urandom`'s C side.
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(|error| OkfError::Io {
        path: "<os random source>".to_string(),
        source: std::io::Error::other(error),
    })?;
    Ok(format!("{KEY_PREFIX}{}", URL_SAFE_NO_PAD.encode(bytes)))
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
}

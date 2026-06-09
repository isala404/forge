//! Small dependency-light helpers shared across the crate.

use sha2::{Digest, Sha256};

/// SHA-256 of `bytes` as a lowercase hex string. Stable across binaries/deploys (unlike `DefaultHasher`), so safe for migration checksums.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(Sha256::digest(bytes).as_slice())
}

/// Lowercase hex encoding of `bytes`. Used for random token rendering and HMAC tags.
pub fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    let mut out = String::with_capacity(bytes.len() * 2);
    for b in bytes {
        let _ = write!(out, "{b:02x}");
    }
    out
}

/// Short non-reversible fingerprint of a key for spans, so raw keys (user ids, emails, tokens) never land in observability.
pub fn key_hash(key: &str) -> String {
    let full = sha256_hex(key.as_bytes());
    full.get(..16).unwrap_or(&full).to_string()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_matches_known_vector() {
        // SHA-256("") well-known digest.
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn sha256_hex_is_stable_and_64_chars() {
        assert_eq!(sha256_hex(b"forge").len(), 64);
        assert_eq!(sha256_hex(b"forge"), sha256_hex(b"forge"));
    }

    #[test]
    fn key_hash_is_16_chars_and_hides_the_key() {
        let h = key_hash("user:42:session");
        assert_eq!(h.len(), 16);
        assert!(!h.contains("user"));
    }
}

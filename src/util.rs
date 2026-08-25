use sha2::{Digest, Sha256};

pub const MAX_NAMESPACE_BYTES: usize = 128;

pub fn validate_namespace(namespace: &str) -> crate::Result<()> {
    if namespace.len() > MAX_NAMESPACE_BYTES {
        return Err(crate::ForgeError::config(
            "namespace must contain at most 128 bytes",
        ));
    }
    if !namespace
        .bytes()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err(crate::ForgeError::config(
            "namespace may contain only ASCII letters, digits, '-', '_', and '.'",
        ));
    }
    Ok(())
}

/// SHA-256 of `bytes` as a lowercase hex string. Stable across binaries/deploys (unlike `DefaultHasher`), so safe for migration checksums.
pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(Sha256::digest(bytes).as_slice())
}

/// Lowercase hex encoding of `bytes`.
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

/// Prefix a logical key with an app namespace as `<ns>:<key>`; an empty namespace leaves
/// the key untouched. Namespaces are colon-free, so `<ns>:<key>` never collides across
/// distinct `(ns, key)`.
pub fn namespaced(ns: &str, key: &str) -> String {
    if ns.is_empty() {
        key.to_string()
    } else {
        format!("{ns}:{key}")
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hex_matches_known_vector() {
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

    #[test]
    fn namespaced_prefixes_only_when_set() {
        assert_eq!(namespaced("", "k"), "k");
        assert_eq!(namespaced("app", "k"), "app:k");
        assert_eq!(namespaced("app", "a:b"), "app:a:b");
    }

    #[test]
    fn namespace_validation_is_exact_and_bounded() {
        assert!(validate_namespace("").is_ok());
        assert!(validate_namespace("App_1.prod").is_ok());
        assert!(validate_namespace("space here").is_err());
        assert!(validate_namespace(&"a".repeat(129)).is_err());
    }
}

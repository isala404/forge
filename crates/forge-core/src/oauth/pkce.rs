//! PKCE (Proof Key for Code Exchange) verification.
//!
//! Implements S256 challenge method per RFC 7636 and OAuth 2.1.
//! Plain method is intentionally unsupported (OAuth 2.1 mandates S256).

use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Verify a PKCE code verifier against a stored code challenge (S256).
///
/// Computes `BASE64URL(SHA256(code_verifier))` and compares to `code_challenge`.
pub fn verify_s256(code_verifier: &str, code_challenge: &str) -> bool {
    let mut hasher = Sha256::new();
    hasher.update(code_verifier.as_bytes());
    let computed = URL_SAFE_NO_PAD.encode(hasher.finalize());
    computed
        .as_bytes()
        .ct_eq(code_challenge.as_bytes())
        .into()
}

/// Compute S256 challenge from a verifier.
pub fn compute_s256_challenge(code_verifier: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(code_verifier.as_bytes());
    URL_SAFE_NO_PAD.encode(hasher.finalize())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn test_s256_roundtrip() {
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let challenge = compute_s256_challenge(verifier);
        assert!(verify_s256(verifier, &challenge));
    }

    #[test]
    fn test_s256_rfc7636_example() {
        // RFC 7636 Appendix B test vector
        let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
        let expected_challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
        assert!(verify_s256(verifier, expected_challenge));
    }

    #[test]
    fn test_s256_wrong_verifier() {
        let challenge = compute_s256_challenge("correct-verifier");
        assert!(!verify_s256("wrong-verifier", &challenge));
    }

    #[test]
    fn test_s256_empty_strings() {
        let challenge = compute_s256_challenge("");
        assert!(verify_s256("", &challenge));
        assert!(!verify_s256("notempty", &challenge));
    }
}

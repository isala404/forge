use crate::error::{ForgeError, Result};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;

type HmacSha256 = Hmac<Sha256>;

/// The HTTP method a presigned URL authorizes. Signed in, so a download URL cannot
/// be replayed as an upload.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Method {
    Get,
    Put,
}

impl Method {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Method::Get => "GET",
            Method::Put => "PUT",
        }
    }
}

/// The exact bytes covered by the signature. Order and separators are fixed so the
/// signer and verifier agree.
fn canonical(method: Method, key: &str, expires_epoch: i64, max_bytes: u64) -> String {
    format!("{}\n{key}\n{expires_epoch}\n{max_bytes}", method.as_str())
}

/// HMAC-SHA256 hex signature over the canonical string. `max_bytes` is `0` for a
/// download (it is part of the signature either way).
pub(crate) fn sign(
    secret: &[u8],
    method: Method,
    key: &str,
    expires_epoch: i64,
    max_bytes: u64,
) -> Result<String> {
    // HMAC accepts any key length, so this never actually errs.
    let mut mac = HmacSha256::new_from_slice(secret)
        .map_err(|_| ForgeError::backend("hmac key initialization failed"))?;
    mac.update(canonical(method, key, expires_epoch, max_bytes).as_bytes());
    Ok(crate::util::hex(&mac.finalize().into_bytes()))
}

/// Constant-time verification of a hex signature against the canonical string.
/// A malformed (non-hex) signature, or any mismatch, returns `false`, never panics.
/// Consumed by `Blob::verify_presigned`.
pub(crate) fn verify(
    secret: &[u8],
    method: Method,
    key: &str,
    expires_epoch: i64,
    max_bytes: u64,
    provided_hex: &str,
) -> bool {
    let Some(provided) = from_hex(provided_hex) else {
        return false;
    };
    let Ok(mut mac) = HmacSha256::new_from_slice(secret) else {
        return false;
    };
    mac.update(canonical(method, key, expires_epoch, max_bytes).as_bytes());
    mac.verify_slice(&provided).is_ok()
}

fn from_hex(s: &str) -> Option<Vec<u8>> {
    if !s.len().is_multiple_of(2) {
        return None;
    }
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(s.len() / 2);
    let mut i = 0;
    while i < bytes.len() {
        let hi = (*bytes.get(i)? as char).to_digit(16)?;
        let lo = (*bytes.get(i + 1)? as char).to_digit(16)?;
        out.push((hi * 16 + lo) as u8);
        i += 2;
    }
    Some(out)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn sign_then_verify_roundtrips() {
        let secret = b"super-secret";
        let sig = sign(secret, Method::Get, "exports/a.csv", 1_000, 0).unwrap();
        assert!(verify(secret, Method::Get, "exports/a.csv", 1_000, 0, &sig));
    }

    #[test]
    fn verification_rejects_tampering() {
        let secret = b"super-secret";
        let sig = sign(secret, Method::Put, "k", 1_000, 1024).unwrap();
        assert!(!verify(secret, Method::Get, "k", 1_000, 1024, &sig));
        assert!(!verify(secret, Method::Put, "other", 1_000, 1024, &sig));
        assert!(!verify(secret, Method::Put, "k", 2_000, 1024, &sig));
        assert!(!verify(secret, Method::Put, "k", 1_000, 2048, &sig));
        assert!(!verify(
            b"wrong-secret",
            Method::Put,
            "k",
            1_000,
            1024,
            &sig
        ));
        assert!(!verify(secret, Method::Put, "k", 1_000, 1024, "not-hex!!"));
    }

    #[test]
    fn hex_decode_roundtrips() {
        assert_eq!(crate::util::hex(&[0x00, 0x0f, 0xff]), "000fff");
        assert_eq!(from_hex("000fff").unwrap(), vec![0x00, 0x0f, 0xff]);
        assert!(from_hex("xyz").is_none());
        assert!(from_hex("abc").is_none()); // odd length
    }
}

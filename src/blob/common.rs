use super::sign::{self, Method};
use super::{
    MAX_CONTENT_TYPE_BYTES, MAX_HTTP_METADATA_BYTES, MAX_KEY_BYTES, MAX_METADATA_BYTES,
    MAX_OBJECT_BYTES, MAX_PRESIGN_EXPIRES, ProxyPresign, PutOpts,
};
use crate::error::{ForgeError, Result};
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// Map a logical key to its stored form by prefixing the namespace (`ns:key`).
pub(super) fn physical(namespace: &str, key: &str) -> String {
    if namespace.is_empty() {
        key.to_string()
    } else {
        format!("{namespace}:{key}")
    }
}

/// Strip the namespace prefix from a stored key, recovering the logical key.
pub(super) fn logical<'a>(namespace: &str, stored: &'a str) -> &'a str {
    if namespace.is_empty() {
        stored
    } else {
        stored
            .strip_prefix(namespace)
            .and_then(|s| s.strip_prefix(':'))
            .unwrap_or(stored)
    }
}

/// Validate a blob key: non-empty and within the byte cap.
pub(crate) fn check_key(key: &str) -> Result<()> {
    if key.is_empty() {
        return Err(ForgeError::invalid("blob key must not be empty"));
    }
    if key.len() > MAX_KEY_BYTES {
        return Err(ForgeError::limit(format!(
            "key is {} bytes; max is {MAX_KEY_BYTES}",
            key.len()
        )));
    }
    Ok(())
}

/// Validate a `put`'s body size, content type, and metadata size.
pub(super) fn check_put(data: &[u8], opts: &PutOpts) -> Result<()> {
    if data.len() > MAX_OBJECT_BYTES {
        return Err(ForgeError::limit(format!(
            "object is {} bytes; max is {MAX_OBJECT_BYTES}",
            data.len()
        )));
    }
    if let Some(ct) = &opts.content_type
        && ct.len() > MAX_CONTENT_TYPE_BYTES
    {
        return Err(ForgeError::limit(format!(
            "content_type is {} bytes; max is {MAX_CONTENT_TYPE_BYTES}",
            ct.len()
        )));
    }
    let meta_bytes: usize = opts.metadata.iter().map(|(k, v)| k.len() + v.len()).sum();
    if meta_bytes > MAX_METADATA_BYTES {
        return Err(ForgeError::limit(format!(
            "metadata is {meta_bytes} bytes; max is {MAX_METADATA_BYTES}"
        )));
    }
    for (name, value) in [
        ("cache_control", opts.cache_control.as_deref()),
        ("content_disposition", opts.content_disposition.as_deref()),
    ] {
        if value.is_some_and(|value| value.len() > MAX_HTTP_METADATA_BYTES) {
            return Err(ForgeError::limit(format!(
                "{name} exceeds {MAX_HTTP_METADATA_BYTES} bytes"
            )));
        }
    }
    if let Some(expected) = &opts.checksum_sha256 {
        check_sha256(expected)?;
        if !data.is_empty() && crate::util::sha256_hex(data) != *expected {
            return Err(ForgeError::precondition(
                "blob SHA-256 checksum does not match",
            ));
        }
    }
    Ok(())
}

pub(super) fn check_sha256(value: &str) -> Result<()> {
    if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(ForgeError::invalid(
            "SHA-256 checksum must be 64 hexadecimal characters",
        ));
    }
    if value.bytes().any(|byte| byte.is_ascii_uppercase()) {
        return Err(ForgeError::invalid(
            "SHA-256 checksum must use lowercase hexadecimal",
        ));
    }
    Ok(())
}

pub(super) fn sha256_base64(value: &str) -> Result<String> {
    check_sha256(value)?;
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(32);
    for pair in bytes.chunks_exact(2) {
        let [high, low] = pair else {
            return Err(ForgeError::invalid("invalid SHA-256 checksum"));
        };
        let high = (*high as char)
            .to_digit(16)
            .ok_or_else(|| ForgeError::invalid("invalid SHA-256 checksum"))?;
        let low = (*low as char)
            .to_digit(16)
            .ok_or_else(|| ForgeError::invalid("invalid SHA-256 checksum"))?;
        decoded.push((high * 16 + low) as u8);
    }
    Ok(aws_smithy_types::base64::encode(decoded))
}

pub(super) fn check_get_conditions(
    if_match: Option<&str>,
    if_none_match: Option<&str>,
) -> Result<()> {
    if if_match.is_some() && if_none_match.is_some() {
        return Err(ForgeError::invalid(
            "if_match and if_none_match are mutually exclusive",
        ));
    }
    if if_match.is_some_and(str::is_empty) || if_none_match.is_some_and(str::is_empty) {
        return Err(ForgeError::invalid("blob ETag condition must not be empty"));
    }
    Ok(())
}

pub(super) fn reject_s3_encryption(opts: &PutOpts) -> Result<()> {
    if opts.s3_encryption.is_some() {
        return Err(ForgeError::not_configured(
            "S3 server-side encryption requires the S3 blob backend",
        ));
    }
    Ok(())
}

/// Whole seconds since the Unix epoch (saturating; times here are always future).
pub(super) fn unix_secs(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Escape `LIKE` wildcards so a caller prefix matches literally.
pub(super) fn like_escape(prefix: &str) -> String {
    let mut out = String::with_capacity(prefix.len() + 2);
    for c in prefix.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A missing signing secret is `Config`: presigning is unconfigured, a deployment
/// problem, classified the same way across presign/verify/router.
fn require_secret(secret: Option<&[u8]>) -> Result<&[u8]> {
    secret
        .filter(|value| !value.is_empty() && value.iter().any(|byte| !byte.is_ascii_whitespace()))
        .ok_or_else(|| {
            ForgeError::config(
                "blob signing secret is not configured or is empty (set a non-empty blob.signing_secret in forge.toml)",
            )
        })
}

/// Build a signed URL (pool-free, so it is unit-testable without a database).
pub(super) fn presign_url(
    secret: Option<&[u8]>,
    base_url: &str,
    namespace: &str,
    method: Method,
    key: &str,
    expires: Duration,
    max_bytes: u64,
) -> Result<ProxyPresign> {
    let secret = require_secret(secret)?;
    if expires.is_zero() {
        return Err(ForgeError::invalid("presign expires must be positive"));
    }
    if expires > MAX_PRESIGN_EXPIRES {
        return Err(ForgeError::limit(
            "presign expires exceeds the 7-day maximum",
        ));
    }
    // A 0-byte upload cap admits only empty bodies, almost certainly a caller bug.
    if matches!(method, Method::Put) && max_bytes == 0 {
        return Err(ForgeError::invalid(
            "presign_upload max_bytes must be positive (0 admits only empty bodies)",
        ));
    }
    let expires_epoch = unix_secs(SystemTime::now() + expires);
    let sig = sign::sign(secret, namespace, method, key, expires_epoch, max_bytes)?;
    let enc_key = utf8_percent_encode(key, NON_ALPHANUMERIC);
    let url = format!(
        "{base_url}?v=1&ns={}&method={}&key={enc_key}&expires={expires_epoch}&max_bytes={max_bytes}&sig={sig}",
        utf8_percent_encode(namespace, NON_ALPHANUMERIC),
        method.as_str(),
    );
    Ok(ProxyPresign {
        url,
        method: method.as_str().to_string(),
        key: key.to_string(),
        expires_epoch,
        max_bytes,
        signature: sig,
        required_headers: std::collections::BTreeMap::new(),
    })
}

/// Verify a presigned URL's parameters against `secret`. `Config` with no secret,
/// `Invalid` for a non-GET/PUT method, `Ok(false)` for an expired URL or bad signature.
pub(super) fn verify_presigned(
    secret: Option<&[u8]>,
    namespace: &str,
    method: &str,
    key: &str,
    expires_epoch: i64,
    max_bytes: u64,
    sig: &str,
) -> Result<bool> {
    let secret = require_secret(secret)?;
    let method = match method.to_ascii_uppercase().as_str() {
        "GET" => Method::Get,
        "PUT" => Method::Put,
        other => {
            return Err(ForgeError::invalid(format!(
                "presign method must be GET or PUT, got {other:?}"
            )));
        }
    };
    // Expired URLs fail verification (matching the router's expiry check) before the
    // constant-time signature compare.
    if expires_epoch <= unix_secs(SystemTime::now()) {
        return Ok(false);
    }
    Ok(sign::verify(
        secret,
        namespace,
        method,
        key,
        expires_epoch,
        max_bytes,
        sig,
    ))
}

/// State every blob backend carries: the key namespace and the presign signing config.
/// Both backends (`PgBlob`, `FsBlob`) embed one and route the backend-agnostic work
/// (key namespacing and presigned-URL mint/verify) through it, so those can never
/// diverge between the two stores.
pub(super) struct Shared {
    namespace: String,
    /// HMAC key for presigned URLs. `None` => presigning is unconfigured and errors.
    secret: Option<Vec<u8>>,
    /// URL prefix presigned URLs point at (where the host app serves them).
    base_url: String,
}

impl Shared {
    pub(super) fn new(namespace: String, secret: Option<Vec<u8>>, base_url: String) -> Self {
        Self {
            namespace,
            secret,
            base_url,
        }
    }

    pub(super) fn physical(&self, key: &str) -> String {
        physical(&self.namespace, key)
    }

    pub(super) fn logical<'a>(&self, stored: &'a str) -> &'a str {
        logical(&self.namespace, stored)
    }

    /// Mint a presigned upload URL: validate, then sign. Identical across backends.
    pub(super) async fn presign_upload(
        &self,
        key: &str,
        expires: Duration,
        max_bytes: u64,
    ) -> Result<ProxyPresign> {
        let span = tracing::info_span!(
            "forge.blob.presign_upload",
            blob.key_hash = %crate::util::key_hash(key),
            blob.presign_expires_secs = expires.as_secs(),
            blob.presign_max_bytes = max_bytes,
            outcome = tracing::field::Empty,
            error.variant = tracing::field::Empty,
        );
        crate::obs::instrument("blob", "presign_upload", span, async move {
            check_key(key)?;
            if max_bytes > MAX_OBJECT_BYTES as u64 {
                return Err(ForgeError::limit(
                    "presign max_bytes exceeds the 50 MiB object cap",
                ));
            }
            presign_url(
                self.secret.as_deref(),
                &self.base_url,
                &self.namespace,
                Method::Put,
                key,
                expires,
                max_bytes,
            )
        })
        .await
    }

    /// Mint a presigned download URL. Identical across backends.
    pub(super) async fn presign_download(
        &self,
        key: &str,
        expires: Duration,
    ) -> Result<ProxyPresign> {
        let span = tracing::info_span!(
            "forge.blob.presign_download",
            blob.key_hash = %crate::util::key_hash(key),
            blob.presign_expires_secs = expires.as_secs(),
            outcome = tracing::field::Empty,
            error.variant = tracing::field::Empty,
        );
        crate::obs::instrument("blob", "presign_download", span, async move {
            check_key(key)?;
            presign_url(
                self.secret.as_deref(),
                &self.base_url,
                &self.namespace,
                Method::Get,
                key,
                expires,
                0,
            )
        })
        .await
    }

    /// Verify a presigned URL's params against the secret. Identical across backends.
    pub(super) fn verify_presigned(
        &self,
        method: &str,
        key: &str,
        expires_epoch: i64,
        max_bytes: u64,
        sig: &str,
    ) -> Result<bool> {
        verify_presigned(
            self.secret.as_deref(),
            &self.namespace,
            method,
            key,
            expires_epoch,
            max_bytes,
            sig,
        )
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn like_escape_neutralizes_wildcards() {
        assert_eq!(like_escape("a%b_c"), "a\\%b\\_c");
    }

    #[test]
    fn presign_requires_a_secret() {
        let err = presign_url(
            None,
            "/api/files",
            "",
            Method::Get,
            "k",
            Duration::from_secs(60),
            0,
        )
        .unwrap_err();
        // Missing signing secret is a configuration problem, classified `Config`
        // consistently across presign_* and verify_presigned.
        assert!(matches!(err, ForgeError::Config(_)));

        for secret in [b"".as_slice(), b"   ".as_slice()] {
            let err = presign_url(
                Some(secret),
                "/api/files",
                "",
                Method::Get,
                "k",
                Duration::from_secs(60),
                0,
            )
            .unwrap_err();
            assert!(matches!(err, ForgeError::Config(_)));
        }
    }

    #[test]
    fn presigned_url_carries_signed_params() {
        let ticket = presign_url(
            Some(b"secret"),
            "/api/files",
            "app",
            Method::Put,
            "exports/a b.csv",
            Duration::from_secs(60),
            1024,
        )
        .unwrap();
        assert!(
            ticket
                .url
                .starts_with("/api/files?v=1&ns=app&method=PUT&key=")
        );
        assert!(ticket.url.contains("max_bytes=1024"));
        assert!(ticket.url.contains("sig="));
        // The space in the key is percent-encoded (NON_ALPHANUMERIC also encodes `.`/`/`).
        assert!(ticket.url.contains("a%20b"));
    }
}

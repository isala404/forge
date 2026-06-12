//! Postgres `blob` backend. Contract: docs/contracts/blob.md.
//!
//! One `forge_blobs` row per object, body in `BYTEA` (whole-body, ≤ 50 MiB in v1).
//! Presigned URLs carry the key + expiry + size cap as query params and an
//! HMAC-SHA256 signature (see [`super::sign`]); they resolve through the optional
//! `blob-router`, which verifies the signature and performs the equivalent get/put.

use super::sign::{self, Method};
use super::{
    Blob, BlobInfo, DEFAULT_CONTENT_TYPE, ListPage, MAX_CONTENT_TYPE_BYTES, MAX_KEY_BYTES,
    MAX_METADATA_BYTES, MAX_OBJECT_BYTES, MAX_PRESIGN_EXPIRES, PutOpts,
};
use crate::error::{ForgeError, Result};
use crate::obs;
use crate::types::Cursor;
use crate::util::{key_hash, sha256_hex};
use async_trait::async_trait;
use bytes::Bytes;
use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use sqlx::PgPool;
use sqlx::types::Json;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tracing::field::Empty;

type Meta = BTreeMap<String, String>;

/// Postgres-backed [`Blob`].
pub(crate) struct PgBlob {
    pool: PgPool,
    namespace: String,
    /// HMAC key for presigned URLs. `None` => presigning is unconfigured and errors.
    secret: Option<Vec<u8>>,
    /// URL prefix the `blob-router` is mounted at; presigned URLs point here.
    base_url: String,
}

impl PgBlob {
    pub(crate) fn new(
        pool: PgPool,
        namespace: String,
        secret: Option<Vec<u8>>,
        base_url: String,
    ) -> Self {
        Self {
            pool,
            namespace,
            secret,
            base_url,
        }
    }

    fn physical(&self, key: &str) -> String {
        if self.namespace.is_empty() {
            key.to_string()
        } else {
            format!("{}:{}", self.namespace, key)
        }
    }

    fn logical<'a>(&self, stored: &'a str) -> &'a str {
        if self.namespace.is_empty() {
            stored
        } else {
            stored
                .strip_prefix(&self.namespace)
                .and_then(|s| s.strip_prefix(':'))
                .unwrap_or(stored)
        }
    }

    /// The HMAC signing secret, for the router's verification path.
    #[cfg(feature = "blob-router")]
    pub(crate) fn signing_secret(&self) -> Option<&[u8]> {
        self.secret.as_deref()
    }

    fn build_presigned(
        &self,
        method: Method,
        key: &str,
        expires: Duration,
        max_bytes: u64,
    ) -> Result<String> {
        presign_url(
            self.secret.as_deref(),
            &self.base_url,
            method,
            key,
            expires,
            max_bytes,
        )
    }
}

/// Build a signed URL (pool-free, so it is unit-testable without a database).
fn presign_url(
    secret: Option<&[u8]>,
    base_url: &str,
    method: Method,
    key: &str,
    expires: Duration,
    max_bytes: u64,
) -> Result<String> {
    let secret = secret.ok_or_else(|| {
        ForgeError::invalid(
            "blob signing secret is not configured (set ForgeConfig.blob_signing_secret)",
        )
    })?;
    if expires.is_zero() {
        return Err(ForgeError::invalid("presign expires must be positive"));
    }
    if expires > MAX_PRESIGN_EXPIRES {
        return Err(ForgeError::limit(
            "presign expires exceeds the 7-day maximum",
        ));
    }
    let expires_epoch = unix_secs(SystemTime::now() + expires);
    let sig = sign::sign(secret, method, key, expires_epoch, max_bytes)?;
    let enc_key = utf8_percent_encode(key, NON_ALPHANUMERIC);
    Ok(format!(
        "{base_url}?key={enc_key}&expires={expires_epoch}&max_bytes={max_bytes}&sig={sig}"
    ))
}

fn check_key(key: &str) -> Result<()> {
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

fn check_put(data: &[u8], opts: &PutOpts) -> Result<()> {
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
    Ok(())
}

/// Whole seconds since the Unix epoch (saturating; times are always future here).
fn unix_secs(t: SystemTime) -> i64 {
    t.duration_since(UNIX_EPOCH)
        .map(|d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or(0)
}

/// Escape `LIKE` wildcards so a caller prefix matches literally.
fn like_escape(prefix: &str) -> String {
    let mut out = String::with_capacity(prefix.len() + 2);
    for c in prefix.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[async_trait]
impl Blob for PgBlob {
    async fn put(&self, key: &str, data: Bytes, opts: PutOpts) -> Result<()> {
        let span = tracing::info_span!(
            "forge.blob.put",
            blob.key_hash = %key_hash(key),
            blob.size_bytes = data.len(),
            blob.etag = Empty,
            blob.meta_count = opts.metadata.len(),
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("blob", "put", span, async move {
            check_key(key)?;
            check_put(&data, &opts)?;
            let pk = self.physical(key);
            let etag = sha256_hex(&data);
            let content_type = opts
                .content_type
                .unwrap_or_else(|| DEFAULT_CONTENT_TYPE.to_string());
            let size = i64::try_from(data.len()).unwrap_or(i64::MAX);
            tracing::Span::current().record("blob.etag", etag.as_str());
            sqlx::query!(
                "INSERT INTO forge_blobs (key, data, content_type, etag, metadata, size, last_modified) \
                 VALUES ($1, $2, $3, $4, $5, $6, now()) \
                 ON CONFLICT (key) DO UPDATE SET \
                   data = EXCLUDED.data, content_type = EXCLUDED.content_type, \
                   etag = EXCLUDED.etag, metadata = EXCLUDED.metadata, \
                   size = EXCLUDED.size, last_modified = EXCLUDED.last_modified",
                pk,
                data.as_ref(),
                content_type,
                etag,
                Json(&opts.metadata) as _,
                size,
            )
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }

    async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        let span = tracing::info_span!(
            "forge.blob.get",
            blob.key_hash = %key_hash(key),
            blob.hit = Empty,
            blob.size_bytes = Empty,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("blob", "get", span, async move {
            check_key(key)?;
            let pk = self.physical(key);
            let row = sqlx::query_scalar!("SELECT data FROM forge_blobs WHERE key = $1", pk)
                .fetch_optional(&self.pool)
                .await?;
            let value = row.map(Bytes::from);
            let s = tracing::Span::current();
            s.record("blob.hit", value.is_some());
            if let Some(v) = &value {
                s.record("blob.size_bytes", v.len());
            }
            Ok(value)
        })
        .await
    }

    async fn head(&self, key: &str) -> Result<Option<BlobInfo>> {
        let span = tracing::info_span!(
            "forge.blob.head",
            blob.key_hash = %key_hash(key),
            blob.hit = Empty,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("blob", "head", span, async move {
            check_key(key)?;
            let pk = self.physical(key);
            let row = sqlx::query!(
                r#"SELECT content_type, etag, size,
                          metadata AS "metadata: Json<Meta>", last_modified
                   FROM forge_blobs WHERE key = $1"#,
                pk
            )
            .fetch_optional(&self.pool)
            .await?;
            tracing::Span::current().record("blob.hit", row.is_some());
            Ok(row.map(|r| BlobInfo {
                key: key.to_string(),
                size: u64::try_from(r.size).unwrap_or(0),
                content_type: r.content_type,
                etag: r.etag,
                last_modified: r.last_modified.into(),
                metadata: r.metadata.0,
            }))
        })
        .await
    }

    async fn delete(&self, key: &str) -> Result<bool> {
        let span = tracing::info_span!(
            "forge.blob.delete",
            blob.key_hash = %key_hash(key),
            blob.hit = Empty,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("blob", "delete", span, async move {
            check_key(key)?;
            let pk = self.physical(key);
            let removed =
                sqlx::query_scalar!("DELETE FROM forge_blobs WHERE key = $1 RETURNING key", pk)
                    .fetch_optional(&self.pool)
                    .await?
                    .is_some();
            tracing::Span::current().record("blob.hit", removed);
            Ok(removed)
        })
        .await
    }

    async fn list(&self, prefix: &str, cursor: Option<Cursor>, limit: u32) -> Result<ListPage> {
        let physical_prefix = self.physical(prefix);
        let pattern = format!("{}%", like_escape(&physical_prefix));
        let limit_i = i64::from(limit.clamp(1, 1000));
        let after = cursor.as_ref().map(|c| c.as_str().to_string());
        let span = tracing::info_span!(
            "forge.blob.list",
            blob.list_returned = Empty,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("blob", "list", span, async move {
            let rows = sqlx::query!(
                r#"SELECT key, content_type, etag, size,
                          metadata AS "metadata: Json<Meta>", last_modified
                   FROM forge_blobs
                   WHERE key LIKE $1 ESCAPE '\' AND ($2::text IS NULL OR key > $2)
                   ORDER BY key LIMIT $3"#,
                pattern,
                after,
                limit_i,
            )
            .fetch_all(&self.pool)
            .await?;

            let next = if (rows.len() as i64) < limit_i {
                None
            } else {
                rows.last().map(|r| Cursor::new(r.key.clone()))
            };
            let items = rows
                .into_iter()
                .map(|r| BlobInfo {
                    key: self.logical(&r.key).to_string(),
                    size: u64::try_from(r.size).unwrap_or(0),
                    content_type: r.content_type,
                    etag: r.etag,
                    last_modified: r.last_modified.into(),
                    metadata: r.metadata.0,
                })
                .collect::<Vec<_>>();
            tracing::Span::current().record("blob.list_returned", items.len());
            Ok(ListPage { items, next })
        })
        .await
    }

    async fn presign_upload(&self, key: &str, expires: Duration, max_bytes: u64) -> Result<String> {
        let span = tracing::info_span!(
            "forge.blob.presign_upload",
            blob.key_hash = %key_hash(key),
            blob.presign_expires_secs = expires.as_secs(),
            blob.presign_max_bytes = max_bytes,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("blob", "presign_upload", span, async move {
            check_key(key)?;
            if max_bytes > MAX_OBJECT_BYTES as u64 {
                return Err(ForgeError::limit(
                    "presign max_bytes exceeds the 50 MiB object cap",
                ));
            }
            self.build_presigned(Method::Put, key, expires, max_bytes)
        })
        .await
    }

    async fn presign_download(&self, key: &str, expires: Duration) -> Result<String> {
        let span = tracing::info_span!(
            "forge.blob.presign_download",
            blob.key_hash = %key_hash(key),
            blob.presign_expires_secs = expires.as_secs(),
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("blob", "presign_download", span, async move {
            check_key(key)?;
            self.build_presigned(Method::Get, key, expires, 0)
        })
        .await
    }

    async fn verify_presigned(
        &self,
        method: &str,
        key: &str,
        expires_epoch: i64,
        max_bytes: u64,
        sig: &str,
    ) -> Result<bool> {
        let secret = self.secret.as_deref().ok_or_else(|| {
            ForgeError::config(
                "blob signing secret is not configured (set ForgeConfig.blob_signing_secret)",
            )
        })?;
        let method = match method.to_ascii_uppercase().as_str() {
            "GET" => Method::Get,
            "PUT" => Method::Put,
            other => {
                return Err(ForgeError::invalid(format!(
                    "presign method must be GET or PUT, got {other:?}"
                )));
            }
        };
        // Expired URLs fail verification (matching the router's expiry check) before
        // the constant-time signature compare.
        if expires_epoch <= unix_secs(SystemTime::now()) {
            return Ok(false);
        }
        Ok(sign::verify(
            secret,
            method,
            key,
            expires_epoch,
            max_bytes,
            sig,
        ))
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
            "/_forge/blob",
            Method::Get,
            "k",
            Duration::from_secs(60),
            0,
        )
        .unwrap_err();
        assert!(matches!(err, ForgeError::Invalid(_)));
    }

    #[test]
    fn presigned_url_carries_signed_params() {
        let url = presign_url(
            Some(b"secret"),
            "/_forge/blob",
            Method::Put,
            "exports/a b.csv",
            Duration::from_secs(60),
            1024,
        )
        .unwrap();
        assert!(url.starts_with("/_forge/blob?key="));
        assert!(url.contains("max_bytes=1024"));
        assert!(url.contains("sig="));
        // The space in the key is percent-encoded (NON_ALPHANUMERIC also encodes `.`/`/`).
        assert!(url.contains("a%20b"));
    }
}

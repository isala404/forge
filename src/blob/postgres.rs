//! Postgres `blob` backend. Contract: docs/contracts/blob.md.
//!
//! One `forge_blobs` row per object, body in `BYTEA` (whole-body, ≤ 50 MiB in v1).
//! Presigned URLs carry the key + expiry + size cap as query params and an
//! HMAC-SHA256 signature (see [`super::sign`]); the host app serves them and verifies
//! the signature via [`Blob::verify_presigned`] before the equivalent get/put.
//! Shared, backend-agnostic helpers (key checks, namespace mapping, presign/verify)
//! live in [`super::common`].

use super::common;
use super::{Blob, BlobInfo, DEFAULT_CONTENT_TYPE, ListPage, PutOpts};
use crate::error::Result;
use crate::obs;
use crate::types::Cursor;
use crate::util::{key_hash, sha256_hex};
use async_trait::async_trait;
use bytes::Bytes;
use sqlx::PgPool;
use sqlx::types::Json;
use std::collections::BTreeMap;
use std::time::Duration;
use tracing::field::Empty;

type Meta = BTreeMap<String, String>;

/// Postgres-backed [`Blob`].
pub(crate) struct PgBlob {
    pool: PgPool,
    shared: common::Shared,
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
            shared: common::Shared::new(namespace, secret, base_url),
        }
    }
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
            common::check_key(key)?;
            common::check_put(&data, &opts)?;
            let pk = self.shared.physical(key);
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
            common::check_key(key)?;
            let pk = self.shared.physical(key);
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
            common::check_key(key)?;
            let pk = self.shared.physical(key);
            let row = sqlx::query!(
                r#"SELECT content_type, etag, size,
                          metadata AS "metadata: Json<Meta>", last_modified
                   FROM forge_blobs WHERE key = $1"#,
                pk
            )
            .fetch_optional(&self.pool)
            .await?;
            tracing::Span::current().record("blob.hit", row.is_some());
            Ok(row.map(|r| {
                BlobInfo::new(
                    key.to_string(),
                    u64::try_from(r.size).unwrap_or(0),
                    r.content_type,
                    r.etag,
                    r.last_modified.into(),
                    r.metadata.0,
                )
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
            common::check_key(key)?;
            let pk = self.shared.physical(key);
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
        let physical_prefix = self.shared.physical(prefix);
        let pattern = format!("{}%", common::like_escape(&physical_prefix));
        let limit_i = i64::from(limit.clamp(1, 1000));
        let after = cursor.as_ref().map(|c| c.token().to_string());
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
                rows.last().map(|r| Cursor::from_token(r.key.clone()))
            };
            let items = rows
                .into_iter()
                .map(|r| {
                    BlobInfo::new(
                        self.shared.logical(&r.key).to_string(),
                        u64::try_from(r.size).unwrap_or(0),
                        r.content_type,
                        r.etag,
                        r.last_modified.into(),
                        r.metadata.0,
                    )
                })
                .collect::<Vec<_>>();
            tracing::Span::current().record("blob.list_returned", items.len());
            Ok(ListPage::new(items, next))
        })
        .await
    }

    async fn presign_upload(&self, key: &str, expires: Duration, max_bytes: u64) -> Result<String> {
        self.shared.presign_upload(key, expires, max_bytes).await
    }

    async fn presign_download(&self, key: &str, expires: Duration) -> Result<String> {
        self.shared.presign_download(key, expires).await
    }

    async fn verify_presigned(
        &self,
        method: &str,
        key: &str,
        expires_epoch: i64,
        max_bytes: u64,
        sig: &str,
    ) -> Result<bool> {
        self.shared
            .verify_presigned(method, key, expires_epoch, max_bytes, sig)
    }
}

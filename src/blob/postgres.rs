use super::common;
use super::{
    Blob, BlobInfo, BlobSummary, ConditionalGet, DEFAULT_CONTENT_TYPE, ListPage, ProxyPresign,
    PutOpts, PutPrecondition,
};
use crate::error::Result;
use crate::obs;
use crate::types::Cursor;
use crate::util::{key_hash, sha256_hex};
use async_trait::async_trait;
use bytes::Bytes;
use sqlx::PgPool;
use sqlx::Row;
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
    #[allow(clippy::disallowed_methods)]
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
            common::reject_s3_encryption(&opts)?;
            let pk = self.shared.physical(key);
            let etag = sha256_hex(&data);
            let content_type = opts
                .content_type
                .unwrap_or_else(|| DEFAULT_CONTENT_TYPE.to_string());
            let size = i64::try_from(data.len()).unwrap_or(i64::MAX);
            tracing::Span::current().record("blob.etag", etag.as_str());
            let (condition, expected) = match &opts.precondition {
                None => ("any", None),
                Some(PutPrecondition::CreateOnly) => ("create", None),
                Some(PutPrecondition::MatchVersion(etag)) => ("match", Some(etag.as_str())),
            };
            let checksum_sha256 = etag.clone();
            let written = sqlx::query_scalar::<_, String>(
                "INSERT INTO forge_blobs (key, data, content_type, etag, metadata, size, last_modified, \
                   cache_control, content_disposition, checksum_sha256) \
                 VALUES ($1, $2, $3, $4, $5, $6, now(), $7, $8, $9) \
                 ON CONFLICT (key) DO UPDATE SET \
                   data = EXCLUDED.data, content_type = EXCLUDED.content_type, \
                   etag = EXCLUDED.etag, metadata = EXCLUDED.metadata, \
                   size = EXCLUDED.size, last_modified = EXCLUDED.last_modified, \
                   cache_control = EXCLUDED.cache_control, \
                   content_disposition = EXCLUDED.content_disposition, \
                   checksum_sha256 = EXCLUDED.checksum_sha256 \
                 WHERE $10 = 'any' OR ($10 = 'match' AND forge_blobs.etag = $11) \
                 RETURNING key",
            )
            .bind(pk)
            .bind(data.as_ref())
            .bind(content_type)
            .bind(etag)
            .bind(Json(&opts.metadata))
            .bind(size)
            .bind(opts.cache_control)
            .bind(opts.content_disposition)
            .bind(checksum_sha256)
            .bind(condition)
            .bind(expected)
            .fetch_optional(&self.pool)
            .await?;
            if written.is_none() {
                return Err(crate::error::ForgeError::precondition(
                    "blob write precondition failed",
                ));
            }
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

    #[allow(clippy::disallowed_methods)]
    async fn get_if(
        &self,
        key: &str,
        if_match: Option<&str>,
        if_none_match: Option<&str>,
    ) -> Result<ConditionalGet> {
        common::check_key(key)?;
        common::check_get_conditions(if_match, if_none_match)?;
        let row = sqlx::query("SELECT data, etag FROM forge_blobs WHERE key = $1")
            .bind(self.shared.physical(key))
            .fetch_optional(&self.pool)
            .await?;
        let Some(row) = row else {
            return Ok(ConditionalGet::Missing);
        };
        let etag: String = row.try_get("etag")?;
        if if_match.is_some_and(|expected| expected != etag) {
            return Err(crate::ForgeError::precondition(
                "blob read version does not match",
            ));
        }
        if if_none_match.is_some_and(|version| version == etag) {
            return Ok(ConditionalGet::NotModified { etag });
        }
        Ok(ConditionalGet::Found {
            body: Bytes::from(row.try_get::<Vec<u8>, _>("data")?),
            etag,
        })
    }

    #[allow(clippy::disallowed_methods)]
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
            let row = sqlx::query(
                "SELECT content_type, etag, size, metadata, last_modified, \
                        cache_control, content_disposition, checksum_sha256 \
                 FROM forge_blobs WHERE key = $1",
            )
            .bind(pk)
            .fetch_optional(&self.pool)
            .await?;
            tracing::Span::current().record("blob.hit", row.is_some());
            row.map(|r| -> Result<BlobInfo> {
                let metadata: Json<Meta> = r.try_get("metadata")?;
                let last_modified: chrono::DateTime<chrono::Utc> = r.try_get("last_modified")?;
                Ok(BlobInfo::new(
                    key.to_string(),
                    u64::try_from(r.try_get::<i64, _>("size")?).unwrap_or(0),
                    r.try_get("content_type")?,
                    r.try_get("etag")?,
                    last_modified.into(),
                    metadata.0,
                )
                .with_storage_metadata(
                    r.try_get("cache_control")?,
                    r.try_get("content_disposition")?,
                    r.try_get("checksum_sha256")?,
                    None,
                ))
            })
            .transpose()
        })
        .await
    }

    async fn delete(&self, key: &str) -> Result<()> {
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
            sqlx::query!("DELETE FROM forge_blobs WHERE key = $1", pk)
                .execute(&self.pool)
                .await?;
            Ok(())
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
                   ORDER BY key LIMIT ($3::bigint + 1)"#,
                pattern,
                after,
                limit_i,
            )
            .fetch_all(&self.pool)
            .await?;

            let next = if (rows.len() as i64) > limit_i {
                rows.get(usize::try_from(limit_i - 1).unwrap_or(0))
                    .map(|r| Cursor::from_token(r.key.clone()))
            } else {
                None
            };
            let items = rows
                .into_iter()
                .take(usize::try_from(limit_i).unwrap_or(1000))
                .map(|r| {
                    BlobSummary::new(
                        self.shared.logical(&r.key).to_string(),
                        u64::try_from(r.size).unwrap_or(0),
                        r.etag,
                        r.last_modified.into(),
                    )
                })
                .collect::<Vec<_>>();
            tracing::Span::current().record("blob.list_returned", items.len());
            Ok(ListPage::new(items, next))
        })
        .await
    }

    async fn presign_upload(
        &self,
        key: &str,
        expires: Duration,
        max_bytes: u64,
    ) -> Result<ProxyPresign> {
        self.shared.presign_upload(key, expires, max_bytes).await
    }

    async fn presign_download(&self, key: &str, expires: Duration) -> Result<ProxyPresign> {
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

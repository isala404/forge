use super::common;
use super::{Blob, BlobInfo, DEFAULT_CONTENT_TYPE, ListPage, PutOpts};
use crate::error::{ForgeError, Result};
use crate::obs;
use crate::types::Cursor;
use crate::util::{key_hash, sha256_hex};
use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use sqlx::types::Json;
use sqlx::{PgPool, Row};
use std::collections::{BTreeMap, HashSet};
use std::path::PathBuf;
use std::time::{Duration, SystemTime};
use tracing::field::Empty;
use uuid::Uuid;

type Meta = BTreeMap<String, String>;

/// Unreferenced files younger than this are left alone by the orphan sweep, so it never
/// races a `put` that has written its file but not yet committed its metadata row.
const ORPHAN_GRACE: Duration = Duration::from_secs(60 * 60);

/// Filesystem-backed [`Blob`]: metadata in `forge_fs_blobs`, bytes under `root`.
pub(crate) struct FsBlob {
    pool: PgPool,
    shared: common::Shared,
    root: PathBuf,
}

impl FsBlob {
    /// Build the backend, creating the root directory if it does not exist. A directory
    /// that cannot be created is a [`ForgeError::Config`] (init-time misconfiguration).
    pub(crate) fn new(
        pool: PgPool,
        namespace: String,
        secret: Option<Vec<u8>>,
        base_url: String,
        root: PathBuf,
    ) -> Result<Self> {
        std::fs::create_dir_all(&root).map_err(|e| {
            ForgeError::config(format!("could not create blob root directory: {e}"))
        })?;
        Ok(Self {
            pool,
            shared: common::Shared::new(namespace, secret, base_url),
            root,
        })
    }

    /// A fresh content path, sharded into a two-char prefix directory so one directory
    /// never holds the whole keyspace: `"ab/ab34…"`.
    fn new_ref() -> String {
        let id = Uuid::new_v4().simple().to_string();
        let prefix = id.get(..2).unwrap_or("00");
        format!("{prefix}/{id}")
    }

    /// Reclaim files on disk that no metadata row references (a `put` that wrote its file
    /// then crashed before committing, or an overwrite's superseded file). Only files
    /// older than [`ORPHAN_GRACE`] are removed, so an in-flight `put` is never swept.
    /// Registered on the maintenance hook.
    pub(crate) async fn sweep_orphans(&self) -> Result<u64> {
        let referenced: HashSet<String> =
            sqlx::query_scalar!("SELECT data_ref FROM forge_fs_blobs")
                .fetch_all(&self.pool)
                .await?
                .into_iter()
                .collect();

        let now = SystemTime::now();
        let mut removed = 0u64;
        let mut prefixes = match tokio::fs::read_dir(&self.root).await {
            Ok(d) => d,
            // Nothing written yet (or root vanished); nothing to sweep.
            Err(_) => return Ok(0),
        };
        while let Some(prefix_entry) = prefixes.next_entry().await.map_err(fs_err)? {
            let is_dir = prefix_entry
                .file_type()
                .await
                .map(|t| t.is_dir())
                .unwrap_or(false);
            if !is_dir {
                continue;
            }
            let prefix_name = prefix_entry.file_name().to_string_lossy().to_string();
            let mut files = match tokio::fs::read_dir(prefix_entry.path()).await {
                Ok(f) => f,
                Err(_) => continue,
            };
            while let Some(file) = files.next_entry().await.map_err(fs_err)? {
                let rel = format!("{prefix_name}/{}", file.file_name().to_string_lossy());
                if referenced.contains(&rel) {
                    continue;
                }
                let old_enough = file
                    .metadata()
                    .await
                    .ok()
                    .and_then(|m| m.modified().ok())
                    .and_then(|mtime| now.duration_since(mtime).ok())
                    .map(|age| age > ORPHAN_GRACE)
                    .unwrap_or(false);
                if old_enough && tokio::fs::remove_file(file.path()).await.is_ok() {
                    removed += 1;
                }
            }
        }
        Ok(removed)
    }

    async fn data_file_exists(&self, data_ref: &str) -> Result<bool> {
        match tokio::fs::metadata(self.root.join(data_ref)).await {
            Ok(meta) => Ok(meta.is_file()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                tracing::warn!(
                    data_ref = %data_ref,
                    "blob row resolves to a missing file (crash window, or replicas without a shared mount)"
                );
                Ok(false)
            }
            Err(e) => Err(fs_err(e)),
        }
    }

    async fn refresh_orphan_grace(&self, data_ref: &str) -> Result<()> {
        let path = self.root.join(data_ref);
        let data_ref = data_ref.to_string();
        tokio::task::spawn_blocking(move || {
            match std::fs::OpenOptions::new().write(true).open(&path) {
                Ok(file) => file.set_modified(SystemTime::now()).map_err(fs_err),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    tracing::warn!(
                        data_ref = %data_ref,
                        "overwritten blob row pointed at a missing old file"
                    );
                    Ok(())
                }
                Err(e) => Err(fs_err(e)),
            }
        })
        .await
        .map_err(|e| ForgeError::backend_with("blob filesystem task join error", true, e))?
    }
}

/// Map a filesystem io error to a secret-safe backend error (the raw cause, which may
/// name a path, is preserved on `source()` but never rendered by `Display`).
fn fs_err(e: std::io::Error) -> ForgeError {
    ForgeError::backend_with("blob filesystem error", false, e)
}

#[async_trait]
impl Blob for FsBlob {
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

            // Write the bytes BEFORE touching the DB: a crash here leaves an orphan file
            // (swept later), never a row pointing at missing bytes. Write to a temp name
            // then rename so a reader never sees a half-written object.
            let data_ref = Self::new_ref();
            let path = self.root.join(&data_ref);
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(fs_err)?;
            }
            let tmp = path.with_extension("tmp");
            tokio::fs::write(&tmp, data.as_ref())
                .await
                .map_err(fs_err)?;
            tokio::fs::rename(&tmp, &path).await.map_err(fs_err)?;

            // Commit metadata. If overwriting, refresh the superseded file's mtime while
            // the row still references it: after commit it becomes an orphan, but the
            // sweep's grace window then starts now, so concurrent readers that saw the
            // old row never observe a normal overwrite as missing.
            let mut tx = self.pool.begin().await?;
            let old_ref = sqlx::query_scalar!(
                "SELECT data_ref FROM forge_fs_blobs WHERE key = $1 FOR UPDATE",
                pk
            )
            .fetch_optional(&mut *tx)
            .await?;
            if let Some(old) = old_ref.as_deref()
                && old != data_ref
            {
                self.refresh_orphan_grace(old).await?;
            }
            sqlx::query!(
                "INSERT INTO forge_fs_blobs \
                   (key, data_ref, content_type, etag, metadata, size, last_modified) \
                 VALUES ($1, $2, $3, $4, $5, $6, now()) \
                 ON CONFLICT (key) DO UPDATE SET \
                   data_ref = EXCLUDED.data_ref, content_type = EXCLUDED.content_type, \
                   etag = EXCLUDED.etag, metadata = EXCLUDED.metadata, \
                   size = EXCLUDED.size, last_modified = EXCLUDED.last_modified",
                pk,
                data_ref,
                content_type,
                etag,
                Json(&opts.metadata) as _,
                size,
            )
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
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
            let data_ref =
                sqlx::query_scalar!("SELECT data_ref FROM forge_fs_blobs WHERE key = $1", pk)
                    .fetch_optional(&self.pool)
                    .await?;
            let s = tracing::Span::current();
            let Some(data_ref) = data_ref else {
                s.record("blob.hit", false);
                return Ok(None);
            };
            match tokio::fs::read(self.root.join(&data_ref)).await {
                Ok(bytes) => {
                    s.record("blob.hit", true);
                    s.record("blob.size_bytes", bytes.len());
                    Ok(Some(Bytes::from(bytes)))
                }
                // Row points at a missing file: treat as not-found per the contract, but
                // warn. A live row resolving to no file is the crash window during a put,
                // or the tell of a multi-replica deploy without a shared mount (each
                // replica sees only its own files).
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    s.record("blob.hit", false);
                    tracing::warn!(
                        data_ref = %data_ref,
                        "blob row resolves to a missing file (crash window, or replicas without a shared mount)"
                    );
                    Ok(None)
                }
                Err(e) => Err(fs_err(e)),
            }
        })
        .await
    }

    // Runtime SQL until the offline sqlx metadata is regenerated for the data_ref column.
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
                r#"SELECT data_ref, content_type, etag, size, metadata, last_modified
                   FROM forge_fs_blobs WHERE key = $1"#,
            )
            .bind(pk)
            .fetch_optional(&self.pool)
            .await?;
            let Some(row) = row else {
                tracing::Span::current().record("blob.hit", false);
                return Ok(None);
            };
            let data_ref: String = row.try_get("data_ref")?;
            if !self.data_file_exists(&data_ref).await? {
                tracing::Span::current().record("blob.hit", false);
                return Ok(None);
            }
            let size: i64 = row.try_get("size")?;
            let metadata: Json<Meta> = row.try_get("metadata")?;
            let last_modified: DateTime<Utc> = row.try_get("last_modified")?;
            tracing::Span::current().record("blob.hit", true);
            Ok(Some(BlobInfo::new(
                key.to_string(),
                u64::try_from(size).unwrap_or(0),
                row.try_get("content_type")?,
                row.try_get("etag")?,
                last_modified.into(),
                metadata.0,
            )))
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
            let removed = sqlx::query_scalar!(
                "DELETE FROM forge_fs_blobs WHERE key = $1 RETURNING data_ref",
                pk
            )
            .fetch_optional(&self.pool)
            .await?;
            let hit = removed.is_some();
            tracing::Span::current().record("blob.hit", hit);
            if let Some(data_ref) = removed {
                // Best-effort file removal; an orphaned file is swept later.
                let _ = tokio::fs::remove_file(self.root.join(&data_ref)).await;
            }
            Ok(hit)
        })
        .await
    }

    // Runtime SQL until the offline sqlx metadata is regenerated for the data_ref column.
    #[allow(clippy::disallowed_methods)]
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
            let rows = sqlx::query(
                r#"SELECT key, data_ref, content_type, etag, size, metadata, last_modified
                   FROM forge_fs_blobs
                   WHERE key LIKE $1 ESCAPE '\' AND ($2::text IS NULL OR key > $2)
                   ORDER BY key LIMIT $3"#,
            )
            .bind(pattern)
            .bind(after)
            .bind(limit_i)
            .fetch_all(&self.pool)
            .await?;

            let next = if (rows.len() as i64) < limit_i {
                None
            } else {
                rows.last()
                    .and_then(|r| r.try_get::<String, _>("key").ok())
                    .map(Cursor::from_token)
            };
            let mut items = Vec::new();
            for r in rows {
                let data_ref: String = r.try_get("data_ref")?;
                if !self.data_file_exists(&data_ref).await? {
                    continue;
                }
                let key: String = r.try_get("key")?;
                let size: i64 = r.try_get("size")?;
                let metadata: Json<Meta> = r.try_get("metadata")?;
                let last_modified: DateTime<Utc> = r.try_get("last_modified")?;
                items.push(BlobInfo::new(
                    self.shared.logical(&key).to_string(),
                    u64::try_from(size).unwrap_or(0),
                    r.try_get("content_type")?,
                    r.try_get("etag")?,
                    last_modified.into(),
                    metadata.0,
                ));
            }
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

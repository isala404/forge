//! Filesystem `blob` backend. Contract: docs/contracts/blob.md.
//!
//! Object metadata lives in Postgres (`forge_fs_blobs`); the object *bytes* live on a
//! configured local directory. This keeps large objects out of the WAL (smaller
//! backups, less replication/vacuum pressure) at two documented costs: `put` is no
//! longer atomic with surrounding app SQL (the file is written outside the DB
//! transaction), and a multi-replica deployment needs a shared mount (or sticky
//! routing) because each replica resolves bytes from its own filesystem.
//!
//! Crash windows are bounded and self-healing: the file is written *before* the
//! metadata row commits, so a crash in between leaves an orphan file (reclaimed by the
//! maintenance sweep), never a row pointing at a missing file. A row that does point at
//! a missing file is treated as not-found. Presign/verify and key checks are shared
//! with the Postgres backend via [`super::common`], so signing is identical.

use super::common;
use super::sign::Method;
use super::{Blob, BlobInfo, DEFAULT_CONTENT_TYPE, ListPage, MAX_OBJECT_BYTES, PutOpts};
use crate::error::{ForgeError, Result};
use crate::obs;
use crate::types::Cursor;
use crate::util::{key_hash, sha256_hex};
use async_trait::async_trait;
use bytes::Bytes;
use sqlx::PgPool;
use sqlx::types::Json;
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
    namespace: String,
    secret: Option<Vec<u8>>,
    base_url: String,
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
            namespace,
            secret,
            base_url,
            root,
        })
    }

    fn physical(&self, key: &str) -> String {
        common::physical(&self.namespace, key)
    }

    fn logical<'a>(&self, stored: &'a str) -> &'a str {
        common::logical(&self.namespace, stored)
    }

    fn build_presigned(
        &self,
        method: Method,
        key: &str,
        expires: Duration,
        max_bytes: u64,
    ) -> Result<String> {
        common::presign_url(
            self.secret.as_deref(),
            &self.base_url,
            method,
            key,
            expires,
            max_bytes,
        )
    }

    /// A fresh content path, sharded into a two-char prefix directory so one directory
    /// never holds the whole keyspace: `"ab/ab34…"`.
    fn new_ref() -> String {
        let id = Uuid::new_v4().simple().to_string();
        let prefix = id.get(..2).unwrap_or("00");
        format!("{prefix}/{id}")
    }

    /// Reclaim files on disk that no metadata row references (e.g. a `put` that wrote
    /// its file then crashed before committing, or an overwrite's superseded file).
    /// Only files older than [`ORPHAN_GRACE`] are removed, so an in-flight `put` is
    /// never swept. Returns how many files were removed. Registered on the maintenance
    /// hook.
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
            // Nothing written yet (or root vanished) — nothing to sweep.
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
            let pk = self.physical(key);
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

            // Commit metadata; capture any superseded file's ref to delete after commit.
            let mut tx = self.pool.begin().await?;
            let old_ref = sqlx::query_scalar!(
                "SELECT data_ref FROM forge_fs_blobs WHERE key = $1 FOR UPDATE",
                pk
            )
            .fetch_optional(&mut *tx)
            .await?;
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

            // Best-effort: drop the overwritten file. If this fails it becomes an
            // orphan and the sweep reclaims it.
            if let Some(old) = old_ref
                && old != data_ref
            {
                let _ = tokio::fs::remove_file(self.root.join(&old)).await;
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
            let pk = self.physical(key);
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
                // Row points at a missing file: treat as not-found per the contract.
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    s.record("blob.hit", false);
                    Ok(None)
                }
                Err(e) => Err(fs_err(e)),
            }
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
            let pk = self.physical(key);
            let row = sqlx::query!(
                r#"SELECT content_type, etag, size,
                          metadata AS "metadata: Json<Meta>", last_modified
                   FROM forge_fs_blobs WHERE key = $1"#,
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
            common::check_key(key)?;
            let pk = self.physical(key);
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

    async fn list(&self, prefix: &str, cursor: Option<Cursor>, limit: u32) -> Result<ListPage> {
        let physical_prefix = self.physical(prefix);
        let pattern = format!("{}%", common::like_escape(&physical_prefix));
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
                   FROM forge_fs_blobs
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
            common::check_key(key)?;
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
            common::check_key(key)?;
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
        common::verify_presigned(
            self.secret.as_deref(),
            method,
            key,
            expires_epoch,
            max_bytes,
            sig,
        )
    }

    fn presign_ready(&self) -> bool {
        self.secret.is_some()
    }
}

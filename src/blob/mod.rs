//! `blob` — lineage: AWS S3 API. See `docs/contracts/blob.md`.
//!
//! Object storage keyed by `/`-delimited path strings. Whole-body `put`/`get` (no
//! streaming/multipart in v1), `head`/`delete`/`list`, and HMAC-signed presigned
//! URLs that resolve through the optional `blob-router`.

use crate::error::Result;
use crate::types::Cursor;
use async_trait::async_trait;
use bytes::Bytes;
use std::collections::BTreeMap;
use std::time::{Duration, SystemTime};

mod sign;

#[cfg(feature = "postgres")]
mod pg;
#[cfg(feature = "postgres")]
pub(crate) use pg::PgBlob;

#[cfg(feature = "blob-router")]
pub mod router;

/// Largest object body accepted by `put` (50 MiB). Over => [`crate::ForgeError::Limit`].
pub const MAX_OBJECT_BYTES: usize = 50 * 1024 * 1024;
/// Largest object key, in encoded UTF-8 bytes. Over => [`crate::ForgeError::Limit`].
pub const MAX_KEY_BYTES: usize = 1024;
/// Largest `content_type`, in bytes. Over => [`crate::ForgeError::Limit`].
pub const MAX_CONTENT_TYPE_BYTES: usize = 256;
/// Largest user metadata, total of all keys + values, in bytes. Over => `Limit`.
pub const MAX_METADATA_BYTES: usize = 2048;
/// Largest presigned-URL lifetime (7 days, the S3 SigV4 ceiling). Over => `Limit`.
pub const MAX_PRESIGN_EXPIRES: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Default content type when `PutOpts.content_type` is unset.
pub const DEFAULT_CONTENT_TYPE: &str = "application/octet-stream";

/// Options for [`Blob::put`]. S3 `Content-Type` + `x-amz-meta-*`.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct PutOpts {
    /// Stored verbatim, echoed by `head`/download. Defaults to
    /// [`DEFAULT_CONTENT_TYPE`].
    pub content_type: Option<String>,
    /// Opaque user metadata, round-tripped on `head`.
    pub metadata: BTreeMap<String, String>,
}

impl PutOpts {
    /// Default options.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set the content type.
    pub fn with_content_type(mut self, ct: impl Into<String>) -> Self {
        self.content_type = Some(ct.into());
        self
    }

    /// Add one user-metadata entry.
    pub fn with_metadata(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.metadata.insert(k.into(), v.into());
        self
    }
}

/// Object metadata (no body), returned by [`Blob::head`] and within [`ListPage`].
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct BlobInfo {
    /// The object key (logical, namespace-stripped).
    pub key: String,
    /// Body length in bytes.
    pub size: u64,
    /// Stored content type.
    pub content_type: String,
    /// Content hash (hex). Changes iff the bytes change; not S3-MD5-shaped.
    pub etag: String,
    /// Last `put` commit time, seconds precision.
    pub last_modified: SystemTime,
    /// User metadata.
    pub metadata: BTreeMap<String, String>,
}

/// One page of [`Blob::list`] results, in lexicographic key order.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct ListPage {
    /// The objects on this page.
    pub items: Vec<BlobInfo>,
    /// Cursor for the next page, or `None` when iteration is complete.
    pub next: Option<Cursor>,
}

/// S3-shaped object storage. Object-safe; the facade hands out `Arc<dyn Blob>`.
///
/// Exact semantics, limits, presign scheme, and error mapping: `docs/contracts/blob.md`.
#[async_trait]
pub trait Blob: Send + Sync {
    /// `PutObject`. Buffered (≤ 50 MiB), last-write-wins. The new `etag` is read via
    /// [`Blob::head`].
    async fn put(&self, key: &str, data: Bytes, opts: PutOpts) -> Result<()>;

    /// `GetObject`. `None` if the key is absent.
    async fn get(&self, key: &str) -> Result<Option<Bytes>>;

    /// `HeadObject`. Metadata only. `None` if absent.
    async fn head(&self, key: &str) -> Result<Option<BlobInfo>>;

    /// `DeleteObject`. `true` if an object was removed, `false` if already absent.
    async fn delete(&self, key: &str) -> Result<bool>;

    /// `ListObjectsV2`: up to `limit` objects with `prefix`, lexicographic order,
    /// cursor-paginated.
    async fn list(&self, prefix: &str, cursor: Option<Cursor>, limit: u32) -> Result<ListPage>;

    /// An HMAC-signed, single-key, time-bound, size-capped upload URL.
    async fn presign_upload(&self, key: &str, expires: Duration, max_bytes: u64) -> Result<String>;

    /// An HMAC-signed, single-key, time-bound download URL.
    async fn presign_download(&self, key: &str, expires: Duration) -> Result<String>;
}

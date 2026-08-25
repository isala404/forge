use crate::error::Result;
use crate::types::Cursor;
use async_trait::async_trait;
use bytes::Bytes;
use std::collections::BTreeMap;
use std::pin::Pin;
use std::time::{Duration, SystemTime};
use tokio::io::{AsyncRead, AsyncReadExt};

/// Largest object body accepted by `put` (50 MiB). Over => [`crate::error::ForgeError::Limit`].
pub const MAX_OBJECT_BYTES: usize = 50 * 1024 * 1024;
/// Largest object key, in encoded UTF-8 bytes. Over => [`crate::error::ForgeError::Limit`].
pub const MAX_KEY_BYTES: usize = 1024;
/// Largest `content_type`, in bytes. Over => [`crate::error::ForgeError::Limit`].
pub const MAX_CONTENT_TYPE_BYTES: usize = 256;
/// Largest user metadata, total of all keys + values, in bytes. Over => `Limit`.
pub const MAX_METADATA_BYTES: usize = 2048;
/// Largest cache-control or content-disposition value, in bytes.
pub const MAX_HTTP_METADATA_BYTES: usize = 1024;
/// S3 multipart part-number ceiling.
pub const MAX_MULTIPART_PARTS: u32 = 10_000;
/// Largest presigned-URL lifetime (7 days, the S3 SigV4 ceiling). Over => `Limit`.
pub const MAX_PRESIGN_EXPIRES: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// Default content type when `PutOpts.content_type` is unset.
pub const DEFAULT_CONTENT_TYPE: &str = "application/octet-stream";

/// A caller-owned asynchronous object stream. S3 reads and multipart writes use this
/// path so object size is not constrained by [`MAX_OBJECT_BYTES`].
pub type BlobReader = Pin<Box<dyn AsyncRead + Send>>;

/// Provider-neutral atomic write condition. The ETag is an opaque version token; callers
/// must pass it back unchanged and must not interpret it as an MD5 digest.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PutPrecondition {
    /// Write only when the key does not exist.
    CreateOnly,
    /// Write only when the current provider version equals this opaque ETag.
    MatchVersion(String),
}

/// Conditional-read result. A matching `if-none-match` is distinct from a missing key.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConditionalGet {
    Missing,
    NotModified { etag: String },
    Found { body: Bytes, etag: String },
}

/// Provider-managed encryption requested for an S3 write. Forge never accepts key material.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum S3Encryption {
    S3Managed,
    Kms { key_id: Option<String> },
}

/// Opaque server-mediated multipart upload handle.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultipartUpload {
    pub key: String,
    pub upload_id: String,
    pub precondition: Option<PutPrecondition>,
}

impl MultipartUpload {
    pub fn new(
        key: impl Into<String>,
        upload_id: impl Into<String>,
        precondition: Option<PutPrecondition>,
    ) -> Self {
        Self {
            key: key.into(),
            upload_id: upload_id.into(),
            precondition,
        }
    }
}

/// One uploaded multipart part. Pass these values back unchanged at completion.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MultipartPart {
    pub part_number: u32,
    pub etag: String,
    pub size: u64,
}

impl MultipartPart {
    pub fn new(part_number: u32, etag: impl Into<String>, size: u64) -> Self {
        Self {
            part_number,
            etag: etag.into(),
            size,
        }
    }
}

/// Options for [`Blob::put`]. S3 `Content-Type` + `x-amz-meta-*`.
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct PutOpts {
    /// Stored verbatim, echoed by `head`/download. Defaults to
    /// [`DEFAULT_CONTENT_TYPE`].
    pub content_type: Option<String>,
    /// Opaque user metadata, round-tripped on `head`.
    pub metadata: BTreeMap<String, String>,
    /// HTTP cache policy returned by downloads.
    pub cache_control: Option<String>,
    /// HTTP download disposition returned by downloads.
    pub content_disposition: Option<String>,
    /// Expected lowercase SHA-256 hex digest. The write fails before commit on mismatch.
    pub checksum_sha256: Option<String>,
    /// S3 provider-managed encryption headers. Unsupported backends fail `NOT_CONFIGURED`.
    pub s3_encryption: Option<S3Encryption>,
    /// Optional atomic write condition.
    pub precondition: Option<PutPrecondition>,
}

impl PutOpts {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_content_type(mut self, ct: impl Into<String>) -> Self {
        self.content_type = Some(ct.into());
        self
    }

    pub fn with_metadata(mut self, k: impl Into<String>, v: impl Into<String>) -> Self {
        self.metadata.insert(k.into(), v.into());
        self
    }

    pub fn with_cache_control(mut self, value: impl Into<String>) -> Self {
        self.cache_control = Some(value.into());
        self
    }

    pub fn with_content_disposition(mut self, value: impl Into<String>) -> Self {
        self.content_disposition = Some(value.into());
        self
    }

    pub fn with_checksum_sha256(mut self, value: impl Into<String>) -> Self {
        self.checksum_sha256 = Some(value.into());
        self
    }

    pub fn with_s3_encryption(mut self, value: S3Encryption) -> Self {
        self.s3_encryption = Some(value);
        self
    }

    pub fn create_only(mut self) -> Self {
        self.precondition = Some(PutPrecondition::CreateOnly);
        self
    }

    pub fn match_version(mut self, etag: impl Into<String>) -> Self {
        self.precondition = Some(PutPrecondition::MatchVersion(etag.into()));
        self
    }
}

/// Complete object metadata (no body), returned by [`Blob::head`].
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct BlobInfo {
    /// The object key (logical, namespace-stripped).
    pub key: String,
    /// Body length in bytes.
    pub size: u64,
    /// Stored content type.
    pub content_type: String,
    /// Opaque provider version token. It may be quoted and is not necessarily an MD5.
    pub etag: String,
    /// Last `put` commit time, seconds precision.
    pub last_modified: SystemTime,
    /// User metadata.
    pub metadata: BTreeMap<String, String>,
    pub cache_control: Option<String>,
    pub content_disposition: Option<String>,
    /// Lowercase SHA-256 hex digest, independent of the provider ETag.
    pub checksum_sha256: Option<String>,
    /// Provider-managed encryption label, when the backend reports one.
    pub server_side_encryption: Option<String>,
}

/// Lightweight object metadata returned by [`Blob::list`]. User metadata and content type
/// require [`Blob::head`], matching what S3 can return without one request per object.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct BlobSummary {
    pub key: String,
    pub size: u64,
    /// Opaque provider version token; never interpret this as an MD5.
    pub etag: String,
    pub last_modified: SystemTime,
}

impl BlobSummary {
    pub fn new(key: String, size: u64, etag: String, last_modified: SystemTime) -> Self {
        Self {
            key,
            size,
            etag,
            last_modified,
        }
    }
}

impl BlobInfo {
    /// Construct object metadata. For backend implementors; app code receives this from
    /// [`Blob::head`].
    pub fn new(
        key: String,
        size: u64,
        content_type: String,
        etag: String,
        last_modified: SystemTime,
        metadata: BTreeMap<String, String>,
    ) -> Self {
        Self {
            key,
            size,
            content_type,
            etag,
            last_modified,
            metadata,
            cache_control: None,
            content_disposition: None,
            checksum_sha256: None,
            server_side_encryption: None,
        }
    }

    /// Attach optional HTTP, integrity, and provider metadata. For backend implementors.
    pub fn with_storage_metadata(
        mut self,
        cache_control: Option<String>,
        content_disposition: Option<String>,
        checksum_sha256: Option<String>,
        server_side_encryption: Option<String>,
    ) -> Self {
        self.cache_control = cache_control;
        self.content_disposition = content_disposition;
        self.checksum_sha256 = checksum_sha256;
        self.server_side_encryption = server_side_encryption;
        self
    }
}

/// One page of [`Blob::list`] results, in lexicographic key order.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ListPage {
    /// The objects on this page.
    pub items: Vec<BlobSummary>,
    /// Cursor for the next page, or `None` when iteration is complete.
    pub next: Option<Cursor>,
}

impl ListPage {
    /// Construct a list page. For backend implementors; app code receives this from
    /// [`Blob::list`].
    pub fn new(items: Vec<BlobSummary>, next: Option<Cursor>) -> Self {
        Self { items, next }
    }
}

/// A Forge proxy-signed request. The proxy verifies the signature and can enforce
/// `max_bytes` before forwarding the request to the configured backend.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ProxyPresign {
    pub url: String,
    pub method: String,
    pub key: String,
    pub expires_epoch: i64,
    pub max_bytes: u64,
    pub signature: String,
    pub required_headers: BTreeMap<String, String>,
}

/// A provider-native presigned request. The URL is a bearer credential. Logs and traces
/// must omit its query string. Native PUT cannot portably enforce a maximum body size.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct NativePresign {
    pub url: String,
    pub method: String,
    pub expires_epoch: i64,
    pub required_headers: BTreeMap<String, String>,
    pub constraints: BTreeMap<String, String>,
}

/// S3-shaped object storage. Object-safe; the facade hands out `Arc<dyn Blob>`.
///
/// Exact semantics, limits, presign scheme, and error mapping: <https://tryforge.dev/primitives/#blob>.
#[async_trait]
pub trait Blob: Send + Sync {
    /// `PutObject`. Buffered (≤ 50 MiB), last-write-wins. The new `etag` is read via
    /// [`Blob::head`].
    async fn put(&self, key: &str, data: Bytes, opts: PutOpts) -> Result<()>;

    /// Stream an object of exactly `content_length` bytes. S3 uses multipart upload for
    /// large inputs and aborts the upload if reading or uploading a part fails.
    async fn put_stream(
        &self,
        key: &str,
        reader: BlobReader,
        content_length: u64,
        opts: PutOpts,
    ) -> Result<()> {
        if content_length > MAX_OBJECT_BYTES as u64 {
            return Err(crate::error::ForgeError::limit(
                "this backend only supports streaming objects up to the 50 MiB buffered limit",
            ));
        }
        let capacity = usize::try_from(content_length)
            .map_err(|_| crate::error::ForgeError::limit("object length exceeds this platform"))?;
        let mut body = Vec::with_capacity(capacity);
        reader
            .take(content_length.saturating_add(1))
            .read_to_end(&mut body)
            .await
            .map_err(|error| {
                crate::error::ForgeError::backend_with(
                    "could not read blob input stream",
                    false,
                    error,
                )
            })?;
        if body.len() as u64 != content_length {
            return Err(crate::error::ForgeError::invalid(
                "blob stream length does not match content_length",
            ));
        }
        self.put(key, Bytes::from(body), opts).await
    }

    /// `GetObject`. `None` if the key is absent.
    async fn get(&self, key: &str) -> Result<Option<Bytes>>;

    /// Atomically apply ETag conditions to a buffered read. Supplying both conditions is invalid.
    async fn get_if(
        &self,
        key: &str,
        if_match: Option<&str>,
        if_none_match: Option<&str>,
    ) -> Result<ConditionalGet> {
        crate::blob::common::check_get_conditions(if_match, if_none_match)?;
        let Some(info) = self.head(key).await? else {
            return Ok(ConditionalGet::Missing);
        };
        if if_match.is_some_and(|expected| expected != info.etag) {
            return Err(crate::ForgeError::precondition(
                "blob read version does not match",
            ));
        }
        if if_none_match.is_some_and(|version| version == info.etag) {
            return Ok(ConditionalGet::NotModified { etag: info.etag });
        }
        let body = self.get(key).await?.ok_or_else(|| {
            crate::ForgeError::precondition("blob changed during conditional read")
        })?;
        Ok(ConditionalGet::Found {
            body,
            etag: info.etag,
        })
    }

    /// Open an object without buffering the full body. `None` if absent.
    async fn open(&self, key: &str) -> Result<Option<BlobReader>> {
        Ok(self
            .get(key)
            .await?
            .map(|body| Box::pin(std::io::Cursor::new(body)) as BlobReader))
    }

    /// Read an inclusive byte range. `None` if absent.
    async fn get_range(&self, key: &str, start: u64, end: u64) -> Result<Option<Bytes>> {
        if end < start {
            return Err(crate::error::ForgeError::invalid(
                "range end must be greater than or equal to start",
            ));
        }
        let Some(body) = self.get(key).await? else {
            return Ok(None);
        };
        if start >= body.len() as u64 {
            return Err(crate::error::ForgeError::precondition(
                "range starts beyond the object",
            ));
        }
        let last = end.min(body.len() as u64 - 1);
        let start = usize::try_from(start)
            .map_err(|_| crate::error::ForgeError::limit("range exceeds this platform"))?;
        let end = usize::try_from(last)
            .map_err(|_| crate::error::ForgeError::limit("range exceeds this platform"))?;
        Ok(Some(body.slice(start..=end)))
    }

    /// `HeadObject`. Metadata only. `None` if absent.
    async fn head(&self, key: &str) -> Result<Option<BlobInfo>>;

    /// Idempotent `DeleteObject`. Success does not imply that an object existed.
    async fn delete(&self, key: &str) -> Result<()>;

    /// `ListObjectsV2`: up to `limit` objects with `prefix`, lexicographic order,
    /// cursor-paginated. Cursors are opaque and backend-specific. A page is ordered by
    /// UTF-8 key bytes, but listing is not a snapshot: concurrent writes may appear or be
    /// omitted. List results intentionally exclude user metadata and content type.
    async fn list(&self, prefix: &str, cursor: Option<Cursor>, limit: u32) -> Result<ListPage>;

    /// Copy without deleting the source. This is deliberately non-atomic across keys.
    async fn copy(&self, source: &str, destination: &str, mut opts: PutOpts) -> Result<BlobInfo> {
        let source_info = self
            .head(source)
            .await?
            .ok_or(crate::ForgeError::NotFound)?;
        let reader = self
            .open(source)
            .await?
            .ok_or(crate::ForgeError::NotFound)?;
        if opts.content_type.is_none() {
            opts.content_type = Some(source_info.content_type);
        }
        if opts.metadata.is_empty() {
            opts.metadata = source_info.metadata;
        }
        if opts.cache_control.is_none() {
            opts.cache_control = source_info.cache_control;
        }
        if opts.content_disposition.is_none() {
            opts.content_disposition = source_info.content_disposition;
        }
        self.put_stream(destination, reader, source_info.size, opts)
            .await?;
        self.head(destination)
            .await?
            .ok_or_else(|| crate::ForgeError::backend("copied blob is not readable"))
    }

    /// Start a provider-native multipart upload. Currently available only for S3.
    async fn create_multipart(&self, _key: &str, _opts: PutOpts) -> Result<MultipartUpload> {
        Err(crate::ForgeError::not_configured(
            "multipart handles require the S3 blob backend",
        ))
    }

    /// Upload or replace one numbered multipart part.
    async fn upload_part(
        &self,
        _upload: &MultipartUpload,
        _part_number: u32,
        _body: Bytes,
    ) -> Result<MultipartPart> {
        Err(crate::ForgeError::not_configured(
            "multipart handles require the S3 blob backend",
        ))
    }

    /// Complete a multipart upload from the exact ordered part receipts.
    async fn complete_multipart(
        &self,
        _upload: &MultipartUpload,
        _parts: Vec<MultipartPart>,
    ) -> Result<BlobInfo> {
        Err(crate::ForgeError::not_configured(
            "multipart handles require the S3 blob backend",
        ))
    }

    /// Idempotently abort a multipart upload.
    async fn abort_multipart(&self, _upload: &MultipartUpload) -> Result<()> {
        Err(crate::ForgeError::not_configured(
            "multipart handles require the S3 blob backend",
        ))
    }

    /// Stream and verify a SHA-256 digest independently of provider ETags.
    async fn verify_checksum_sha256(&self, key: &str, expected_hex: &str) -> Result<bool> {
        crate::blob::common::check_sha256(expected_hex)?;
        let Some(mut reader) = self.open(key).await? else {
            return Err(crate::ForgeError::NotFound);
        };
        use sha2::Digest as _;
        let mut hasher = sha2::Sha256::new();
        let mut buffer = vec![0u8; 64 * 1024];
        loop {
            let read = reader.read(&mut buffer).await.map_err(|error| {
                crate::ForgeError::backend_with("could not read blob for checksum", false, error)
            })?;
            if read == 0 {
                break;
            }
            let chunk = buffer.get(..read).ok_or_else(|| {
                crate::ForgeError::backend("blob reader returned an invalid byte count")
            })?;
            hasher.update(chunk);
        }
        Ok(crate::util::hex(&hasher.finalize()) == expected_hex)
    }

    /// An HMAC-signed, single-key, time-bound, size-capped upload URL.
    async fn presign_upload(
        &self,
        key: &str,
        expires: Duration,
        max_bytes: u64,
    ) -> Result<ProxyPresign>;

    /// An HMAC-signed, single-key, time-bound download URL.
    async fn presign_download(&self, key: &str, expires: Duration) -> Result<ProxyPresign>;

    /// Native provider GET presign. Only available on backends that can mint one.
    async fn presign_native_get(&self, _key: &str, _expires: Duration) -> Result<NativePresign> {
        Err(crate::error::ForgeError::not_configured(
            "native presigning requires the S3 blob backend",
        ))
    }

    /// Native provider PUT presign. Required headers are signed and must be sent exactly.
    /// No portable upload-size ceiling is implied.
    async fn presign_native_put(
        &self,
        _key: &str,
        _expires: Duration,
        _opts: PutOpts,
    ) -> Result<NativePresign> {
        Err(crate::error::ForgeError::not_configured(
            "native presigning requires the S3 blob backend",
        ))
    }

    /// Verify the parameters of a presigned URL against the configured signing
    /// secret. Returns `Ok(true)` only if the signature matches *and* the URL has not
    /// expired; `Ok(false)` for a bad signature or an expired URL; `Err(Config)` if no
    /// signing secret is set, `Err(Invalid)` if `method` is not `GET`/`PUT`.
    ///
    /// Exposed so the host app (or a language binding) that serves the presigned URLs
    /// enforces it instead of trusting the key blindly. `expires_epoch`, `max_bytes`,
    /// and `sig` come straight off the URL's query params.
    async fn verify_presigned(
        &self,
        method: &str,
        key: &str,
        expires_epoch: i64,
        max_bytes: u64,
        sig: &str,
    ) -> Result<bool>;
}

pub(crate) mod common;
pub(crate) mod sign;

mod fs;
mod memory;
mod postgres;
mod s3;
pub(crate) use fs::FsBlob;
pub(crate) use memory::MemBlob;
pub(crate) use postgres::PgBlob;
pub(crate) use s3::S3Blob;

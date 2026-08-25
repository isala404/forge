use super::common;
use super::{
    Blob, BlobInfo, BlobSummary, ConditionalGet, DEFAULT_CONTENT_TYPE, ListPage, ProxyPresign,
    PutOpts, PutPrecondition,
};
use crate::backend::{BackendLifecycle, Primitive};
use crate::error::Result;
use crate::types::Cursor;
use crate::util::sha256_hex;
use async_trait::async_trait;
use bytes::Bytes;
use std::collections::{BTreeMap, HashMap};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// One stored object: bytes plus the metadata `head`/`list` echo back. `size` and `etag`
/// derive from `data`, the source of truth for both.
struct Object {
    data: Bytes,
    content_type: String,
    etag: String,
    last_modified: SystemTime,
    metadata: BTreeMap<String, String>,
    cache_control: Option<String>,
    content_disposition: Option<String>,
    checksum_sha256: String,
}

impl Object {
    /// Body-less metadata view for `head`/`list`. `key` is the logical
    /// (namespace-stripped) key the caller used.
    fn info(&self, key: String) -> BlobInfo {
        BlobInfo::new(
            key,
            u64::try_from(self.data.len()).unwrap_or(u64::MAX),
            self.content_type.clone(),
            self.etag.clone(),
            self.last_modified,
            self.metadata.clone(),
        )
        .with_storage_metadata(
            self.cache_control.clone(),
            self.content_disposition.clone(),
            Some(self.checksum_sha256.clone()),
            None,
        )
    }

    fn summary(&self, key: String) -> BlobSummary {
        BlobSummary::new(
            key,
            u64::try_from(self.data.len()).unwrap_or(u64::MAX),
            self.etag.clone(),
            self.last_modified,
        )
    }
}

/// In-memory [`Blob`]: bytes + metadata in a map, presigning via the shared HMAC scheme.
pub(crate) struct MemBlob {
    state: Mutex<HashMap<String, Object>>,
    /// Namespace + presign config, via the same helper the Postgres and filesystem
    /// backends use so key mapping and URL signing never diverge.
    shared: common::Shared,
}

impl MemBlob {
    pub(crate) fn new(namespace: String, secret: Option<Vec<u8>>, base_url: String) -> Self {
        Self {
            state: Mutex::new(HashMap::new()),
            shared: common::Shared::new(namespace, secret, base_url),
        }
    }

    /// Take the map lock, recovering the guard if a previous holder panicked. Critical
    /// sections are short and synchronous (no `await` across the lock), so a poisoned lock
    /// never reflects a half-updated invariant worth aborting for.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Object>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }
}

/// `put` commit time truncated to whole seconds, matching the seconds precision
/// documented for `BlobInfo::last_modified`.
fn commit_time() -> SystemTime {
    let now = SystemTime::now();
    now.duration_since(UNIX_EPOCH)
        .map(|d| UNIX_EPOCH + Duration::from_secs(d.as_secs()))
        .unwrap_or(now)
}

#[async_trait]
impl Blob for MemBlob {
    async fn put(&self, key: &str, data: Bytes, opts: PutOpts) -> Result<()> {
        common::check_key(key)?;
        common::check_put(&data, &opts)?;
        common::reject_s3_encryption(&opts)?;
        let pk = self.shared.physical(key);
        let etag = sha256_hex(&data);
        let checksum_sha256 = etag.clone();
        let content_type = opts
            .content_type
            .unwrap_or_else(|| DEFAULT_CONTENT_TYPE.to_string());
        let mut state = self.lock();
        match &opts.precondition {
            Some(PutPrecondition::CreateOnly) if state.contains_key(&pk) => {
                return Err(crate::error::ForgeError::precondition(
                    "blob already exists",
                ));
            }
            Some(PutPrecondition::MatchVersion(expected))
                if state.get(&pk).map(|object| &object.etag) != Some(expected) =>
            {
                return Err(crate::error::ForgeError::precondition(
                    "blob version does not match",
                ));
            }
            _ => {}
        }
        state.insert(
            pk,
            Object {
                data,
                content_type,
                etag,
                last_modified: commit_time(),
                metadata: opts.metadata,
                cache_control: opts.cache_control,
                content_disposition: opts.content_disposition,
                checksum_sha256,
            },
        );
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        common::check_key(key)?;
        let pk = self.shared.physical(key);
        Ok(self.lock().get(&pk).map(|o| o.data.clone()))
    }

    async fn get_if(
        &self,
        key: &str,
        if_match: Option<&str>,
        if_none_match: Option<&str>,
    ) -> Result<ConditionalGet> {
        common::check_key(key)?;
        common::check_get_conditions(if_match, if_none_match)?;
        let pk = self.shared.physical(key);
        let state = self.lock();
        let Some(object) = state.get(&pk) else {
            return Ok(ConditionalGet::Missing);
        };
        if if_match.is_some_and(|expected| expected != object.etag) {
            return Err(crate::ForgeError::precondition(
                "blob read version does not match",
            ));
        }
        if if_none_match.is_some_and(|version| version == object.etag) {
            return Ok(ConditionalGet::NotModified {
                etag: object.etag.clone(),
            });
        }
        Ok(ConditionalGet::Found {
            body: object.data.clone(),
            etag: object.etag.clone(),
        })
    }

    async fn head(&self, key: &str) -> Result<Option<BlobInfo>> {
        common::check_key(key)?;
        let pk = self.shared.physical(key);
        Ok(self.lock().get(&pk).map(|o| o.info(key.to_string())))
    }

    async fn delete(&self, key: &str) -> Result<()> {
        common::check_key(key)?;
        let pk = self.shared.physical(key);
        self.lock().remove(&pk);
        Ok(())
    }

    async fn list(&self, prefix: &str, cursor: Option<Cursor>, limit: u32) -> Result<ListPage> {
        let physical_prefix = self.shared.physical(prefix);
        let limit = limit.clamp(1, 1000) as usize;
        // Keyset pagination over the physical key, like the Postgres backend: the cursor
        // token is the last physical key returned; the next page starts after it.
        let after = cursor.map(|c| c.token().to_string());
        let state = self.lock();
        let mut matched: Vec<(String, BlobSummary)> = state
            .iter()
            .filter(|(k, _)| k.starts_with(physical_prefix.as_str()))
            .filter(|(k, _)| after.as_deref().is_none_or(|a| k.as_str() > a))
            .map(|(k, o)| (k.clone(), o.summary(self.shared.logical(k).to_string())))
            .collect();
        matched.sort_by(|a, b| a.0.cmp(&b.0));
        matched.truncate(limit.saturating_add(1));
        let next = (matched.len() > limit).then(|| {
            Cursor::from_token(
                matched
                    .get(limit - 1)
                    .map(|item| item.0.clone())
                    .unwrap_or_default(),
            )
        });
        matched.truncate(limit);
        let items = matched.into_iter().map(|(_, info)| info).collect();
        Ok(ListPage::new(items, next))
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

#[async_trait]
impl BackendLifecycle for MemBlob {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn primitive(&self) -> Primitive {
        Primitive::Blob
    }
    fn durable(&self) -> bool {
        false
    }
    fn caveats(&self) -> &'static str {
        "in-process, not durable"
    }
    // Blobs carry no TTL, so there is nothing to sweep: the no-op `maintain` default applies.
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use crate::S3Encryption;
    use crate::blob::MAX_CONTENT_TYPE_BYTES;
    use crate::error::ForgeError;

    fn b(s: &str) -> Bytes {
        Bytes::from(s.as_bytes().to_vec())
    }

    #[tokio::test]
    async fn put_then_get_and_head_roundtrip() {
        let blob = MemBlob::new(String::new(), None, "/files".to_string());
        let opts = PutOpts::new()
            .with_content_type("text/csv")
            .with_metadata("owner", "alice");
        blob.put("exports/data.csv", b("a,b,c"), opts)
            .await
            .unwrap();

        assert_eq!(
            blob.get("exports/data.csv").await.unwrap(),
            Some(b("a,b,c"))
        );
        let info = blob.head("exports/data.csv").await.unwrap().unwrap();
        assert_eq!(info.key, "exports/data.csv");
        assert_eq!(info.size, 5);
        assert_eq!(info.content_type, "text/csv");
        assert_eq!(info.etag, sha256_hex(b"a,b,c"), "etag is the content hash");
        assert_eq!(
            info.metadata.get("owner").map(String::as_str),
            Some("alice")
        );

        assert_eq!(blob.get("missing").await.unwrap(), None);
        assert!(blob.head("missing").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn head_defaults_content_type_when_unset() {
        let blob = MemBlob::new(String::new(), None, "/files".to_string());
        blob.put("k", b("x"), PutOpts::new()).await.unwrap();
        let info = blob.head("k").await.unwrap().unwrap();
        assert_eq!(info.content_type, DEFAULT_CONTENT_TYPE);
        assert!(info.metadata.is_empty());
    }

    #[tokio::test]
    async fn put_overwrites_last_write_wins() {
        let blob = MemBlob::new(String::new(), None, "/files".to_string());
        blob.put(
            "k",
            b("v1"),
            PutOpts::new()
                .with_content_type("text/plain")
                .with_metadata("a", "1"),
        )
        .await
        .unwrap();
        let first = blob.head("k").await.unwrap().unwrap();

        blob.put(
            "k",
            b("v2-longer"),
            PutOpts::new().with_content_type("application/json"),
        )
        .await
        .unwrap();
        let second = blob.head("k").await.unwrap().unwrap();

        assert_eq!(blob.get("k").await.unwrap(), Some(b("v2-longer")));
        assert_eq!(second.content_type, "application/json");
        assert!(
            second.metadata.is_empty(),
            "an overwrite replaces metadata wholesale, it does not merge"
        );
        assert_eq!(second.size, 9);
        assert_ne!(first.etag, second.etag, "etag tracks the bytes");
    }

    #[tokio::test]
    async fn conditional_reads_copy_headers_and_checksum_compose() {
        let blob = MemBlob::new(String::new(), None, "/files".to_string());
        let body = b("portable bytes");
        let checksum = sha256_hex(&body);
        blob.put(
            "source",
            body.clone(),
            PutOpts::new()
                .with_cache_control("public, max-age=60")
                .with_content_disposition("attachment; filename=report.txt")
                .with_checksum_sha256(checksum.clone()),
        )
        .await
        .unwrap();
        let info = blob.head("source").await.unwrap().unwrap();
        assert_eq!(info.cache_control.as_deref(), Some("public, max-age=60"));
        assert_eq!(info.checksum_sha256.as_deref(), Some(checksum.as_str()));
        assert!(
            blob.verify_checksum_sha256("source", &checksum)
                .await
                .unwrap()
        );

        assert!(matches!(
            blob.get_if("source", None, Some(&info.etag)).await.unwrap(),
            ConditionalGet::NotModified { .. }
        ));
        assert!(matches!(
            blob.get_if("source", Some("wrong"), None).await,
            Err(ForgeError::Precondition(_))
        ));
        let copied = blob
            .copy("source", "copy", PutOpts::new().create_only())
            .await
            .unwrap();
        assert_eq!(blob.get("copy").await.unwrap(), Some(body));
        assert_eq!(copied.cache_control, info.cache_control);
        assert!(matches!(
            blob.create_multipart("large", PutOpts::new()).await,
            Err(ForgeError::NotConfigured(_))
        ));
        assert!(matches!(
            blob.put(
                "encrypted",
                b("x"),
                PutOpts::new().with_s3_encryption(S3Encryption::S3Managed),
            )
            .await,
            Err(ForgeError::NotConfigured(_))
        ));
    }

    #[tokio::test]
    async fn delete_is_idempotent() {
        let blob = MemBlob::new(String::new(), None, "/files".to_string());
        blob.put("k", b("v"), PutOpts::new()).await.unwrap();
        blob.delete("k").await.unwrap();
        blob.delete("k").await.unwrap();
        assert_eq!(blob.get("k").await.unwrap(), None);
    }

    #[tokio::test]
    async fn rejects_empty_key_and_oversized_metadata() {
        let blob = MemBlob::new(String::new(), None, "/files".to_string());
        assert!(
            matches!(
                blob.put("", b("x"), PutOpts::new()).await,
                Err(ForgeError::Invalid(_))
            ),
            "empty key is Invalid"
        );
        let huge_ct = "x".repeat(MAX_CONTENT_TYPE_BYTES + 1);
        assert!(
            matches!(
                blob.put("k", b("v"), PutOpts::new().with_content_type(huge_ct))
                    .await,
                Err(ForgeError::Limit(_))
            ),
            "content type over the cap is Limit"
        );
    }

    #[tokio::test]
    async fn list_paginates_by_prefix() {
        let blob = MemBlob::new(String::new(), None, "/files".to_string());
        for i in 0..10 {
            blob.put(&format!("img/{i:02}.png"), b("x"), PutOpts::new())
                .await
                .unwrap();
        }
        blob.put("other/x", b("y"), PutOpts::new()).await.unwrap();

        let mut seen = Vec::new();
        let mut cursor = None;
        loop {
            let page = blob.list("img/", cursor, 3).await.unwrap();
            seen.extend(page.items.into_iter().map(|i| i.key));
            match page.next {
                Some(c) => cursor = Some(c),
                None => break,
            }
        }
        assert_eq!(seen.len(), 10, "exactly the 10 img/* keys");
        assert!(seen.is_sorted(), "list returns keys in lexicographic order");
        assert_eq!(seen.first().map(String::as_str), Some("img/00.png"));
        assert!(!seen.iter().any(|k| k.starts_with("other/")));
    }

    #[tokio::test]
    async fn namespace_prefixes_storage_but_exposes_logical_keys() {
        let blob = MemBlob::new("tenant_a".to_string(), None, "/files".to_string());
        blob.put("docs/a.txt", b("hi"), PutOpts::new())
            .await
            .unwrap();
        {
            // Reaching into the map proves the stored key carries the namespace prefix.
            let state = blob.lock();
            assert!(state.contains_key("tenant_a:docs/a.txt"));
        }
        let info = blob.head("docs/a.txt").await.unwrap().unwrap();
        assert_eq!(info.key, "docs/a.txt", "head exposes the logical key");
        let page = blob.list("", None, 100).await.unwrap();
        assert_eq!(
            page.items.first().map(|i| i.key.as_str()),
            Some("docs/a.txt"),
            "list strips the namespace back to the logical key"
        );
    }

    #[tokio::test]
    async fn namespaces_isolate_keys() {
        let a = MemBlob::new("app_a".to_string(), None, "/files".to_string());
        let b2 = MemBlob::new("app_b".to_string(), None, "/files".to_string());
        a.put("shared", b("from-a"), PutOpts::new()).await.unwrap();
        b2.put("shared", b("from-b"), PutOpts::new()).await.unwrap();
        assert_eq!(a.get("shared").await.unwrap(), Some(b("from-a")));
        assert_eq!(b2.get("shared").await.unwrap(), Some(b("from-b")));
    }

    #[tokio::test]
    async fn presign_roundtrips_and_requires_a_secret() {
        let unsigned = MemBlob::new(String::new(), None, "/files".to_string());
        // No signing secret => presign and verify are Config errors (a deployment problem).
        assert!(matches!(
            unsigned
                .presign_download("k", Duration::from_secs(60))
                .await,
            Err(ForgeError::Config(_))
        ));
        assert!(matches!(
            unsigned
                .verify_presigned("GET", "k", 0, 0, "deadbeef")
                .await,
            Err(ForgeError::Config(_))
        ));

        let blob = MemBlob::new(
            String::new(),
            Some(b"sign-key".to_vec()),
            "/files".to_string(),
        );
        let url = blob
            .presign_download("docs/report.pdf", Duration::from_secs(300))
            .await
            .unwrap();
        let expires = url.expires_epoch;
        let sig = url.signature;

        assert!(
            blob.verify_presigned("GET", "docs/report.pdf", expires, 0, &sig)
                .await
                .unwrap(),
            "a freshly minted download URL verifies"
        );
        assert!(
            !blob
                .verify_presigned("GET", "docs/other.pdf", expires, 0, &sig)
                .await
                .unwrap(),
            "tampering with the key invalidates the signature"
        );
        assert!(
            !blob
                .verify_presigned("PUT", "docs/report.pdf", expires, 0, &sig)
                .await
                .unwrap(),
            "a GET URL cannot be replayed as a PUT"
        );

        let other_namespace = MemBlob::new(
            "other-app".to_string(),
            Some(b"sign-key".to_vec()),
            "/files".to_string(),
        );
        assert!(
            !other_namespace
                .verify_presigned("GET", "docs/report.pdf", expires, 0, &sig)
                .await
                .unwrap(),
            "a URL cannot be replayed into another application namespace"
        );
    }

    #[tokio::test]
    async fn presign_upload_signs_the_size_cap() {
        let blob = MemBlob::new(
            String::new(),
            Some(b"sign-key".to_vec()),
            "/files".to_string(),
        );
        let url = blob
            .presign_upload("uploads/x.bin", Duration::from_secs(120), 4096)
            .await
            .unwrap();
        assert!(url.url.contains("max_bytes=4096"));
        let expires = url.expires_epoch;
        let sig = url.signature;

        assert!(
            blob.verify_presigned("PUT", "uploads/x.bin", expires, 4096, &sig)
                .await
                .unwrap()
        );
        assert!(
            !blob
                .verify_presigned("PUT", "uploads/x.bin", expires, 8192, &sig)
                .await
                .unwrap(),
            "a size cap other than the one signed must fail"
        );
    }
}

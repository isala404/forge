//! In-process `blob` backend. Contract: docs/contracts/blob.md.
//!
//! Bytes and metadata live in a `Mutex<HashMap>` keyed by the same `<namespace>:<key>`
//! physical key the Postgres backend uses, so namespacing and the HMAC presign/verify
//! scheme (shared via [`super::common`]) stay identical. Observable behavior matches
//! [`super::PgBlob`]; only storage differs, and nothing survives a restart.

use super::common;
use super::{Blob, BlobInfo, DEFAULT_CONTENT_TYPE, ListPage, PutOpts};
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
        let pk = self.shared.physical(key);
        let etag = sha256_hex(&data);
        let content_type = opts
            .content_type
            .unwrap_or_else(|| DEFAULT_CONTENT_TYPE.to_string());
        // Last-write-wins: a fresh object replaces all prior bytes and metadata, like the
        // Postgres `ON CONFLICT DO UPDATE SET ...` overwrite.
        self.lock().insert(
            pk,
            Object {
                data,
                content_type,
                etag,
                last_modified: commit_time(),
                metadata: opts.metadata,
            },
        );
        Ok(())
    }

    async fn get(&self, key: &str) -> Result<Option<Bytes>> {
        common::check_key(key)?;
        let pk = self.shared.physical(key);
        Ok(self.lock().get(&pk).map(|o| o.data.clone()))
    }

    async fn head(&self, key: &str) -> Result<Option<BlobInfo>> {
        common::check_key(key)?;
        let pk = self.shared.physical(key);
        Ok(self.lock().get(&pk).map(|o| o.info(key.to_string())))
    }

    async fn delete(&self, key: &str) -> Result<bool> {
        common::check_key(key)?;
        let pk = self.shared.physical(key);
        Ok(self.lock().remove(&pk).is_some())
    }

    async fn list(&self, prefix: &str, cursor: Option<Cursor>, limit: u32) -> Result<ListPage> {
        let physical_prefix = self.shared.physical(prefix);
        let limit = limit.clamp(1, 1000) as usize;
        // Keyset pagination over the physical key, like the Postgres backend: the cursor
        // token is the last physical key returned; the next page starts after it.
        let after = cursor.map(|c| c.token().to_string());
        let state = self.lock();
        let mut matched: Vec<(String, BlobInfo)> = state
            .iter()
            .filter(|(k, _)| k.starts_with(physical_prefix.as_str()))
            .filter(|(k, _)| after.as_deref().is_none_or(|a| k.as_str() > a))
            .map(|(k, o)| (k.clone(), o.info(self.shared.logical(k).to_string())))
            .collect();
        matched.sort_by(|a, b| a.0.cmp(&b.0));
        matched.truncate(limit);
        let next = if matched.len() < limit {
            None
        } else {
            matched.last().map(|(k, _)| Cursor::from_token(k.clone()))
        };
        let items = matched.into_iter().map(|(_, info)| info).collect();
        Ok(ListPage::new(items, next))
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
    use crate::blob::MAX_CONTENT_TYPE_BYTES;
    use crate::error::ForgeError;

    fn b(s: &str) -> Bytes {
        Bytes::from(s.as_bytes().to_vec())
    }

    /// Pull a query-param value off a presigned URL. Test-only: the exact key is known,
    /// so this never has to percent-decode.
    fn param(url: &str, name: &str) -> String {
        let needle = format!("{name}=");
        url.split(&['?', '&'][..])
            .find_map(|kv| kv.strip_prefix(needle.as_str()))
            .map(str::to_string)
            .unwrap()
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
    async fn delete_reports_presence() {
        let blob = MemBlob::new(String::new(), None, "/files".to_string());
        blob.put("k", b("v"), PutOpts::new()).await.unwrap();
        assert!(blob.delete("k").await.unwrap(), "present => removed");
        assert!(!blob.delete("k").await.unwrap(), "already absent => false");
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
        let expires: i64 = param(&url, "expires").parse().unwrap();
        let sig = param(&url, "sig");

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
        assert!(url.contains("max_bytes=4096"));
        let expires: i64 = param(&url, "expires").parse().unwrap();
        let sig = param(&url, "sig");

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

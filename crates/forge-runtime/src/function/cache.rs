use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use serde_json::Value;
use sha2::{Digest, Sha256};

/// `Hasher` adapter that funnels writes into a SHA-256 digest. Used to keep
/// cache keys deterministic across rolling deploys; the standard library
/// `DefaultHasher` is explicitly not stable across Rust versions.
struct Sha256Hasher(Sha256);

impl Sha256Hasher {
    fn new() -> Self {
        Self(Sha256::new())
    }

    fn finish_u64(self) -> u64 {
        let digest = self.0.finalize();
        let mut buf = [0u8; 8];
        // SHA-256 always yields 32 bytes; `get` keeps clippy::indexing_slicing happy.
        if let Some(prefix) = digest.get(..8) {
            buf.copy_from_slice(prefix);
        }
        u64::from_be_bytes(buf)
    }
}

impl Hasher for Sha256Hasher {
    fn write(&mut self, bytes: &[u8]) {
        self.0.update(bytes);
    }

    fn finish(&self) -> u64 {
        // `Hasher::finish` requires a non-consuming signature, but SHA-256 is
        // not free to clone. Cache callers use `finish_u64` after dropping the
        // hasher; this fallback exists only to satisfy the trait.
        let digest = self.0.clone().finalize();
        let mut buf = [0u8; 8];
        // SHA-256 always yields 32 bytes, but `get` keeps clippy happy.
        if let Some(prefix) = digest.get(..8) {
            buf.copy_from_slice(prefix);
        }
        u64::from_be_bytes(buf)
    }
}

/// A simple in-memory cache for query results.
pub struct QueryCache {
    entries: RwLock<HashMap<CacheKey, CacheEntry>>,
    max_entries: usize,
}

#[derive(Clone, Eq, PartialEq, Hash)]
struct CacheKey {
    function_name: String,
    args_hash: u64,
    auth_scope_hash: u64,
}

struct CacheEntry {
    value: Arc<Value>,
    expires_at: Instant,
    created_at: Instant,
}

impl QueryCache {
    /// Create a new query cache with default settings.
    pub fn new() -> Self {
        Self::with_max_entries(10_000)
    }

    /// Create a new query cache with a maximum number of entries.
    pub fn with_max_entries(max_entries: usize) -> Self {
        Self {
            entries: RwLock::new(HashMap::new()),
            max_entries,
        }
    }

    /// Get a cached value if it exists and hasn't expired.
    pub fn get(
        &self,
        function_name: &str,
        args: &Value,
        auth_scope: Option<&str>,
    ) -> Option<Arc<Value>> {
        let key = self.make_key(function_name, args, auth_scope);

        let entries = self.entries.read().ok()?;
        let entry = entries.get(&key)?;

        if Instant::now() < entry.expires_at {
            Some(Arc::clone(&entry.value))
        } else {
            None
        }
    }

    /// Set a cached value with a TTL.
    pub fn set(
        &self,
        function_name: &str,
        args: &Value,
        auth_scope: Option<&str>,
        value: Value,
        ttl: Duration,
    ) {
        let key = self.make_key(function_name, args, auth_scope);
        let now = Instant::now();

        let entry = CacheEntry {
            value: Arc::new(value),
            expires_at: now + ttl,
            created_at: now,
        };

        if let Ok(mut entries) = self.entries.write() {
            // Evict expired entries if we're at capacity
            if entries.len() >= self.max_entries {
                self.evict_expired(&mut entries);
            }

            // If still at capacity, evict oldest entries
            if entries.len() >= self.max_entries {
                self.evict_oldest(&mut entries, (self.max_entries / 10).max(1));
            }

            entries.insert(key, entry);
        }
    }

    /// Invalidate a specific cache entry.
    pub fn invalidate(&self, function_name: &str, args: &Value) {
        let key = self.make_key(function_name, args, None);
        if let Ok(mut entries) = self.entries.write() {
            entries.retain(|k, _| {
                !(k.function_name == key.function_name && k.args_hash == key.args_hash)
            });
        }
    }

    /// Invalidate all entries for a function.
    pub fn invalidate_function(&self, function_name: &str) {
        if let Ok(mut entries) = self.entries.write() {
            entries.retain(|k, _| k.function_name != function_name);
        }
    }

    /// Invalidate all cached queries that depend on any of the given tables.
    pub fn invalidate_by_tables(&self, query_names: &[&str]) {
        if query_names.is_empty() {
            return;
        }
        if let Ok(mut entries) = self.entries.write() {
            entries.retain(|k, _| !query_names.iter().any(|name| k.function_name == *name));
        }
    }

    /// Clear the entire cache.
    pub fn clear(&self) {
        if let Ok(mut entries) = self.entries.write() {
            entries.clear();
        }
    }

    /// Get the number of cached entries.
    pub fn len(&self) -> usize {
        self.entries.read().map(|e| e.len()).unwrap_or(0)
    }

    /// Check if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn make_key(&self, function_name: &str, args: &Value, auth_scope: Option<&str>) -> CacheKey {
        CacheKey {
            function_name: function_name.to_string(),
            args_hash: hash_value(args),
            auth_scope_hash: hash_str(auth_scope.unwrap_or("")),
        }
    }

    fn evict_expired(&self, entries: &mut HashMap<CacheKey, CacheEntry>) {
        let now = Instant::now();
        entries.retain(|_, v| v.expires_at > now);
    }

    fn evict_oldest(&self, entries: &mut HashMap<CacheKey, CacheEntry>, count: usize) {
        let mut oldest: Vec<_> = entries
            .iter()
            .map(|(k, v)| (k.clone(), v.created_at))
            .collect();

        oldest.sort_by_key(|(_, t)| *t);

        for (key, _) in oldest.into_iter().take(count) {
            entries.remove(&key);
        }
    }
}

impl Default for QueryCache {
    fn default() -> Self {
        Self::new()
    }
}

fn hash_value(value: &Value) -> u64 {
    let mut hasher = Sha256Hasher::new();
    hash_value_recursive(value, &mut hasher);
    hasher.finish_u64()
}

fn hash_str(value: &str) -> u64 {
    let mut hasher = Sha256Hasher::new();
    value.hash(&mut hasher);
    hasher.finish_u64()
}

fn hash_value_recursive<H: Hasher>(value: &Value, hasher: &mut H) {
    match value {
        Value::Null => 0u8.hash(hasher),
        Value::Bool(b) => {
            1u8.hash(hasher);
            b.hash(hasher);
        }
        Value::Number(n) => {
            2u8.hash(hasher);
            n.to_string().hash(hasher);
        }
        Value::String(s) => {
            3u8.hash(hasher);
            s.hash(hasher);
        }
        Value::Array(arr) => {
            4u8.hash(hasher);
            arr.len().hash(hasher);
            for v in arr {
                hash_value_recursive(v, hasher);
            }
        }
        Value::Object(obj) => {
            5u8.hash(hasher);
            obj.len().hash(hasher);
            // Sort keys for consistent hashing
            let mut keys: Vec<_> = obj.keys().collect();
            keys.sort();
            for key in keys {
                key.hash(hasher);
                hash_value_recursive(&obj[key], hasher);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn test_cache_set_get() {
        let cache = QueryCache::new();
        let args = json!({"id": 123});
        let value = json!({"name": "test"});

        cache.set(
            "get_user",
            &args,
            Some("user:1"),
            value.clone(),
            Duration::from_secs(60),
        );

        let result = cache.get("get_user", &args, Some("user:1"));
        assert_eq!(result.as_deref(), Some(&value));
    }

    #[test]
    fn test_cache_miss() {
        let cache = QueryCache::new();
        let args = json!({"id": 123});

        let result = cache.get("get_user", &args, Some("user:1"));
        assert_eq!(result, None);
    }

    #[test]
    fn test_cache_invalidate() {
        let cache = QueryCache::new();
        let args = json!({"id": 123});
        let value = json!({"name": "test"});

        cache.set(
            "get_user",
            &args,
            Some("user:1"),
            value,
            Duration::from_secs(60),
        );
        cache.invalidate("get_user", &args);

        let result = cache.get("get_user", &args, Some("user:1"));
        assert_eq!(result, None);
    }

    #[test]
    fn test_cache_invalidate_function() {
        let cache = QueryCache::new();
        let args1 = json!({"id": 1});
        let args2 = json!({"id": 2});

        cache.set(
            "get_user",
            &args1,
            Some("user:1"),
            json!({"name": "a"}),
            Duration::from_secs(60),
        );
        cache.set(
            "get_user",
            &args2,
            Some("user:1"),
            json!({"name": "b"}),
            Duration::from_secs(60),
        );
        cache.set(
            "list_users",
            &json!({}),
            Some("user:1"),
            json!([]),
            Duration::from_secs(60),
        );

        cache.invalidate_function("get_user");

        assert_eq!(cache.get("get_user", &args1, Some("user:1")), None);
        assert_eq!(cache.get("get_user", &args2, Some("user:1")), None);
        assert!(
            cache
                .get("list_users", &json!({}), Some("user:1"))
                .is_some()
        );
    }

    #[test]
    fn test_hash_consistency() {
        let v1 = json!({"a": 1, "b": 2});
        let v2 = json!({"b": 2, "a": 1});

        // Object keys should be sorted for consistent hashing
        assert_eq!(hash_value(&v1), hash_value(&v2));
    }

    #[test]
    fn test_cache_isolation_by_auth_scope() {
        let cache = QueryCache::new();
        let args = json!({"id": 1});

        cache.set(
            "get_profile",
            &args,
            Some("subject:user-a"),
            json!({"name": "Alice"}),
            Duration::from_secs(60),
        );

        assert!(
            cache
                .get("get_profile", &args, Some("subject:user-b"))
                .is_none()
        );
        assert!(
            cache
                .get("get_profile", &args, Some("subject:user-a"))
                .is_some()
        );
    }
}

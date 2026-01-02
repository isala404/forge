use std::collections::HashMap;
use std::hash::{Hash, Hasher};
use std::sync::RwLock;
use std::time::{Duration, Instant};

use serde_json::Value;

/// A simple in-memory cache for query results.
pub struct QueryCache {
    entries: RwLock<HashMap<CacheKey, CacheEntry>>,
    max_entries: usize,
}

#[derive(Clone, Eq, PartialEq, Hash)]
struct CacheKey {
    function_name: String,
    args_hash: u64,
}

struct CacheEntry {
    value: Value,
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
    pub fn get(&self, function_name: &str, args: &Value) -> Option<Value> {
        let key = self.make_key(function_name, args);

        let entries = self.entries.read().ok()?;
        let entry = entries.get(&key)?;

        if Instant::now() < entry.expires_at {
            Some(entry.value.clone())
        } else {
            None
        }
    }

    /// Set a cached value with a TTL.
    pub fn set(&self, function_name: &str, args: &Value, value: Value, ttl: Duration) {
        let key = self.make_key(function_name, args);
        let now = Instant::now();

        let entry = CacheEntry {
            value,
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
                self.evict_oldest(&mut entries, self.max_entries / 10);
            }

            entries.insert(key, entry);
        }
    }

    /// Invalidate a specific cache entry.
    pub fn invalidate(&self, function_name: &str, args: &Value) {
        let key = self.make_key(function_name, args);
        if let Ok(mut entries) = self.entries.write() {
            entries.remove(&key);
        }
    }

    /// Invalidate all entries for a function.
    pub fn invalidate_function(&self, function_name: &str) {
        if let Ok(mut entries) = self.entries.write() {
            entries.retain(|k, _| k.function_name != function_name);
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

    fn make_key(&self, function_name: &str, args: &Value) -> CacheKey {
        CacheKey {
            function_name: function_name.to_string(),
            args_hash: hash_value(args),
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
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    hash_value_recursive(value, &mut hasher);
    hasher.finish()
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

        cache.set("get_user", &args, value.clone(), Duration::from_secs(60));

        let result = cache.get("get_user", &args);
        assert_eq!(result, Some(value));
    }

    #[test]
    fn test_cache_miss() {
        let cache = QueryCache::new();
        let args = json!({"id": 123});

        let result = cache.get("get_user", &args);
        assert_eq!(result, None);
    }

    #[test]
    fn test_cache_invalidate() {
        let cache = QueryCache::new();
        let args = json!({"id": 123});
        let value = json!({"name": "test"});

        cache.set("get_user", &args, value, Duration::from_secs(60));
        cache.invalidate("get_user", &args);

        let result = cache.get("get_user", &args);
        assert_eq!(result, None);
    }

    #[test]
    fn test_cache_invalidate_function() {
        let cache = QueryCache::new();
        let args1 = json!({"id": 1});
        let args2 = json!({"id": 2});

        cache.set("get_user", &args1, json!({"name": "a"}), Duration::from_secs(60));
        cache.set("get_user", &args2, json!({"name": "b"}), Duration::from_secs(60));
        cache.set("list_users", &json!({}), json!([]), Duration::from_secs(60));

        cache.invalidate_function("get_user");

        assert_eq!(cache.get("get_user", &args1), None);
        assert_eq!(cache.get("get_user", &args2), None);
        assert!(cache.get("list_users", &json!({})).is_some());
    }

    #[test]
    fn test_hash_consistency() {
        let v1 = json!({"a": 1, "b": 2});
        let v2 = json!({"b": 2, "a": 1});

        // Object keys should be sorted for consistent hashing
        assert_eq!(hash_value(&v1), hash_value(&v2));
    }
}

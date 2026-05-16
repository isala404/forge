use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};

use forge_core::realtime::Change;
use forge_core::{AuthContext, FunctionInfo};
use serde_json::Value;
use sha2::{Digest, Sha256};
use tokio::sync::broadcast;

use super::registry::FunctionRegistry;

/// `Hasher` adapter that funnels writes into a SHA-256 digest. Used to keep
/// cache keys deterministic across rolling deploys; the standard library
/// `DefaultHasher` is explicitly not stable across Rust versions.
struct Sha256Hasher(Sha256);

impl Sha256Hasher {
    fn new() -> Self {
        Self(Sha256::new())
    }

    /// Consume the hasher and return a 64-bit cache key.
    ///
    /// Truncation to u64 is acceptable for cache key dedup — collision
    /// probability is ~1/2^64 per birthday bound which is negligible for
    /// cache sizes under billions of entries.
    fn finish_u64(self) -> u64 {
        let digest = self.0.finalize();
        let mut buf = [0u8; 8];
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
        // Required by the trait for `Hash::hash` to work. Clones internal
        // state because `Hasher::finish` is non-consuming. All real callers
        // use `finish_u64(self)` instead.
        let digest = self.0.clone().finalize();
        let mut buf = [0u8; 8];
        if let Some(prefix) = digest.get(..8) {
            buf.copy_from_slice(prefix);
        }
        u64::from_be_bytes(buf)
    }
}

/// A simple in-memory cache for query results.
pub struct QueryCache {
    entries: RwLock<CacheState>,
    max_entries: usize,
}

struct CacheState {
    map: HashMap<CacheKey, CacheEntry>,
    /// Insertion-order queue for O(1) eviction of oldest entries.
    insertion_order: VecDeque<CacheKey>,
    /// Reverse index: function name -> set of cache keys for that function.
    /// Avoids O(N) scan in `invalidate_by_tables` / `invalidate_function`.
    name_to_keys: HashMap<Arc<str>, HashSet<CacheKey>>,
}

/// Cache key using hashed function name to avoid per-lookup String allocation.
/// The `function_name_hash` is a stable SHA-256 truncation of the name.
#[derive(Clone, Copy, Eq, PartialEq, Hash)]
struct CacheKey {
    function_name_hash: u64,
    args_hash: u64,
    auth_scope_hash: u64,
}

struct CacheEntry {
    value: Arc<Value>,
    expires_at: Instant,
    /// The original function name, retained for invalidation-by-name lookups.
    function_name: Arc<str>,
}

impl QueryCache {
    /// Create a new query cache with default settings.
    pub fn new() -> Self {
        Self::with_max_entries(10_000)
    }

    /// Create a new query cache with a maximum number of entries.
    pub fn with_max_entries(max_entries: usize) -> Self {
        Self {
            entries: RwLock::new(CacheState {
                map: HashMap::new(),
                insertion_order: VecDeque::new(),
                name_to_keys: HashMap::new(),
            }),
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
        let key = Self::make_key(function_name, args, auth_scope);

        let state = self.entries.read().ok()?;
        let entry = state.map.get(&key)?;

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
        let key = Self::make_key(function_name, args, auth_scope);
        let now = Instant::now();

        let entry = CacheEntry {
            value: Arc::new(value),
            expires_at: now + ttl,
            function_name: Arc::from(function_name),
        };

        if let Ok(mut state) = self.entries.write() {
            // Evict expired entries if we're at capacity
            if state.map.len() >= self.max_entries {
                Self::evict_expired(&mut state);
            }

            // If still at capacity, evict oldest entries via insertion order
            if state.map.len() >= self.max_entries {
                Self::evict_oldest(&mut state, (self.max_entries / 10).max(1));
            }

            if !state.map.contains_key(&key) {
                state.insertion_order.push_back(key);
            }
            state
                .name_to_keys
                .entry(Arc::clone(&entry.function_name))
                .or_default()
                .insert(key);
            state.map.insert(key, entry);
        }
    }

    /// Invalidate a specific cache entry.
    pub fn invalidate(&self, function_name: &str, args: &Value) {
        let name_hash = hash_str(function_name);
        let args_hash = hash_value(args);
        if let Ok(mut state) = self.entries.write() {
            let matching: Vec<CacheKey> = state
                .map
                .keys()
                .filter(|k| k.function_name_hash == name_hash && k.args_hash == args_hash)
                .copied()
                .collect();
            for key in &matching {
                if let Some(entry) = state.map.remove(key)
                    && let Some(keys) = state.name_to_keys.get_mut(&entry.function_name)
                {
                    keys.remove(key);
                    if keys.is_empty() {
                        state.name_to_keys.remove(&entry.function_name);
                    }
                }
            }
            if !matching.is_empty() {
                let removed: HashSet<&CacheKey> = matching.iter().collect();
                state.insertion_order.retain(|k| !removed.contains(k));
            }
        }
    }

    /// Invalidate all entries for a function.
    pub fn invalidate_function(&self, function_name: &str) {
        if let Ok(mut state) = self.entries.write() {
            let name_arc: Arc<str> = Arc::from(function_name);
            if let Some(keys) = state.name_to_keys.remove(&name_arc) {
                for key in &keys {
                    state.map.remove(key);
                }
                state.insertion_order.retain(|k| !keys.contains(k));
            }
        }
    }

    /// Invalidate all cached queries that depend on any of the given tables.
    pub fn invalidate_by_tables(&self, query_names: &[&str]) {
        if query_names.is_empty() {
            return;
        }
        if let Ok(mut state) = self.entries.write() {
            let mut all_removed_keys: HashSet<CacheKey> = HashSet::new();
            for name in query_names {
                let name_arc: Arc<str> = Arc::from(*name);
                if let Some(keys) = state.name_to_keys.remove(&name_arc) {
                    for key in &keys {
                        state.map.remove(key);
                    }
                    all_removed_keys.extend(keys);
                }
            }
            if !all_removed_keys.is_empty() {
                state
                    .insertion_order
                    .retain(|k| !all_removed_keys.contains(k));
            }
        }
    }

    /// Clear the entire cache.
    pub fn clear(&self) {
        if let Ok(mut state) = self.entries.write() {
            state.map.clear();
            state.insertion_order.clear();
            state.name_to_keys.clear();
        }
    }

    /// Get the number of cached entries.
    pub fn len(&self) -> usize {
        self.entries.read().map(|e| e.map.len()).unwrap_or(0)
    }

    /// Check if the cache is empty.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    fn make_key(function_name: &str, args: &Value, auth_scope: Option<&str>) -> CacheKey {
        CacheKey {
            function_name_hash: hash_str(function_name),
            args_hash: hash_value(args),
            auth_scope_hash: hash_str(auth_scope.unwrap_or("")),
        }
    }

    fn evict_expired(state: &mut CacheState) {
        let now = Instant::now();
        let expired_keys: Vec<CacheKey> = state
            .map
            .iter()
            .filter(|(_, v)| v.expires_at <= now)
            .map(|(k, _)| *k)
            .collect();
        for key in &expired_keys {
            if let Some(entry) = state.map.remove(key)
                && let Some(keys) = state.name_to_keys.get_mut(&entry.function_name)
            {
                keys.remove(key);
                if keys.is_empty() {
                    state.name_to_keys.remove(&entry.function_name);
                }
            }
        }
        if !expired_keys.is_empty() {
            let removed: HashSet<&CacheKey> = expired_keys.iter().collect();
            state.insertion_order.retain(|k| !removed.contains(k));
        }
    }

    /// Evict the oldest entries by popping from the front of the insertion queue.
    fn evict_oldest(state: &mut CacheState, count: usize) {
        let mut evicted = 0;
        while evicted < count {
            let Some(key) = state.insertion_order.pop_front() else {
                break;
            };
            if let Some(entry) = state.map.remove(&key) {
                if let Some(keys) = state.name_to_keys.get_mut(&entry.function_name) {
                    keys.remove(&key);
                    if keys.is_empty() {
                        state.name_to_keys.remove(&entry.function_name);
                    }
                }
                evicted += 1;
            }
        }
    }
}

impl Default for QueryCache {
    fn default() -> Self {
        Self::new()
    }
}

/// Coordinates query-result caching: owns the [`QueryCache`], the reverse
/// table -> query index used for mutation invalidation, and the auth-scope
/// derivation that keys cache entries per principal.
///
/// Splitting these out of [`super::router::FunctionRouter`] keeps the router
/// focused on dispatch and lets the cache concerns be tested in isolation.
pub struct QueryCacheCoordinator {
    cache: QueryCache,
    /// Reverse index: table name -> dependent queries with the columns each one
    /// reads. Carrying the column set here lets `invalidate_for_mutation` skip
    /// queries whose read set doesn't overlap the mutation's write set without
    /// re-traversing the registry.
    table_to_queries: HashMap<String, Vec<QueryDep>>,
}

/// A query that depends on a table, with the columns it reads from that table.
/// Empty `selected_columns` means "could read any column" — treated as a wildcard
/// so the query is always invalidated when the table is touched.
#[derive(Clone)]
struct QueryDep {
    name: String,
    selected_columns: HashSet<String>,
}

impl QueryCacheCoordinator {
    /// Create a coordinator from the function registry. The reverse index is
    /// built once at construction so mutation invalidation is a hash lookup.
    pub fn new(registry: &FunctionRegistry) -> Self {
        Self {
            cache: QueryCache::new(),
            table_to_queries: build_table_index(registry),
        }
    }

    /// Lookup a cached value with an already-derived scope. Use this when the
    /// caller has computed [`Self::auth_scope`] up front (e.g. before `auth` is
    /// moved into a context).
    pub fn get_by_scope(
        &self,
        function_name: &str,
        args: &Value,
        scope: Option<&str>,
    ) -> Option<Arc<Value>> {
        self.cache.get(function_name, args, scope)
    }

    /// Store a value with an already-derived scope.
    pub fn set_by_scope(
        &self,
        function_name: &str,
        args: &Value,
        scope: Option<&str>,
        value: Value,
        ttl: Duration,
    ) {
        self.cache.set(function_name, args, scope, value, ttl);
    }

    /// Invalidate cached queries whose table dependencies overlap with the
    /// mutation's write set, narrowed by column intersection when both sides
    /// know which columns they touch.
    ///
    /// Conservative on uncertainty: when either the mutation's `changed_columns`
    /// or the candidate query's `selected_columns` is empty (extractor couldn't
    /// determine columns from the SQL), the query is invalidated. False positives
    /// are cheap (re-execute a query); false negatives would serve stale data.
    pub fn invalidate_for_mutation(&self, info: &FunctionInfo) {
        if info.table_dependencies.is_empty() {
            return;
        }
        let mutation_cols: HashSet<&str> = info.changed_columns.iter().copied().collect();
        let mutation_cols_unknown = mutation_cols.is_empty();

        let mut affected: HashSet<&str> = HashSet::new();
        for table in info.table_dependencies {
            let Some(queries) = self.table_to_queries.get(*table) else {
                continue;
            };
            for dep in queries {
                if mutation_cols_unknown
                    || dep.selected_columns.is_empty()
                    || dep
                        .selected_columns
                        .iter()
                        .any(|c| mutation_cols.contains(c.as_str()))
                {
                    affected.insert(dep.name.as_str());
                }
            }
        }
        if !affected.is_empty() {
            let names: Vec<&str> = affected.into_iter().collect();
            self.cache.invalidate_by_tables(&names);
            tracing::trace!(
                mutation = info.name,
                invalidated_queries = ?names,
                "Cache invalidated after mutation"
            );
        }
    }

    /// Invalidate cached queries affected by a `forge_changes` NOTIFY event.
    ///
    /// Used to propagate mutations across cluster nodes: every node listening
    /// to `forge_changes` runs this for each peer-emitted change so its local
    /// cache stays consistent. Without this, mutations on node A leave stale
    /// cache entries on node B until TTL expires.
    ///
    /// Mirrors `invalidate_for_mutation`'s column-intersection logic but uses
    /// the change's runtime `changed_columns` rather than the mutation's
    /// compile-time set. An empty change column list means "could be any
    /// column" (older trigger payloads, INSERT/DELETE) — fall back to full
    /// invalidation for that table.
    pub fn invalidate_by_change(&self, change: &Change) {
        let Some(queries) = self.table_to_queries.get(&change.table) else {
            return;
        };
        let change_cols_unknown = change.changed_columns.is_empty();
        let change_cols: HashSet<&str> =
            change.changed_columns.iter().map(String::as_str).collect();

        let mut affected: Vec<&str> = Vec::new();
        for dep in queries {
            if change_cols_unknown
                || dep.selected_columns.is_empty()
                || dep
                    .selected_columns
                    .iter()
                    .any(|c| change_cols.contains(c.as_str()))
            {
                affected.push(dep.name.as_str());
            }
        }
        if !affected.is_empty() {
            self.cache.invalidate_by_tables(&affected);
            tracing::trace!(
                table = %change.table,
                invalidated_queries = ?affected,
                "Cache invalidated by cluster change"
            );
        }
    }

    /// Spawn a background task that drains a `Change` broadcast and evicts
    /// matching cache entries. Returns a handle the caller can abort on
    /// shutdown. The receiver typically comes from
    /// `Reactor::change_subscriber()`.
    pub fn spawn_cluster_invalidator(
        self: Arc<Self>,
        mut rx: broadcast::Receiver<Change>,
    ) -> tokio::task::JoinHandle<()> {
        tokio::spawn(async move {
            loop {
                match rx.recv().await {
                    Ok(change) => self.invalidate_by_change(&change),
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        // We dropped n change events; the local cache could
                        // be holding values that should have been evicted.
                        // Clear everything to recover correctness.
                        tracing::warn!(
                            dropped = n,
                            "Cache invalidator lagged; clearing local cache"
                        );
                        self.cache.clear();
                    }
                    Err(broadcast::error::RecvError::Closed) => {
                        tracing::debug!("Change channel closed; cache invalidator stopping");
                        break;
                    }
                }
            }
        })
    }

    /// Derive a stable cache scope from auth context. Anonymous callers share
    /// the `"anon"` scope; authenticated callers get a hash that mixes role +
    /// claims so cross-tenant cache bleed is impossible.
    ///
    /// Volatile JWT claims (`iat`, `nbf`, `exp`, `jti`, `auth_time`, `sid`,
    /// `nonce`) are excluded — they change on every token refresh for the
    /// same logical principal and would otherwise fragment the cache, killing
    /// hit rate for any system using refresh tokens.
    pub fn auth_scope(auth: &AuthContext) -> Option<String> {
        if !auth.is_authenticated() {
            return Some("anon".to_string());
        }

        let mut roles = auth.roles().to_vec();
        roles.sort();
        roles.dedup();

        let mut claims = BTreeMap::new();
        for (k, v) in auth.claims() {
            if is_volatile_claim(k) {
                continue;
            }
            claims.insert(k.clone(), v.clone());
        }

        let claims_json = match serde_json::to_string(&claims) {
            Ok(json) => json,
            Err(_) => {
                tracing::error!(
                    "BTreeMap<String, Value> serialization failed — cache scope degraded"
                );
                String::new()
            }
        };
        let mut buf = String::with_capacity(64 + claims_json.len());
        for role in &roles {
            buf.push_str(role);
            buf.push('\x1f');
        }
        buf.push('\x1e');
        buf.push_str(&claims_json);
        let scope = crate::stable_hash::stable_u64(buf.as_bytes());

        let principal = auth
            .principal_id()
            .unwrap_or_else(|| "authenticated".to_string());

        Some(format!("subject:{principal}:scope:{scope:016x}"))
    }
}

/// Standard JWT claims that vary across token refreshes for the same
/// principal and must not influence the per-principal cache scope.
fn is_volatile_claim(name: &str) -> bool {
    matches!(
        name,
        "iat" | "nbf" | "exp" | "jti" | "auth_time" | "sid" | "nonce"
    )
}

fn build_table_index(registry: &FunctionRegistry) -> HashMap<String, Vec<QueryDep>> {
    let mut index: HashMap<String, Vec<QueryDep>> = HashMap::new();
    for (name, info) in registry.queries() {
        let selected_columns: HashSet<String> = info
            .selected_columns
            .iter()
            .map(|c| (*c).to_string())
            .collect();
        for table in info.table_dependencies {
            index
                .entry((*table).to_string())
                .or_default()
                .push(QueryDep {
                    name: name.to_string(),
                    selected_columns: selected_columns.clone(),
                });
        }
    }
    index
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
            let mut keys: Vec<_> = obj.keys().collect();
            keys.sort();
            for key in keys {
                key.hash(hasher);
                if let Some(v) = obj.get(key.as_str()) {
                    hash_value_recursive(v, hasher);
                }
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
    fn test_auth_scope_stable_across_token_refresh() {
        let user_id = uuid::Uuid::new_v4();
        let mut claims_t1 = std::collections::HashMap::new();
        claims_t1.insert(
            "sub".to_string(),
            serde_json::Value::String(user_id.to_string()),
        );
        claims_t1.insert(
            "tenant_id".to_string(),
            serde_json::Value::String("acme".to_string()),
        );
        claims_t1.insert("iat".to_string(), serde_json::Value::from(1_700_000_000));
        claims_t1.insert("exp".to_string(), serde_json::Value::from(1_700_003_600));
        claims_t1.insert("nbf".to_string(), serde_json::Value::from(1_700_000_000));
        claims_t1.insert(
            "jti".to_string(),
            serde_json::Value::String("token-uuid-1".to_string()),
        );

        let mut claims_t2 = claims_t1.clone();
        claims_t2.insert("iat".to_string(), serde_json::Value::from(1_700_010_000));
        claims_t2.insert("exp".to_string(), serde_json::Value::from(1_700_013_600));
        claims_t2.insert("nbf".to_string(), serde_json::Value::from(1_700_010_000));
        claims_t2.insert(
            "jti".to_string(),
            serde_json::Value::String("token-uuid-2".to_string()),
        );

        let auth_t1 = AuthContext::authenticated(user_id, vec!["member".to_string()], claims_t1);
        let auth_t2 = AuthContext::authenticated(user_id, vec!["member".to_string()], claims_t2);

        assert_eq!(
            QueryCacheCoordinator::auth_scope(&auth_t1),
            QueryCacheCoordinator::auth_scope(&auth_t2),
            "Token refresh must not change cache scope for the same principal"
        );
    }

    #[test]
    fn test_auth_scope_differs_by_tenant() {
        let user_id = uuid::Uuid::new_v4();
        let mut claims_a = std::collections::HashMap::new();
        claims_a.insert(
            "sub".to_string(),
            serde_json::Value::String(user_id.to_string()),
        );
        claims_a.insert(
            "tenant_id".to_string(),
            serde_json::Value::String("tenant-a".to_string()),
        );

        let mut claims_b = std::collections::HashMap::new();
        claims_b.insert(
            "sub".to_string(),
            serde_json::Value::String(user_id.to_string()),
        );
        claims_b.insert(
            "tenant_id".to_string(),
            serde_json::Value::String("tenant-b".to_string()),
        );

        let auth_a = AuthContext::authenticated(user_id, vec!["member".to_string()], claims_a);
        let auth_b = AuthContext::authenticated(user_id, vec!["member".to_string()], claims_b);

        assert_ne!(
            QueryCacheCoordinator::auth_scope(&auth_a),
            QueryCacheCoordinator::auth_scope(&auth_b),
            "Different tenant claims must produce distinct scopes"
        );
    }

    /// Build a coordinator with a pre-populated table -> dependent-queries
    /// index. Lets the invalidation tests assert behaviour without standing
    /// up a full FunctionRegistry of fake handlers.
    fn coordinator_with_deps(deps: Vec<(&str, &str, &[&str])>) -> QueryCacheCoordinator {
        let mut index: HashMap<String, Vec<QueryDep>> = HashMap::new();
        for (table, name, cols) in deps {
            index.entry(table.to_string()).or_default().push(QueryDep {
                name: name.to_string(),
                selected_columns: cols.iter().map(|c| (*c).to_string()).collect(),
            });
        }
        QueryCacheCoordinator {
            cache: QueryCache::new(),
            table_to_queries: index,
        }
    }

    fn mutation_info(
        name: &'static str,
        tables: &'static [&'static str],
        changed: &'static [&'static str],
    ) -> FunctionInfo {
        FunctionInfo {
            name,
            description: None,
            kind: forge_core::FunctionKind::Mutation,
            required_role: None,
            is_public: false,
            cache_ttl: None,
            timeout: None,
            http_timeout: None,
            rate_limit_requests: None,
            rate_limit_per_secs: None,
            rate_limit_key: None,
            log_level: None,
            table_dependencies: tables,
            selected_columns: &[],
            changed_columns: changed,
            transactional: true,
            consistent: false,
            max_upload_size_bytes: None,
        }
    }

    #[test]
    fn invalidate_skips_query_when_columns_disjoint() {
        let coord = coordinator_with_deps(vec![
            ("users", "list_user_emails", &["id", "email"]),
            ("users", "list_user_names", &["id", "name"]),
        ]);
        coord.set_by_scope(
            "list_user_emails",
            &json!({}),
            Some("anon"),
            json!([]),
            Duration::from_secs(60),
        );
        coord.set_by_scope(
            "list_user_names",
            &json!({}),
            Some("anon"),
            json!([]),
            Duration::from_secs(60),
        );

        // Mutation changes only `name`. Email-only query should survive.
        coord.invalidate_for_mutation(&mutation_info("rename_user", &["users"], &["name"]));

        assert!(
            coord
                .get_by_scope("list_user_emails", &json!({}), Some("anon"))
                .is_some(),
            "email query must survive a name-only mutation"
        );
        assert!(
            coord
                .get_by_scope("list_user_names", &json!({}), Some("anon"))
                .is_none(),
            "name query must be invalidated"
        );
    }

    #[test]
    fn invalidate_falls_back_when_mutation_columns_unknown() {
        let coord = coordinator_with_deps(vec![("users", "list_user_emails", &["id", "email"])]);
        coord.set_by_scope(
            "list_user_emails",
            &json!({}),
            Some("anon"),
            json!([]),
            Duration::from_secs(60),
        );

        // Empty changed_columns => we don't know what the mutation touched,
        // so every dependent query has to go.
        coord.invalidate_for_mutation(&mutation_info("opaque_mutation", &["users"], &[]));

        assert!(
            coord
                .get_by_scope("list_user_emails", &json!({}), Some("anon"))
                .is_none(),
            "unknown column set must fall back to full invalidation"
        );
    }

    #[test]
    fn invalidate_falls_back_when_query_columns_unknown() {
        // Query with no extracted columns means dynamic SQL; we can't reason
        // about what it reads, so any mutation on the table invalidates it.
        let coord = coordinator_with_deps(vec![("users", "dynamic_query", &[])]);
        coord.set_by_scope(
            "dynamic_query",
            &json!({}),
            Some("anon"),
            json!([]),
            Duration::from_secs(60),
        );

        coord.invalidate_for_mutation(&mutation_info("rename_user", &["users"], &["name"]));

        assert!(
            coord
                .get_by_scope("dynamic_query", &json!({}), Some("anon"))
                .is_none(),
            "queries with unknown selected columns must always be invalidated"
        );
    }

    #[test]
    fn invalidate_by_change_evicts_matching_query() {
        use forge_core::realtime::{Change, ChangeOperation};

        let coord = coordinator_with_deps(vec![("users", "list_user_names", &["id", "name"])]);
        coord.set_by_scope(
            "list_user_names",
            &json!({}),
            Some("anon"),
            json!([]),
            Duration::from_secs(60),
        );

        let change =
            Change::new("users", ChangeOperation::Update).with_columns(vec!["name".to_string()]);
        coord.invalidate_by_change(&change);

        assert!(
            coord
                .get_by_scope("list_user_names", &json!({}), Some("anon"))
                .is_none(),
            "name change must invalidate name-reading query"
        );
    }

    #[test]
    fn invalidate_by_change_skips_disjoint_columns() {
        use forge_core::realtime::{Change, ChangeOperation};

        let coord = coordinator_with_deps(vec![("users", "list_user_emails", &["id", "email"])]);
        coord.set_by_scope(
            "list_user_emails",
            &json!({}),
            Some("anon"),
            json!([]),
            Duration::from_secs(60),
        );

        // Change touched only `name`; the email-only query should survive.
        let change =
            Change::new("users", ChangeOperation::Update).with_columns(vec!["name".to_string()]);
        coord.invalidate_by_change(&change);

        assert!(
            coord
                .get_by_scope("list_user_emails", &json!({}), Some("anon"))
                .is_some(),
            "disjoint column change must not invalidate"
        );
    }

    #[test]
    fn invalidate_by_change_falls_back_when_change_columns_unknown() {
        use forge_core::realtime::{Change, ChangeOperation};

        let coord = coordinator_with_deps(vec![("users", "list_user_emails", &["id", "email"])]);
        coord.set_by_scope(
            "list_user_emails",
            &json!({}),
            Some("anon"),
            json!([]),
            Duration::from_secs(60),
        );

        // Empty changed_columns (e.g. INSERT/DELETE) means "could be anything".
        let change = Change::new("users", ChangeOperation::Insert);
        coord.invalidate_by_change(&change);

        assert!(
            coord
                .get_by_scope("list_user_emails", &json!({}), Some("anon"))
                .is_none(),
            "unknown change columns must fall back to full invalidation"
        );
    }

    #[test]
    fn invalidate_by_change_ignores_unrelated_table() {
        use forge_core::realtime::{Change, ChangeOperation};

        let coord = coordinator_with_deps(vec![("users", "list_users", &["id"])]);
        coord.set_by_scope(
            "list_users",
            &json!({}),
            Some("anon"),
            json!([]),
            Duration::from_secs(60),
        );

        let change = Change::new("orders", ChangeOperation::Update);
        coord.invalidate_by_change(&change);

        assert!(
            coord
                .get_by_scope("list_users", &json!({}), Some("anon"))
                .is_some(),
            "change to unrelated table must not invalidate"
        );
    }

    #[tokio::test]
    async fn cluster_invalidator_evicts_on_broadcast() {
        use forge_core::realtime::{Change, ChangeOperation};

        let coord = Arc::new(coordinator_with_deps(vec![(
            "users",
            "list_user_names",
            &["id", "name"],
        )]));
        coord.set_by_scope(
            "list_user_names",
            &json!({}),
            Some("anon"),
            json!([]),
            Duration::from_secs(60),
        );

        let (tx, rx) = broadcast::channel::<Change>(8);
        let handle = Arc::clone(&coord).spawn_cluster_invalidator(rx);

        tx.send(
            Change::new("users", ChangeOperation::Update).with_columns(vec!["name".to_string()]),
        )
        .expect("send must succeed with active receiver");

        // Allow the spawned task to drain the broadcast.
        for _ in 0..50 {
            if coord
                .get_by_scope("list_user_names", &json!({}), Some("anon"))
                .is_none()
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        assert!(
            coord
                .get_by_scope("list_user_names", &json!({}), Some("anon"))
                .is_none(),
            "broadcast change must reach the invalidator and evict the entry"
        );

        drop(tx);
        handle.await.expect("invalidator task must exit cleanly");
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

use std::sync::Arc;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::readset::ReadSet;
use super::session::SessionId;

/// Unique subscription identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionId(pub Uuid);

impl SubscriptionId {
    /// Generate a new random subscription ID.
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    /// Create from an existing UUID.
    pub fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

    /// Get the inner UUID.
    pub fn as_uuid(&self) -> Uuid {
        self.0
    }
}

impl Default for SubscriptionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SubscriptionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Opaque dedup key for subscriptions sharing the same query+args+auth_scope.
///
/// Wraps a 64-bit FNV-1a hash (fixed seed, stable across processes) so the
/// algorithm can be swapped later without changing any call sites.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriptionKey(pub u64);

/// Compact identifier for query groups. u32 for cache-friendly storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct QueryGroupId(pub u32);

impl std::fmt::Display for QueryGroupId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "qg-{}", self.0)
    }
}

/// Compact identifier for subscribers within a slab.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SubscriberId(pub u32);

impl std::fmt::Display for SubscriberId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "sub-{}", self.0)
    }
}

/// Authentication scope for query group identity.
/// Two subscriptions with the same query+args but different auth scopes
/// must be in different groups (different users see different data).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct AuthScope {
    pub principal_id: Option<String>,
    pub tenant_id: Option<String>,
}

impl AuthScope {
    pub fn from_auth(auth: &crate::function::AuthContext) -> Self {
        Self {
            principal_id: auth.principal_id(),
            tenant_id: auth
                .claim("tenant_id")
                .and_then(|v| v.as_str())
                .map(ToString::to_string),
        }
    }
}

/// A query group is the primary unit of execution.
/// Multiple subscribers watching the same query+args+auth_scope share a single group.
/// On invalidation, the query executes once per group (not per subscriber).
pub struct QueryGroup {
    pub id: QueryGroupId,
    pub query_name: String,
    pub args: Arc<serde_json::Value>,
    pub auth_scope: AuthScope,
    pub auth_context: crate::function::AuthContext,
    /// Compile-time table dependencies from FunctionInfo.
    pub table_deps: &'static [&'static str],
    /// Compile-time selected columns from FunctionInfo.
    pub selected_cols: &'static [&'static str],
    pub read_set: ReadSet,
    /// Result hash for delta detection. Shared across all subscribers.
    pub last_result_hash: Option<String>,
    /// All subscribers in this group.
    pub subscribers: Vec<SubscriberId>,
    pub created_at: DateTime<Utc>,
    pub execution_count: u64,
}

/// `DefaultHasher` randomises its seed per process, so equal inputs hash to
/// different values after a restart. FNV-1a's fixed basis avoids that.
struct FnvHasher(u64);

impl FnvHasher {
    fn new() -> Self {
        Self(14695981039346656037)
    }
}

impl std::hash::Hasher for FnvHasher {
    fn write(&mut self, bytes: &[u8]) {
        for &b in bytes {
            self.0 ^= b as u64;
            self.0 = self.0.wrapping_mul(1099511628211);
        }
    }

    fn finish(&self) -> u64 {
        self.0
    }
}

impl QueryGroup {
    /// Compute the dedup key for (query_name, args, auth_scope).
    ///
    /// Uses FNV-1a so the result is stable across process restarts — unlike
    /// `DefaultHasher` which randomises its seed per process.
    pub fn compute_lookup_key(
        query_name: &str,
        args: &serde_json::Value,
        auth_scope: &AuthScope,
    ) -> SubscriptionKey {
        use std::hash::{Hash, Hasher};

        let mut hasher = FnvHasher::new();
        query_name.hash(&mut hasher);
        Self::hash_json_canonical(args, &mut hasher);
        auth_scope.hash(&mut hasher);
        SubscriptionKey(hasher.finish())
    }

    fn hash_json_canonical(value: &serde_json::Value, hasher: &mut impl std::hash::Hasher) {
        use std::hash::Hash;
        match value {
            serde_json::Value::Object(map) => {
                let mut keys: Vec<&String> = map.keys().collect();
                keys.sort();
                for key in keys {
                    key.hash(hasher);
                    Self::hash_json_canonical(&map[key], hasher);
                }
            }
            other => other.to_string().hash(hasher),
        }
    }

    /// Record a query execution result.
    pub fn record_execution(&mut self, read_set: ReadSet, result_hash: String) {
        self.read_set = read_set;
        self.last_result_hash = Some(result_hash);
        self.execution_count += 1;
    }

    /// Check if a change should invalidate this group.
    /// Uses the runtime read set when populated, otherwise falls back to the
    /// compile-time table dependencies from macro extraction.
    pub fn should_invalidate(&self, change: &super::readset::Change) -> bool {
        let table_matches = if self.read_set.tables.is_empty() {
            self.table_deps.iter().any(|t| *t == change.table)
        } else {
            change.invalidates(&self.read_set)
        };

        if !table_matches {
            return false;
        }

        if !change.invalidates_columns(self.selected_cols) {
            return false;
        }

        true
    }
}

/// Lightweight subscriber within a query group. ~48 bytes.
pub struct Subscriber {
    pub id: SubscriberId,
    pub session_id: SessionId,
    /// Client-facing subscription ID (what the client uses in subscribe/unsubscribe calls).
    pub client_sub_id: String,
    /// Back-reference to the query group this subscriber belongs to.
    pub group_id: QueryGroupId,
    /// Subscription ID used by the session server for tracking.
    pub subscription_id: SubscriptionId,
}

/// Subscription state from the client's perspective.
#[derive(Debug, Clone)]
pub struct SubscriptionState<T> {
    /// Whether the initial load is in progress.
    pub loading: bool,
    /// Current data.
    pub data: Option<T>,
    /// Error if any.
    pub error: Option<String>,
    /// Whether data may be stale (reconnecting).
    pub stale: bool,
}

impl<T> Default for SubscriptionState<T> {
    fn default() -> Self {
        Self {
            loading: true,
            data: None,
            error: None,
            stale: false,
        }
    }
}

impl<T> SubscriptionState<T> {
    /// Create a loading state.
    pub fn loading() -> Self {
        Self::default()
    }

    /// Create a state with data.
    pub fn with_data(data: T) -> Self {
        Self {
            loading: false,
            data: Some(data),
            error: None,
            stale: false,
        }
    }

    /// Create an error state.
    pub fn with_error(error: impl Into<String>) -> Self {
        Self {
            loading: false,
            data: None,
            error: Some(error.into()),
            stale: false,
        }
    }

    /// Mark as stale.
    pub fn mark_stale(&mut self) {
        self.stale = true;
    }

    /// Clear stale flag.
    pub fn clear_stale(&mut self) {
        self.stale = false;
    }
}

/// Delta format for subscription updates.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Delta<T> {
    /// New items added.
    pub added: Vec<T>,
    /// IDs of removed items.
    pub removed: Vec<String>,
    /// Updated items (partial).
    pub updated: Vec<T>,
}

impl<T> Default for Delta<T> {
    fn default() -> Self {
        Self {
            added: Vec::new(),
            removed: Vec::new(),
            updated: Vec::new(),
        }
    }
}

impl<T> Delta<T> {
    /// Create an empty delta.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Check if the delta is empty (no changes).
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty() && self.updated.is_empty()
    }

    /// Total number of changes.
    pub fn change_count(&self) -> usize {
        self.added.len() + self.removed.len() + self.updated.len()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn test_subscription_id_generation() {
        let id1 = SubscriptionId::new();
        let id2 = SubscriptionId::new();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_subscription_state_default() {
        let state: SubscriptionState<String> = SubscriptionState::default();
        assert!(state.loading);
        assert!(state.data.is_none());
        assert!(state.error.is_none());
        assert!(!state.stale);
    }

    #[test]
    fn test_subscription_state_with_data() {
        let state = SubscriptionState::with_data(vec![1, 2, 3]);
        assert!(!state.loading);
        assert_eq!(state.data, Some(vec![1, 2, 3]));
        assert!(state.error.is_none());
    }

    #[test]
    fn lookup_key_stable_across_calls() {
        let scope = AuthScope {
            principal_id: Some("user-1".to_string()),
            tenant_id: None,
        };
        let key1 = QueryGroup::compute_lookup_key(
            "get_projects",
            &serde_json::json!({"userId": "abc"}),
            &scope,
        );
        let key2 = QueryGroup::compute_lookup_key(
            "get_projects",
            &serde_json::json!({"userId": "abc"}),
            &scope,
        );
        assert_eq!(key1, key2);

        let other_scope = AuthScope {
            principal_id: Some("user-2".to_string()),
            tenant_id: None,
        };
        let key3 = QueryGroup::compute_lookup_key(
            "get_projects",
            &serde_json::json!({"userId": "abc"}),
            &other_scope,
        );
        assert_ne!(key1, key3);
    }

    #[test]
    fn lookup_key_is_deterministic() {
        let scope = AuthScope {
            principal_id: Some("u1".to_string()),
            tenant_id: None,
        };
        let key =
            QueryGroup::compute_lookup_key("get_items", &serde_json::json!({"id": "42"}), &scope);
        let expected =
            QueryGroup::compute_lookup_key("get_items", &serde_json::json!({"id": "42"}), &scope);
        assert_eq!(key, expected);
        // FNV-1a with a non-empty input can't produce 0 (sanity: hashing actually ran).
        assert_ne!(key.0, 0);
    }

    #[test]
    fn lookup_key_canonical_json_order_invariant() {
        let scope = AuthScope {
            principal_id: None,
            tenant_id: None,
        };
        let key_ab =
            QueryGroup::compute_lookup_key("q", &serde_json::json!({"a": 1, "b": 2}), &scope);
        let key_ba =
            QueryGroup::compute_lookup_key("q", &serde_json::json!({"b": 2, "a": 1}), &scope);
        assert_eq!(key_ab, key_ba);
    }

    #[test]
    fn test_delta_empty() {
        let delta: Delta<String> = Delta::empty();
        assert!(delta.is_empty());
        assert_eq!(delta.change_count(), 0);
    }

    #[test]
    fn test_delta_with_changes() {
        let delta = Delta {
            added: vec!["a".to_string()],
            removed: vec!["b".to_string()],
            updated: vec!["c".to_string()],
        };

        assert!(!delta.is_empty());
        assert_eq!(delta.change_count(), 3);
    }
}

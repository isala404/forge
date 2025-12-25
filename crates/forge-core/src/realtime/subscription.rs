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

/// Information about a server-side subscription.
#[derive(Debug, Clone)]
pub struct SubscriptionInfo {
    /// Unique subscription ID.
    pub id: SubscriptionId,
    /// Session that owns this subscription.
    pub session_id: SessionId,
    /// Query function name.
    pub query_name: String,
    /// Query arguments (as JSON).
    pub args: serde_json::Value,
    /// Hash of query + args for deduplication.
    pub query_hash: String,
    /// Read set from last execution.
    pub read_set: ReadSet,
    /// Hash of last result for delta computation.
    pub last_result_hash: Option<String>,
    /// When the subscription was created.
    pub created_at: DateTime<Utc>,
    /// When the subscription was last executed.
    pub last_executed_at: Option<DateTime<Utc>>,
    /// Number of times the subscription has been re-executed.
    pub execution_count: u64,
    /// Estimated memory usage in bytes.
    pub memory_bytes: usize,
}

impl SubscriptionInfo {
    /// Create a new subscription info.
    pub fn new(
        session_id: SessionId,
        query_name: impl Into<String>,
        args: serde_json::Value,
    ) -> Self {
        let query_name = query_name.into();
        let query_hash = compute_query_hash(&query_name, &args);

        Self {
            id: SubscriptionId::new(),
            session_id,
            query_name,
            args,
            query_hash,
            read_set: ReadSet::new(),
            last_result_hash: None,
            created_at: Utc::now(),
            last_executed_at: None,
            execution_count: 0,
            memory_bytes: 0,
        }
    }

    /// Update after execution.
    pub fn record_execution(&mut self, read_set: ReadSet, result_hash: String) {
        self.read_set = read_set;
        self.memory_bytes = self.read_set.memory_bytes() + self.query_name.len() + 128;
        self.last_result_hash = Some(result_hash);
        self.last_executed_at = Some(Utc::now());
        self.execution_count += 1;
    }

    /// Check if a change should invalidate this subscription.
    pub fn should_invalidate(&self, change: &super::readset::Change) -> bool {
        change.invalidates(&self.read_set)
    }
}

/// Compute a hash of query name + args for deduplication.
fn compute_query_hash(query_name: &str, args: &serde_json::Value) -> String {
    use std::collections::hash_map::DefaultHasher;
    use std::hash::{Hash, Hasher};

    let mut hasher = DefaultHasher::new();
    query_name.hash(&mut hasher);
    args.to_string().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
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
    fn test_subscription_info_creation() {
        let session_id = SessionId::new();
        let info = SubscriptionInfo::new(
            session_id,
            "get_projects",
            serde_json::json!({"userId": "abc"}),
        );

        assert_eq!(info.query_name, "get_projects");
        assert_eq!(info.execution_count, 0);
        assert!(!info.query_hash.is_empty());
    }

    #[test]
    fn test_query_hash_consistency() {
        let hash1 = compute_query_hash("get_projects", &serde_json::json!({"userId": "abc"}));
        let hash2 = compute_query_hash("get_projects", &serde_json::json!({"userId": "abc"}));
        let hash3 = compute_query_hash("get_projects", &serde_json::json!({"userId": "xyz"}));

        assert_eq!(hash1, hash2);
        assert_ne!(hash1, hash3);
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

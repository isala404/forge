use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;

use forge_core::cluster::NodeId;
use forge_core::realtime::{
    Change, ReadSet, SessionId, SessionInfo, SessionStatus, SubscriptionId, SubscriptionInfo,
};

/// Session manager for tracking WebSocket connections.
pub struct SessionManager {
    sessions: Arc<RwLock<HashMap<SessionId, SessionInfo>>>,
    node_id: NodeId,
}

impl SessionManager {
    /// Create a new session manager.
    pub fn new(node_id: NodeId) -> Self {
        Self {
            sessions: Arc::new(RwLock::new(HashMap::new())),
            node_id,
        }
    }

    /// Create a new session.
    pub async fn create_session(&self) -> SessionInfo {
        let mut session = SessionInfo::new(self.node_id);
        session.connect();

        let mut sessions = self.sessions.write().await;
        sessions.insert(session.id, session.clone());

        session
    }

    /// Get a session by ID.
    pub async fn get_session(&self, session_id: SessionId) -> Option<SessionInfo> {
        let sessions = self.sessions.read().await;
        sessions.get(&session_id).cloned()
    }

    /// Update a session.
    pub async fn update_session(&self, session: SessionInfo) {
        let mut sessions = self.sessions.write().await;
        sessions.insert(session.id, session);
    }

    /// Remove a session.
    pub async fn remove_session(&self, session_id: SessionId) {
        let mut sessions = self.sessions.write().await;
        sessions.remove(&session_id);
    }

    /// Mark a session as disconnected.
    pub async fn disconnect_session(&self, session_id: SessionId) {
        let mut sessions = self.sessions.write().await;
        if let Some(session) = sessions.get_mut(&session_id) {
            session.disconnect();
        }
    }

    /// Get all connected sessions.
    pub async fn get_connected_sessions(&self) -> Vec<SessionInfo> {
        let sessions = self.sessions.read().await;
        sessions
            .values()
            .filter(|s| s.is_connected())
            .cloned()
            .collect()
    }

    /// Count sessions by status.
    pub async fn count_by_status(&self) -> SessionCounts {
        let sessions = self.sessions.read().await;
        let mut counts = SessionCounts::default();

        for session in sessions.values() {
            match session.status {
                SessionStatus::Connecting => counts.connecting += 1,
                SessionStatus::Connected => counts.connected += 1,
                SessionStatus::Reconnecting => counts.reconnecting += 1,
                SessionStatus::Disconnected => counts.disconnected += 1,
            }
            counts.total += 1;
        }

        counts
    }

    /// Clean up disconnected sessions older than the given duration.
    pub async fn cleanup_old_sessions(&self, max_age: std::time::Duration) {
        let mut sessions = self.sessions.write().await;
        let cutoff = chrono::Utc::now() - chrono::Duration::from_std(max_age).unwrap();

        sessions.retain(|_, session| {
            session.status != SessionStatus::Disconnected || session.last_active_at > cutoff
        });
    }
}

/// Session count statistics.
#[derive(Debug, Clone, Default)]
pub struct SessionCounts {
    /// Connecting sessions.
    pub connecting: usize,
    /// Connected sessions.
    pub connected: usize,
    /// Reconnecting sessions.
    pub reconnecting: usize,
    /// Disconnected sessions.
    pub disconnected: usize,
    /// Total sessions.
    pub total: usize,
}

/// Subscription manager for tracking active subscriptions.
pub struct SubscriptionManager {
    /// Subscriptions by ID.
    subscriptions: Arc<RwLock<HashMap<SubscriptionId, SubscriptionInfo>>>,
    /// Subscriptions by session ID for fast lookup.
    by_session: Arc<RwLock<HashMap<SessionId, Vec<SubscriptionId>>>>,
    /// Subscriptions by query hash for deduplication.
    by_query_hash: Arc<RwLock<HashMap<String, Vec<SubscriptionId>>>>,
    /// Maximum subscriptions per session.
    max_per_session: usize,
}

impl SubscriptionManager {
    /// Create a new subscription manager.
    pub fn new(max_per_session: usize) -> Self {
        Self {
            subscriptions: Arc::new(RwLock::new(HashMap::new())),
            by_session: Arc::new(RwLock::new(HashMap::new())),
            by_query_hash: Arc::new(RwLock::new(HashMap::new())),
            max_per_session,
        }
    }

    /// Create a new subscription.
    pub async fn create_subscription(
        &self,
        session_id: SessionId,
        query_name: impl Into<String>,
        args: serde_json::Value,
    ) -> forge_core::Result<SubscriptionInfo> {
        // Check limit
        let by_session = self.by_session.read().await;
        if let Some(subs) = by_session.get(&session_id) {
            if subs.len() >= self.max_per_session {
                return Err(forge_core::ForgeError::Validation(format!(
                    "Maximum subscriptions per session ({}) exceeded",
                    self.max_per_session
                )));
            }
        }
        drop(by_session);

        let subscription = SubscriptionInfo::new(session_id, query_name, args);

        // Store subscription
        let mut subscriptions = self.subscriptions.write().await;
        subscriptions.insert(subscription.id, subscription.clone());

        // Index by session
        let mut by_session = self.by_session.write().await;
        by_session
            .entry(session_id)
            .or_default()
            .push(subscription.id);

        // Index by query hash
        let mut by_query_hash = self.by_query_hash.write().await;
        by_query_hash
            .entry(subscription.query_hash.clone())
            .or_default()
            .push(subscription.id);

        Ok(subscription)
    }

    /// Get a subscription by ID.
    pub async fn get_subscription(
        &self,
        subscription_id: SubscriptionId,
    ) -> Option<SubscriptionInfo> {
        let subscriptions = self.subscriptions.read().await;
        subscriptions.get(&subscription_id).cloned()
    }

    /// Update a subscription after execution.
    pub async fn update_subscription(
        &self,
        subscription_id: SubscriptionId,
        read_set: ReadSet,
        result_hash: String,
    ) {
        let mut subscriptions = self.subscriptions.write().await;
        if let Some(sub) = subscriptions.get_mut(&subscription_id) {
            sub.record_execution(read_set, result_hash);
        }
    }

    /// Remove a subscription.
    pub async fn remove_subscription(&self, subscription_id: SubscriptionId) {
        let mut subscriptions = self.subscriptions.write().await;
        if let Some(sub) = subscriptions.remove(&subscription_id) {
            // Remove from session index
            let mut by_session = self.by_session.write().await;
            if let Some(subs) = by_session.get_mut(&sub.session_id) {
                subs.retain(|id| *id != subscription_id);
            }

            // Remove from query hash index
            let mut by_query_hash = self.by_query_hash.write().await;
            if let Some(subs) = by_query_hash.get_mut(&sub.query_hash) {
                subs.retain(|id| *id != subscription_id);
            }
        }
    }

    /// Remove all subscriptions for a session.
    pub async fn remove_session_subscriptions(&self, session_id: SessionId) {
        let subscription_ids: Vec<SubscriptionId> = {
            let by_session = self.by_session.read().await;
            by_session.get(&session_id).cloned().unwrap_or_default()
        };

        for sub_id in subscription_ids {
            self.remove_subscription(sub_id).await;
        }

        // Clean up session entry
        let mut by_session = self.by_session.write().await;
        by_session.remove(&session_id);
    }

    /// Find subscriptions affected by a change.
    pub async fn find_affected_subscriptions(&self, change: &Change) -> Vec<SubscriptionId> {
        let subscriptions = self.subscriptions.read().await;
        subscriptions
            .iter()
            .filter(|(_, sub)| sub.should_invalidate(change))
            .map(|(id, _)| *id)
            .collect()
    }

    /// Get subscriptions by query hash (for coalescing).
    pub async fn get_by_query_hash(&self, query_hash: &str) -> Vec<SubscriptionInfo> {
        let by_query_hash = self.by_query_hash.read().await;
        let subscriptions = self.subscriptions.read().await;

        by_query_hash
            .get(query_hash)
            .map(|ids| {
                ids.iter()
                    .filter_map(|id| subscriptions.get(id).cloned())
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Get subscription counts.
    pub async fn counts(&self) -> SubscriptionCounts {
        let subscriptions = self.subscriptions.read().await;
        let by_session = self.by_session.read().await;

        SubscriptionCounts {
            total: subscriptions.len(),
            unique_queries: self.by_query_hash.read().await.len(),
            sessions: by_session.len(),
            memory_bytes: subscriptions.values().map(|s| s.memory_bytes).sum(),
        }
    }
}

/// Subscription count statistics.
#[derive(Debug, Clone, Default)]
pub struct SubscriptionCounts {
    /// Total subscriptions.
    pub total: usize,
    /// Number of unique queries (coalesced).
    pub unique_queries: usize,
    /// Number of sessions with subscriptions.
    pub sessions: usize,
    /// Total memory used by subscriptions.
    pub memory_bytes: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_session_manager_create() {
        let node_id = NodeId::new();
        let manager = SessionManager::new(node_id);

        let session = manager.create_session().await;
        assert!(session.is_connected());

        let retrieved = manager.get_session(session.id).await;
        assert!(retrieved.is_some());
    }

    #[tokio::test]
    async fn test_session_manager_disconnect() {
        let node_id = NodeId::new();
        let manager = SessionManager::new(node_id);

        let session = manager.create_session().await;
        manager.disconnect_session(session.id).await;

        let retrieved = manager.get_session(session.id).await.unwrap();
        assert!(!retrieved.is_connected());
    }

    #[tokio::test]
    async fn test_subscription_manager_create() {
        let manager = SubscriptionManager::new(50);
        let session_id = SessionId::new();

        let sub = manager
            .create_subscription(session_id, "get_projects", serde_json::json!({}))
            .await
            .unwrap();

        assert_eq!(sub.query_name, "get_projects");

        let retrieved = manager.get_subscription(sub.id).await;
        assert!(retrieved.is_some());
    }

    #[tokio::test]
    async fn test_subscription_manager_limit() {
        let manager = SubscriptionManager::new(2);
        let session_id = SessionId::new();

        // First two should succeed
        manager
            .create_subscription(session_id, "query1", serde_json::json!({}))
            .await
            .unwrap();
        manager
            .create_subscription(session_id, "query2", serde_json::json!({}))
            .await
            .unwrap();

        // Third should fail
        let result = manager
            .create_subscription(session_id, "query3", serde_json::json!({}))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_subscription_manager_remove_session() {
        let manager = SubscriptionManager::new(50);
        let session_id = SessionId::new();

        manager
            .create_subscription(session_id, "query1", serde_json::json!({}))
            .await
            .unwrap();
        manager
            .create_subscription(session_id, "query2", serde_json::json!({}))
            .await
            .unwrap();

        let counts = manager.counts().await;
        assert_eq!(counts.total, 2);

        manager.remove_session_subscriptions(session_id).await;

        let counts = manager.counts().await;
        assert_eq!(counts.total, 0);
    }
}

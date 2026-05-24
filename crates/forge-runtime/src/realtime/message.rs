use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};
use std::time::Duration;

use dashmap::DashMap;
use serde::Serialize;
use tokio::sync::mpsc;

use forge_core::cluster::NodeId;
use forge_core::realtime::{Delta, SessionId, SubscriptionId};

#[derive(Debug, Clone)]
pub struct RealtimeConfig {
    pub max_subscriptions_per_session: usize,
}

impl Default for RealtimeConfig {
    fn default() -> Self {
        Self {
            max_subscriptions_per_session: 100,
        }
    }
}

/// Job data sent to client (subset of internal JobRecord).
#[derive(Debug, Clone, Serialize)]
pub struct JobData {
    pub job_id: String,
    pub status: String,
    #[serde(rename = "progress")]
    pub progress_percent: Option<i32>,
    #[serde(rename = "message")]
    pub progress_message: Option<String>,
    pub output: Option<serde_json::Value>,
    pub error: Option<String>,
}

/// Workflow data sent to client.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowData {
    pub workflow_id: String,
    pub status: String,
    #[serde(rename = "step")]
    pub current_step: Option<String>,
    pub waiting_for: Option<String>,
    pub steps: Vec<WorkflowStepData>,
    pub output: Option<serde_json::Value>,
    pub error: Option<String>,
}

/// Workflow step data sent to client.
#[derive(Debug, Clone, Serialize)]
pub struct WorkflowStepData {
    pub name: String,
    pub status: String,
    pub error: Option<String>,
}

/// Server -> SSE-pipeline message envelope.
///
/// `#[non_exhaustive]` so 1.0.x can add new variants without breaking
/// downstream Rust matchers (forge-dioxus, custom integrations).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum RealtimeMessage {
    Data {
        subscription_id: String,
        data: std::sync::Arc<serde_json::Value>,
    },
    DeltaUpdate {
        subscription_id: String,
        delta: Delta<serde_json::Value>,
    },
    JobUpdate {
        client_sub_id: String,
        job: JobData,
    },
    WorkflowUpdate {
        client_sub_id: String,
        workflow: WorkflowData,
    },
    AuthFailed {
        reason: String,
    },
    /// Sent to slow clients before disconnecting them.
    Lagging,
}

/// Per-session state with backpressure tracking.
struct SessionEntry {
    sender: mpsc::Sender<RealtimeMessage>,
    subscriptions: Vec<SubscriptionId>,
    connected_at: chrono::DateTime<chrono::Utc>,
    /// Unix timestamp (seconds) of the last successful push to this session.
    /// Stored atomically so `try_send_to_session` (which holds a shared
    /// reference via `DashMap::get`) can refresh it without re-acquiring an
    /// exclusive lock. `cleanup_stale` reads this to evict sessions that have
    /// gone quiet — without the bump, eviction would key off connection age
    /// alone and tear down healthy long-lived sessions.
    last_active: AtomicI64,
    /// Consecutive failed try_send attempts. Resets on success.
    consecutive_drops: AtomicU32,
    /// Cumulative drops since connection. Never resets, so intermittently-slow
    /// clients that drain just enough to avoid consecutive eviction still get
    /// caught once total drops exceed the lifetime threshold.
    total_drops: AtomicU32,
    /// JWT expiry as Unix timestamp. `None` for unauthenticated (anonymous) sessions.
    token_exp: Option<i64>,
}

/// Maximum consecutive drops before evicting a slow client.
const MAX_CONSECUTIVE_DROPS: u32 = 10;
/// Maximum lifetime drops before evicting an intermittently-slow client.
const MAX_TOTAL_DROPS: u32 = 50;

pub struct SessionServer {
    config: RealtimeConfig,
    node_id: NodeId,
    /// Active connections by session ID. DashMap for concurrent access.
    connections: DashMap<SessionId, SessionEntry>,
    /// Subscription to session mapping for fast reverse lookup.
    subscription_sessions: DashMap<SubscriptionId, SessionId>,
}

impl SessionServer {
    /// Create a new session server.
    pub fn new(node_id: NodeId, config: RealtimeConfig) -> Self {
        Self {
            config,
            node_id,
            connections: DashMap::new(),
            subscription_sessions: DashMap::new(),
        }
    }

    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    pub fn config(&self) -> &RealtimeConfig {
        &self.config
    }

    /// Register a new connection.
    ///
    /// `token_exp` is the JWT `exp` claim (Unix timestamp). Pass `None` for
    /// unauthenticated sessions. Events will not be pushed to sessions whose
    /// token has expired.
    pub fn register_connection(
        &self,
        session_id: SessionId,
        sender: mpsc::Sender<RealtimeMessage>,
        token_exp: Option<i64>,
    ) {
        let now = chrono::Utc::now();
        let entry = SessionEntry {
            sender,
            subscriptions: Vec::new(),
            connected_at: now,
            last_active: AtomicI64::new(now.timestamp()),
            consecutive_drops: AtomicU32::new(0),
            total_drops: AtomicU32::new(0),
            token_exp,
        };
        // Notify the displaced client before replacing it so it doesn't
        // think it's still receiving live data on a dead channel.
        if let Some(prev) = self.connections.insert(session_id, entry) {
            let _ = prev.sender.try_send(RealtimeMessage::AuthFailed {
                reason: "Session replaced by a newer connection".to_string(),
            });
        }
    }

    /// Notify the client that its auth has been revoked, then tear down the
    /// connection. Returns the subscription IDs the session held, so callers
    /// can clean up associated query/job/workflow state. The notification is
    /// best-effort (`try_send`): if the channel is full or closed, eviction
    /// still proceeds.
    ///
    /// Use this when a session's underlying principal has been demoted,
    /// tenant-moved, or revoked server-side before the JWT's `exp`. The
    /// reactor's cached `AuthContext` on each `QueryGroup` is only
    /// re-validated on token expiry, so without an explicit revocation path
    /// the session would keep receiving data under the stale scope until
    /// `exp`. After this call the client must reconnect and re-subscribe
    /// with a fresh token.
    pub fn revoke_session(
        &self,
        session_id: SessionId,
        reason: &str,
    ) -> Option<Vec<SubscriptionId>> {
        if let Some(conn) = self.connections.get(&session_id) {
            let _ = conn.sender.try_send(RealtimeMessage::AuthFailed {
                reason: reason.to_string(),
            });
        }
        self.remove_connection(session_id)
    }

    /// Remove a connection.
    pub fn remove_connection(&self, session_id: SessionId) -> Option<Vec<SubscriptionId>> {
        if let Some((_, conn)) = self.connections.remove(&session_id) {
            for sub_id in &conn.subscriptions {
                self.subscription_sessions.remove(sub_id);
            }
            Some(conn.subscriptions)
        } else {
            None
        }
    }

    /// Add a subscription to a connection.
    pub fn add_subscription(
        &self,
        session_id: SessionId,
        subscription_id: SubscriptionId,
    ) -> forge_core::Result<()> {
        let mut conn = self
            .connections
            .get_mut(&session_id)
            .ok_or_else(|| forge_core::ForgeError::Validation("Session not found".to_string()))?;

        if conn.subscriptions.len() >= self.config.max_subscriptions_per_session {
            return Err(forge_core::ForgeError::Validation(format!(
                "Maximum subscriptions per session ({}) exceeded",
                self.config.max_subscriptions_per_session
            )));
        }

        conn.subscriptions.push(subscription_id);
        drop(conn);

        self.subscription_sessions
            .insert(subscription_id, session_id);

        Ok(())
    }

    /// Remove a subscription from a connection.
    pub fn remove_subscription(&self, subscription_id: SubscriptionId) {
        if let Some((_, session_id)) = self.subscription_sessions.remove(&subscription_id)
            && let Some(mut conn) = self.connections.get_mut(&session_id)
        {
            conn.subscriptions.retain(|id| *id != subscription_id);
        }
    }

    /// Non-blocking send with backpressure. Returns false if client was evicted.
    ///
    /// Before pushing, checks whether the session's JWT has expired. Expired
    /// sessions are evicted immediately so the client reconnects with a fresh token.
    pub fn try_send_to_session(
        &self,
        session_id: SessionId,
        message: RealtimeMessage,
    ) -> Result<(), SendError> {
        let conn = self
            .connections
            .get(&session_id)
            .ok_or(SendError::SessionNotFound)?;

        if let Some(exp) = conn.token_exp {
            let now = chrono::Utc::now().timestamp();
            if exp < now {
                drop(conn);
                tracing::debug!(%session_id, "Evicting SSE session with expired token");
                self.evict_session(session_id);
                return Err(SendError::TokenExpired);
            }
        }

        match conn.sender.try_send(message) {
            Ok(()) => {
                conn.consecutive_drops.store(0, Ordering::Relaxed);
                conn.last_active
                    .store(chrono::Utc::now().timestamp(), Ordering::Relaxed);
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(_)) => {
                let consecutive = conn.consecutive_drops.fetch_add(1, Ordering::Relaxed);
                let total = conn.total_drops.fetch_add(1, Ordering::Relaxed);
                if consecutive >= MAX_CONSECUTIVE_DROPS || total >= MAX_TOTAL_DROPS {
                    let _ = conn.sender.try_send(RealtimeMessage::Lagging);
                    drop(conn);
                    self.evict_session(session_id);
                    Err(SendError::Evicted)
                } else {
                    Err(SendError::Full)
                }
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                drop(conn);
                self.remove_connection(session_id);
                Err(SendError::Closed)
            }
        }
    }

    /// Blocking send for initial data delivery where we need backpressure.
    pub async fn send_to_session(
        &self,
        session_id: SessionId,
        message: RealtimeMessage,
    ) -> forge_core::Result<()> {
        let sender = {
            let conn = self.connections.get(&session_id).ok_or_else(|| {
                forge_core::ForgeError::Validation("Session not found".to_string())
            })?;
            conn.sender.clone()
        };

        sender
            .send(message)
            .await
            .map_err(|_| forge_core::ForgeError::internal("Failed to send message"))
    }

    /// Send a delta to all sessions subscribed to a subscription.
    pub async fn broadcast_delta(
        &self,
        subscription_id: SubscriptionId,
        delta: Delta<serde_json::Value>,
    ) -> forge_core::Result<()> {
        let session_id = self.subscription_sessions.get(&subscription_id).map(|r| *r);

        if let Some(session_id) = session_id {
            let message = RealtimeMessage::DeltaUpdate {
                subscription_id: subscription_id.to_string(),
                delta,
            };
            self.send_to_session(session_id, message).await?;
        }

        Ok(())
    }

    /// Evict a slow session.
    fn evict_session(&self, session_id: SessionId) {
        tracing::warn!(?session_id, "Evicting slow client");
        self.remove_connection(session_id);
    }

    /// Get connection count.
    pub fn connection_count(&self) -> usize {
        self.connections.len()
    }

    /// Get subscription count.
    pub fn subscription_count(&self) -> usize {
        self.subscription_sessions.len()
    }

    /// Get server statistics.
    pub fn stats(&self) -> SessionStats {
        let total_subscriptions: usize =
            self.connections.iter().map(|c| c.subscriptions.len()).sum();

        SessionStats {
            connections: self.connections.len(),
            subscriptions: total_subscriptions,
            node_id: self.node_id,
        }
    }

    /// Cleanup stale connections.
    pub fn cleanup_stale(&self, max_idle: Duration) {
        let cutoff_ts = (chrono::Utc::now()
            - chrono::Duration::from_std(max_idle).unwrap_or(chrono::Duration::days(30)))
        .timestamp();

        let stale: Vec<(SessionId, chrono::DateTime<chrono::Utc>)> = self
            .connections
            .iter()
            .filter(|entry| entry.last_active.load(Ordering::Relaxed) < cutoff_ts)
            .map(|entry| (*entry.key(), entry.connected_at))
            .collect();

        if let Some((_, oldest_connected_at)) =
            stale.iter().min_by_key(|(_, connected_at)| *connected_at)
        {
            tracing::debug!(
                count = stale.len(),
                oldest_connected_at = %oldest_connected_at,
                "Cleaning up stale connections"
            );
        }

        for (session_id, _) in stale {
            // Re-check last_active under the entry guard: a concurrent
            // try_send may have bumped it between the snapshot and now,
            // and evicting a connection that just successfully received
            // traffic would drop a healthy client.
            let still_stale = self
                .connections
                .get(&session_id)
                .is_some_and(|c| c.last_active.load(Ordering::Relaxed) < cutoff_ts);
            if still_stale {
                self.remove_connection(session_id);
            }
        }
    }

    /// Evict sessions whose JWT has expired. Sends an auth error event to each
    /// so the client knows to re-authenticate rather than just reconnecting.
    ///
    /// Returns the session IDs that were evicted so callers can immediately
    /// clean up associated subscription state (query groups, job/workflow
    /// subscriptions) without waiting for the SSE bridge task to notice the
    /// closed channel.
    pub fn cleanup_expired_tokens(&self) -> Vec<SessionId> {
        let now = chrono::Utc::now().timestamp();

        let expired: Vec<SessionId> = self
            .connections
            .iter()
            .filter(|entry| entry.token_exp.is_some_and(|exp| exp < now))
            .map(|entry| *entry.key())
            .collect();

        if expired.is_empty() {
            return Vec::new();
        }

        tracing::debug!(
            count = expired.len(),
            "Evicting sessions with expired tokens"
        );

        for &session_id in &expired {
            if let Some(conn) = self.connections.get(&session_id) {
                let _ = conn.sender.try_send(RealtimeMessage::AuthFailed {
                    reason: "Token expired".to_string(),
                });
            }
            self.evict_session(session_id);
        }

        expired
    }
}

/// Error type for try_send operations.
#[derive(Debug)]
pub enum SendError {
    SessionNotFound,
    Full,
    Closed,
    Evicted,
    /// Session's JWT has expired; the session was evicted.
    TokenExpired,
}

/// Session server statistics.
#[derive(Debug, Clone)]
pub struct SessionStats {
    pub connections: usize,
    pub subscriptions: usize,
    pub node_id: NodeId,
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn test_realtime_config_default() {
        let config = RealtimeConfig::default();
        assert_eq!(config.max_subscriptions_per_session, 100);
    }

    #[test]
    fn test_session_server_creation() {
        let node_id = NodeId::new();
        let server = SessionServer::new(node_id, RealtimeConfig::default());

        assert_eq!(server.node_id(), node_id);
        assert_eq!(server.connection_count(), 0);
        assert_eq!(server.subscription_count(), 0);
    }

    #[test]
    fn test_session_connection() {
        let node_id = NodeId::new();
        let server = SessionServer::new(node_id, RealtimeConfig::default());
        let session_id = SessionId::new();
        let (tx, _rx) = mpsc::channel(100);

        server.register_connection(session_id, tx, None);
        assert_eq!(server.connection_count(), 1);

        let removed = server.remove_connection(session_id);
        assert!(removed.is_some());
        assert_eq!(server.connection_count(), 0);
    }

    #[test]
    fn test_session_subscription() {
        let node_id = NodeId::new();
        let server = SessionServer::new(node_id, RealtimeConfig::default());
        let session_id = SessionId::new();
        let subscription_id = SubscriptionId::new();
        let (tx, _rx) = mpsc::channel(100);

        server.register_connection(session_id, tx, None);
        server
            .add_subscription(session_id, subscription_id)
            .unwrap();

        assert_eq!(server.subscription_count(), 1);

        server.remove_subscription(subscription_id);
        assert_eq!(server.subscription_count(), 0);
    }

    #[test]
    fn test_session_subscription_limit() {
        let node_id = NodeId::new();
        let config = RealtimeConfig {
            max_subscriptions_per_session: 2,
        };
        let server = SessionServer::new(node_id, config);
        let session_id = SessionId::new();
        let (tx, _rx) = mpsc::channel(100);

        server.register_connection(session_id, tx, None);

        server
            .add_subscription(session_id, SubscriptionId::new())
            .unwrap();
        server
            .add_subscription(session_id, SubscriptionId::new())
            .unwrap();

        let result = server.add_subscription(session_id, SubscriptionId::new());
        assert!(result.is_err());
    }

    #[test]
    fn test_try_send_backpressure() {
        let node_id = NodeId::new();
        let server = SessionServer::new(node_id, RealtimeConfig::default());
        let session_id = SessionId::new();
        // Tiny buffer to trigger backpressure
        let (tx, _rx) = mpsc::channel(1);

        server.register_connection(session_id, tx, None);

        // First send should succeed
        let result = server.try_send_to_session(session_id, RealtimeMessage::Lagging);
        assert!(result.is_ok());

        // Second send to full buffer should return Full
        let result = server.try_send_to_session(session_id, RealtimeMessage::Lagging);
        assert!(matches!(result, Err(SendError::Full)));
    }

    #[test]
    fn test_session_stats() {
        let node_id = NodeId::new();
        let server = SessionServer::new(node_id, RealtimeConfig::default());
        let session_id = SessionId::new();
        let (tx, _rx) = mpsc::channel(100);

        server.register_connection(session_id, tx, None);
        server
            .add_subscription(session_id, SubscriptionId::new())
            .unwrap();
        server
            .add_subscription(session_id, SubscriptionId::new())
            .unwrap();

        let stats = server.stats();
        assert_eq!(stats.connections, 1);
        assert_eq!(stats.subscriptions, 2);
        assert_eq!(stats.node_id, node_id);
    }

    #[test]
    fn expired_token_session_is_evicted_on_push() {
        let node_id = NodeId::new();
        let server = SessionServer::new(node_id, RealtimeConfig::default());
        let session_id = SessionId::new();
        let (tx, _rx) = mpsc::channel(100);

        // Register with an already-expired token (exp = 1 second into Unix epoch)
        server.register_connection(session_id, tx, Some(1));
        assert_eq!(server.connection_count(), 1);

        let result = server.try_send_to_session(session_id, RealtimeMessage::Lagging);

        assert!(matches!(result, Err(SendError::TokenExpired)));
        // Session evicted — no longer tracked
        assert_eq!(server.connection_count(), 0);
    }

    #[test]
    fn valid_token_session_is_not_evicted() {
        let node_id = NodeId::new();
        let server = SessionServer::new(node_id, RealtimeConfig::default());
        let session_id = SessionId::new();
        let (tx, _rx) = mpsc::channel(100);

        // Token that expires an hour from now
        let future_exp = chrono::Utc::now().timestamp() + 3600;
        server.register_connection(session_id, tx, Some(future_exp));

        let result = server.try_send_to_session(session_id, RealtimeMessage::Lagging);

        assert!(result.is_ok());
        assert_eq!(server.connection_count(), 1);
    }

    #[test]
    fn try_send_to_missing_session_returns_not_found() {
        let server = SessionServer::new(NodeId::new(), RealtimeConfig::default());
        let result = server.try_send_to_session(SessionId::new(), RealtimeMessage::Lagging);
        assert!(matches!(result, Err(SendError::SessionNotFound)));
    }

    #[test]
    fn try_send_to_closed_sender_returns_closed_and_evicts() {
        let server = SessionServer::new(NodeId::new(), RealtimeConfig::default());
        let session_id = SessionId::new();
        let (tx, rx) = mpsc::channel(1);

        server.register_connection(session_id, tx, None);
        drop(rx);

        let result = server.try_send_to_session(session_id, RealtimeMessage::Lagging);
        assert!(matches!(result, Err(SendError::Closed)));
        assert_eq!(server.connection_count(), 0);
    }

    #[test]
    fn backpressure_evicts_after_max_consecutive_drops() {
        let server = SessionServer::new(NodeId::new(), RealtimeConfig::default());
        let session_id = SessionId::new();
        // Buffer of 1: one send succeeds, all subsequent attempts return Full.
        let (tx, _rx) = mpsc::channel(1);
        server.register_connection(session_id, tx, None);

        // Fill the buffer.
        assert!(
            server
                .try_send_to_session(session_id, RealtimeMessage::Lagging)
                .is_ok()
        );

        // First MAX_CONSECUTIVE_DROPS Full results don't evict.
        for _ in 0..MAX_CONSECUTIVE_DROPS {
            let r = server.try_send_to_session(session_id, RealtimeMessage::Lagging);
            assert!(matches!(r, Err(SendError::Full)));
        }
        assert_eq!(server.connection_count(), 1);

        // The next Full crosses the threshold and evicts.
        let r = server.try_send_to_session(session_id, RealtimeMessage::Lagging);
        assert!(matches!(r, Err(SendError::Evicted)));
        assert_eq!(server.connection_count(), 0);
    }

    #[tokio::test]
    async fn drop_counter_resets_on_successful_send() {
        let server = SessionServer::new(NodeId::new(), RealtimeConfig::default());
        let session_id = SessionId::new();
        let (tx, mut rx) = mpsc::channel(1);
        server.register_connection(session_id, tx, None);

        // Fill, then accumulate Full errors short of the threshold.
        assert!(
            server
                .try_send_to_session(session_id, RealtimeMessage::Lagging)
                .is_ok()
        );
        for _ in 0..(MAX_CONSECUTIVE_DROPS - 1) {
            assert!(matches!(
                server.try_send_to_session(session_id, RealtimeMessage::Lagging),
                Err(SendError::Full)
            ));
        }

        // Drain so the next send succeeds and resets the counter.
        let _ = rx.recv().await;
        assert!(
            server
                .try_send_to_session(session_id, RealtimeMessage::Lagging)
                .is_ok()
        );

        // The counter reset, so we can now absorb a fresh batch of Full errors
        // without being evicted.
        for _ in 0..MAX_CONSECUTIVE_DROPS {
            assert!(matches!(
                server.try_send_to_session(session_id, RealtimeMessage::Lagging),
                Err(SendError::Full)
            ));
        }
        assert_eq!(server.connection_count(), 1);
    }

    #[tokio::test]
    async fn total_drops_evicts_intermittently_slow_client() {
        let server = SessionServer::new(NodeId::new(), RealtimeConfig::default());
        let session_id = SessionId::new();
        let (tx, mut rx) = mpsc::channel(1);
        server.register_connection(session_id, tx, None);

        // Simulate an intermittently-slow client: accumulate drops just under
        // the consecutive threshold, then drain and succeed to reset the
        // consecutive counter. Repeat until total_drops crosses the lifetime
        // threshold.
        let batches_needed = MAX_TOTAL_DROPS / (MAX_CONSECUTIVE_DROPS - 1) + 1;
        for batch in 0..batches_needed {
            assert!(
                server
                    .try_send_to_session(session_id, RealtimeMessage::Lagging)
                    .is_ok(),
                "batch {batch}: initial send should succeed"
            );

            for _ in 0..(MAX_CONSECUTIVE_DROPS - 1) {
                let r = server.try_send_to_session(session_id, RealtimeMessage::Lagging);
                match r {
                    Err(SendError::Full) => {}
                    Err(SendError::Evicted) => {
                        assert_eq!(server.connection_count(), 0);
                        return;
                    }
                    other => panic!("unexpected result in batch {batch}: {other:?}"),
                }
            }

            // Drain the buffer so the next batch's initial send can succeed,
            // resetting the consecutive counter but leaving total_drops to grow.
            let _ = rx.recv().await;
        }
        panic!("session should have been evicted by total_drops");
    }

    #[test]
    fn remove_connection_purges_subscription_mappings() {
        let server = SessionServer::new(NodeId::new(), RealtimeConfig::default());
        let session_id = SessionId::new();
        let sub_a = SubscriptionId::new();
        let sub_b = SubscriptionId::new();
        let (tx, _rx) = mpsc::channel(8);

        server.register_connection(session_id, tx, None);
        server.add_subscription(session_id, sub_a).unwrap();
        server.add_subscription(session_id, sub_b).unwrap();
        assert_eq!(server.subscription_count(), 2);

        let removed = server.remove_connection(session_id).unwrap();
        assert_eq!(removed.len(), 2);
        assert_eq!(server.subscription_count(), 0);
        // The reverse map is fully cleared.
        assert!(server.remove_connection(session_id).is_none());
    }

    #[test]
    fn add_subscription_to_unknown_session_errors() {
        let server = SessionServer::new(NodeId::new(), RealtimeConfig::default());
        let result = server.add_subscription(SessionId::new(), SubscriptionId::new());
        assert!(result.is_err());
        assert_eq!(server.subscription_count(), 0);
    }

    #[test]
    fn remove_unknown_subscription_is_noop() {
        let server = SessionServer::new(NodeId::new(), RealtimeConfig::default());
        // No panic, no state change.
        server.remove_subscription(SubscriptionId::new());
        assert_eq!(server.subscription_count(), 0);
    }

    #[tokio::test]
    async fn broadcast_delta_routes_to_subscribed_session() {
        let server = SessionServer::new(NodeId::new(), RealtimeConfig::default());
        let session_id = SessionId::new();
        let sub_id = SubscriptionId::new();
        let (tx, mut rx) = mpsc::channel(8);

        server.register_connection(session_id, tx, None);
        server.add_subscription(session_id, sub_id).unwrap();

        let mut delta = Delta::empty();
        delta.added.push(serde_json::json!({"hello": "world"}));
        server.broadcast_delta(sub_id, delta).await.unwrap();

        match rx.recv().await {
            Some(RealtimeMessage::DeltaUpdate {
                subscription_id, ..
            }) => {
                assert_eq!(subscription_id, sub_id.to_string());
            }
            other => panic!("expected DeltaUpdate, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn broadcast_delta_without_subscription_is_noop() {
        let server = SessionServer::new(NodeId::new(), RealtimeConfig::default());
        // No matching subscription -> Ok(()) and no panic.
        let result = server
            .broadcast_delta(SubscriptionId::new(), Delta::empty())
            .await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn send_to_session_delivers_message() {
        let server = SessionServer::new(NodeId::new(), RealtimeConfig::default());
        let session_id = SessionId::new();
        let (tx, mut rx) = mpsc::channel(8);
        server.register_connection(session_id, tx, None);

        server
            .send_to_session(
                session_id,
                RealtimeMessage::AuthFailed {
                    reason: "test".to_string(),
                },
            )
            .await
            .unwrap();

        match rx.recv().await {
            Some(RealtimeMessage::AuthFailed { reason }) => assert_eq!(reason, "test"),
            other => panic!("expected AuthFailed, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn send_to_unknown_session_errors() {
        let server = SessionServer::new(NodeId::new(), RealtimeConfig::default());
        let result = server
            .send_to_session(SessionId::new(), RealtimeMessage::Lagging)
            .await;
        assert!(result.is_err());
    }

    #[test]
    fn cleanup_stale_evicts_only_idle_sessions() {
        let server = SessionServer::new(NodeId::new(), RealtimeConfig::default());

        let idle = SessionId::new();
        let active = SessionId::new();
        let (tx1, _rx1) = mpsc::channel(8);
        let (tx2, _rx2) = mpsc::channel(8);

        // Both registered "now"; we'll then backdate `idle.last_active` so it
        // crosses the cleanup cutoff.
        server.register_connection(idle, tx1, None);
        server.register_connection(active, tx2, None);

        // Reach into the entry and rewrite last_active to two hours ago.
        let two_hours_ago = chrono::Utc::now().timestamp() - 7200;
        {
            let entry = server.connections.get(&idle).unwrap();
            entry.last_active.store(two_hours_ago, Ordering::Relaxed);
        }

        // Cutoff = now - 1 hour. `idle` (-2h) is stale; `active` (now) is fresh.
        server.cleanup_stale(Duration::from_secs(3600));

        assert_eq!(server.connection_count(), 1);
        assert!(server.connections.contains_key(&active));
        assert!(!server.connections.contains_key(&idle));
    }

    #[test]
    fn cleanup_expired_tokens_evicts_and_notifies() {
        let server = SessionServer::new(NodeId::new(), RealtimeConfig::default());

        let expired = SessionId::new();
        let valid = SessionId::new();
        let (tx_expired, mut rx_expired) = mpsc::channel(8);
        let (tx_valid, _rx_valid) = mpsc::channel(8);

        // Token expired one second after the Unix epoch (clearly in the past).
        server.register_connection(expired, tx_expired, Some(1));
        // Token good for another hour.
        server.register_connection(valid, tx_valid, Some(chrono::Utc::now().timestamp() + 3600));

        let evicted = server.cleanup_expired_tokens();

        // Only the expired session was evicted.
        assert_eq!(server.connection_count(), 1);
        assert!(server.connections.contains_key(&valid));
        // Returned list contains exactly the evicted session.
        assert_eq!(evicted, vec![expired]);

        // The expired client received an AuthFailed notification before eviction.
        match rx_expired.try_recv() {
            Ok(RealtimeMessage::AuthFailed { reason }) => {
                assert!(reason.contains("expired"), "unexpected reason: {reason}");
            }
            other => panic!("expected AuthFailed, got {other:?}"),
        }
    }

    #[test]
    fn cleanup_expired_tokens_skips_unauthenticated_sessions() {
        let server = SessionServer::new(NodeId::new(), RealtimeConfig::default());
        let session_id = SessionId::new();
        let (tx, _rx) = mpsc::channel(8);
        server.register_connection(session_id, tx, None);

        let evicted = server.cleanup_expired_tokens();

        assert_eq!(server.connection_count(), 1);
        assert!(evicted.is_empty());
    }

    #[tokio::test]
    async fn revoke_session_notifies_then_evicts_connection_and_subscriptions() {
        // Server-side auth revocation path: client gets one final AuthFailed
        // message, the connection is removed, and the subscription mappings
        // are returned for caller cleanup. After revocation the session must
        // be unreachable — any send returns SessionNotFound, forcing the
        // client to reconnect with a fresh token.
        let server = SessionServer::new(NodeId::new(), RealtimeConfig::default());
        let session_id = SessionId::new();
        let sub_a = SubscriptionId::new();
        let sub_b = SubscriptionId::new();
        let (tx, mut rx) = mpsc::channel(8);

        server.register_connection(session_id, tx, None);
        server.add_subscription(session_id, sub_a).unwrap();
        server.add_subscription(session_id, sub_b).unwrap();

        let removed = server
            .revoke_session(session_id, "role demoted")
            .expect("session existed");
        assert_eq!(removed.len(), 2);
        assert!(removed.contains(&sub_a));
        assert!(removed.contains(&sub_b));

        match rx.recv().await {
            Some(RealtimeMessage::AuthFailed { reason }) => assert_eq!(reason, "role demoted"),
            other => panic!("expected AuthFailed, got {other:?}"),
        }

        assert_eq!(server.connection_count(), 0);
        assert_eq!(server.subscription_count(), 0);
        let result = server.try_send_to_session(session_id, RealtimeMessage::Lagging);
        assert!(matches!(result, Err(SendError::SessionNotFound)));

        // Calling revoke again on a gone session is a no-op.
        assert!(server.revoke_session(session_id, "again").is_none());
    }

    #[test]
    fn cleanup_expired_tokens_returns_empty_when_nothing_expired() {
        let server = SessionServer::new(NodeId::new(), RealtimeConfig::default());
        let session_id = SessionId::new();
        let (tx, _rx) = mpsc::channel(8);
        // Token good for another hour — nothing should expire.
        server.register_connection(session_id, tx, Some(chrono::Utc::now().timestamp() + 3600));

        let evicted = server.cleanup_expired_tokens();

        assert_eq!(server.connection_count(), 1);
        assert!(evicted.is_empty());
    }
}

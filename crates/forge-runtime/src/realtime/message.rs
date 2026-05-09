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

/// Message types for real-time communication.
///
/// `#[non_exhaustive]` so 1.0.x can add new variants without breaking
/// downstream Rust matchers (forge-dioxus, custom integrations).
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum RealtimeMessage {
    Subscribe {
        id: String,
        query: String,
        args: serde_json::Value,
    },
    Unsubscribe {
        subscription_id: SubscriptionId,
    },
    Ping,
    Pong,
    Data {
        subscription_id: String,
        data: serde_json::Value,
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
    Error {
        code: String,
        message: String,
    },
    ErrorWithId {
        id: String,
        code: String,
        message: String,
    },
    AuthSuccess,
    AuthFailed {
        reason: String,
    },
    /// Sent to slow clients before disconnecting them.
    Lagging,
    /// Ephemeral pub-sub fan-out (forge_channels). The variant is reserved
    /// for GA; the publish/subscribe pipeline lands in 1.0.x.
    Channel {
        channel: String,
        payload: serde_json::Value,
    },
    /// Server detected a dropped or out-of-order delivery for a subscription
    /// and asks the client to resync via `last-event-id`. Reserved for GA;
    /// emission rules land in 1.0.x.
    GapDetected {
        client_sub_id: String,
        last_event_id: Option<String>,
    },
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
    /// JWT expiry as Unix timestamp. `None` for unauthenticated (anonymous) sessions.
    token_exp: Option<i64>,
}

/// Maximum consecutive drops before evicting a slow client.
const MAX_CONSECUTIVE_DROPS: u32 = 10;

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
            token_exp,
        };
        self.connections.insert(session_id, entry);
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
                let drops = conn.consecutive_drops.fetch_add(1, Ordering::Relaxed);
                if drops >= MAX_CONSECUTIVE_DROPS {
                    // Try to send lagging notification before evicting
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
            .map_err(|_| forge_core::ForgeError::Internal("Failed to send message".to_string()))
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
            - chrono::Duration::from_std(max_idle).unwrap_or(chrono::TimeDelta::MAX))
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
            self.remove_connection(session_id);
        }
    }

    /// Evict sessions whose JWT has expired. Sends an auth error event to each
    /// so the client knows to re-authenticate rather than just reconnecting.
    pub fn cleanup_expired_tokens(&self) {
        let now = chrono::Utc::now().timestamp();

        let expired: Vec<SessionId> = self
            .connections
            .iter()
            .filter(|entry| entry.token_exp.is_some_and(|exp| exp < now))
            .map(|entry| *entry.key())
            .collect();

        if expired.is_empty() {
            return;
        }

        tracing::debug!(count = expired.len(), "Evicting sessions with expired tokens");

        for session_id in expired {
            if let Some(conn) = self.connections.get(&session_id) {
                let _ = conn.sender.try_send(RealtimeMessage::AuthFailed {
                    reason: "Token expired".to_string(),
                });
            }
            self.evict_session(session_id);
        }
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
        let result = server.try_send_to_session(session_id, RealtimeMessage::Ping);
        assert!(result.is_ok());

        // Second send to full buffer should return Full
        let result = server.try_send_to_session(session_id, RealtimeMessage::Ping);
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

        let result = server.try_send_to_session(session_id, RealtimeMessage::Ping);

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

        let result = server.try_send_to_session(session_id, RealtimeMessage::Ping);

        assert!(result.is_ok());
        assert_eq!(server.connection_count(), 1);
    }
}

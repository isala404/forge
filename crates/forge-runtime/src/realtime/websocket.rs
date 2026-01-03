use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, RwLock};

use forge_core::cluster::NodeId;
use forge_core::realtime::{Delta, SessionId, SubscriptionId};

use crate::gateway::websocket::{JobData, WorkflowData};

/// WebSocket server configuration.
#[derive(Debug, Clone)]
pub struct WebSocketConfig {
    /// Maximum subscriptions per connection.
    pub max_subscriptions_per_connection: usize,
    /// Subscription timeout.
    pub subscription_timeout: Duration,
    /// Rate limit for subscription creation (per minute).
    pub subscription_rate_limit: usize,
    /// Heartbeat interval for keepalive.
    pub heartbeat_interval: Duration,
    /// Maximum message size in bytes.
    pub max_message_size: usize,
    /// Reconnect settings.
    pub reconnect: ReconnectConfig,
}

impl Default for WebSocketConfig {
    fn default() -> Self {
        Self {
            max_subscriptions_per_connection: 50,
            subscription_timeout: Duration::from_secs(30),
            subscription_rate_limit: 100,
            heartbeat_interval: Duration::from_secs(30),
            max_message_size: 1024 * 1024, // 1MB
            reconnect: ReconnectConfig::default(),
        }
    }
}

/// Reconnection configuration.
#[derive(Debug, Clone)]
pub struct ReconnectConfig {
    /// Whether reconnection is enabled.
    pub enabled: bool,
    /// Maximum reconnection attempts.
    pub max_attempts: usize,
    /// Initial delay between attempts.
    pub delay: Duration,
    /// Maximum delay between attempts.
    pub max_delay: Duration,
    /// Backoff strategy.
    pub backoff: BackoffStrategy,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            max_attempts: 10,
            delay: Duration::from_secs(1),
            max_delay: Duration::from_secs(30),
            backoff: BackoffStrategy::Exponential,
        }
    }
}

/// Backoff strategy for reconnection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackoffStrategy {
    /// Linear backoff.
    Linear,
    /// Exponential backoff.
    Exponential,
    /// Fixed delay.
    Fixed,
}

/// Message types for WebSocket communication.
#[derive(Debug, Clone)]
pub enum WebSocketMessage {
    /// Subscribe to a query.
    Subscribe {
        id: String,
        query: String,
        args: serde_json::Value,
    },
    /// Unsubscribe from a subscription.
    Unsubscribe { subscription_id: SubscriptionId },
    /// Ping for keepalive.
    Ping,
    /// Pong response.
    Pong,
    /// Initial data for subscription.
    Data {
        subscription_id: SubscriptionId,
        data: serde_json::Value,
    },
    /// Delta update for subscription.
    DeltaUpdate {
        subscription_id: SubscriptionId,
        delta: Delta<serde_json::Value>,
    },
    /// Job progress update.
    JobUpdate { client_sub_id: String, job: JobData },
    /// Workflow progress update.
    WorkflowUpdate {
        client_sub_id: String,
        workflow: WorkflowData,
    },
    /// Error message.
    Error { code: String, message: String },
    /// Error message with subscription ID.
    ErrorWithId {
        id: String,
        code: String,
        message: String,
    },
}

/// Represents a connected WebSocket client.
#[derive(Debug)]
pub struct WebSocketConnection {
    /// Session ID for this connection.
    #[allow(dead_code)]
    pub session_id: SessionId,
    /// Active subscriptions.
    pub subscriptions: Vec<SubscriptionId>,
    /// Sender for outgoing messages.
    pub sender: mpsc::Sender<WebSocketMessage>,
    /// When the connection was established.
    #[allow(dead_code)]
    pub connected_at: chrono::DateTime<chrono::Utc>,
    /// Last activity time.
    pub last_active: chrono::DateTime<chrono::Utc>,
}

impl WebSocketConnection {
    /// Create a new connection.
    pub fn new(session_id: SessionId, sender: mpsc::Sender<WebSocketMessage>) -> Self {
        let now = chrono::Utc::now();
        Self {
            session_id,
            subscriptions: Vec::new(),
            sender,
            connected_at: now,
            last_active: now,
        }
    }

    /// Add a subscription.
    pub fn add_subscription(&mut self, subscription_id: SubscriptionId) {
        self.subscriptions.push(subscription_id);
        self.last_active = chrono::Utc::now();
    }

    /// Remove a subscription.
    pub fn remove_subscription(&mut self, subscription_id: SubscriptionId) {
        self.subscriptions.retain(|id| *id != subscription_id);
        self.last_active = chrono::Utc::now();
    }

    /// Send a message to the client.
    pub async fn send(
        &self,
        message: WebSocketMessage,
    ) -> Result<(), mpsc::error::SendError<WebSocketMessage>> {
        self.sender.send(message).await
    }
}

/// WebSocket server for managing real-time connections.
pub struct WebSocketServer {
    #[allow(dead_code)]
    config: WebSocketConfig,
    node_id: NodeId,
    /// Active connections by session ID.
    connections: Arc<RwLock<HashMap<SessionId, WebSocketConnection>>>,
    /// Subscription to session mapping for fast lookup.
    subscription_sessions: Arc<RwLock<HashMap<SubscriptionId, SessionId>>>,
}

impl WebSocketServer {
    /// Create a new WebSocket server.
    pub fn new(node_id: NodeId, config: WebSocketConfig) -> Self {
        Self {
            config,
            node_id,
            connections: Arc::new(RwLock::new(HashMap::new())),
            subscription_sessions: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Get the node ID.
    pub fn node_id(&self) -> NodeId {
        self.node_id
    }

    /// Get the configuration.
    pub fn config(&self) -> &WebSocketConfig {
        &self.config
    }

    /// Register a new connection.
    pub async fn register_connection(
        &self,
        session_id: SessionId,
        sender: mpsc::Sender<WebSocketMessage>,
    ) {
        let connection = WebSocketConnection::new(session_id, sender);
        let mut connections = self.connections.write().await;
        connections.insert(session_id, connection);
    }

    /// Remove a connection.
    pub async fn remove_connection(&self, session_id: SessionId) -> Option<Vec<SubscriptionId>> {
        let mut connections = self.connections.write().await;
        if let Some(conn) = connections.remove(&session_id) {
            // Clean up subscription mappings
            let mut sub_sessions = self.subscription_sessions.write().await;
            for sub_id in &conn.subscriptions {
                sub_sessions.remove(sub_id);
            }
            Some(conn.subscriptions)
        } else {
            None
        }
    }

    /// Add a subscription to a connection.
    pub async fn add_subscription(
        &self,
        session_id: SessionId,
        subscription_id: SubscriptionId,
    ) -> forge_core::Result<()> {
        let mut connections = self.connections.write().await;
        let conn = connections
            .get_mut(&session_id)
            .ok_or_else(|| forge_core::ForgeError::Validation("Session not found".to_string()))?;

        // Check subscription limit
        if conn.subscriptions.len() >= self.config.max_subscriptions_per_connection {
            return Err(forge_core::ForgeError::Validation(format!(
                "Maximum subscriptions per connection ({}) exceeded",
                self.config.max_subscriptions_per_connection
            )));
        }

        conn.add_subscription(subscription_id);

        // Update subscription to session mapping
        let mut sub_sessions = self.subscription_sessions.write().await;
        sub_sessions.insert(subscription_id, session_id);

        Ok(())
    }

    /// Remove a subscription from a connection.
    pub async fn remove_subscription(&self, subscription_id: SubscriptionId) {
        let session_id = {
            let mut sub_sessions = self.subscription_sessions.write().await;
            sub_sessions.remove(&subscription_id)
        };

        if let Some(session_id) = session_id {
            let mut connections = self.connections.write().await;
            if let Some(conn) = connections.get_mut(&session_id) {
                conn.remove_subscription(subscription_id);
            }
        }
    }

    /// Send a message to a specific session.
    pub async fn send_to_session(
        &self,
        session_id: SessionId,
        message: WebSocketMessage,
    ) -> forge_core::Result<()> {
        let connections = self.connections.read().await;
        let conn = connections
            .get(&session_id)
            .ok_or_else(|| forge_core::ForgeError::Validation("Session not found".to_string()))?;

        conn.send(message)
            .await
            .map_err(|_| forge_core::ForgeError::Internal("Failed to send message".to_string()))
    }

    /// Send a delta to all sessions subscribed to a subscription.
    pub async fn broadcast_delta(
        &self,
        subscription_id: SubscriptionId,
        delta: Delta<serde_json::Value>,
    ) -> forge_core::Result<()> {
        let session_id = {
            let sub_sessions = self.subscription_sessions.read().await;
            sub_sessions.get(&subscription_id).copied()
        };

        if let Some(session_id) = session_id {
            let message = WebSocketMessage::DeltaUpdate {
                subscription_id,
                delta,
            };
            self.send_to_session(session_id, message).await?;
        }

        Ok(())
    }

    /// Get connection count.
    pub async fn connection_count(&self) -> usize {
        self.connections.read().await.len()
    }

    /// Get subscription count.
    pub async fn subscription_count(&self) -> usize {
        self.subscription_sessions.read().await.len()
    }

    /// Get server statistics.
    pub async fn stats(&self) -> WebSocketStats {
        let connections = self.connections.read().await;
        let total_subscriptions: usize = connections.values().map(|c| c.subscriptions.len()).sum();

        WebSocketStats {
            connections: connections.len(),
            subscriptions: total_subscriptions,
            node_id: self.node_id,
        }
    }

    /// Cleanup stale connections.
    pub async fn cleanup_stale(&self, max_idle: Duration) {
        let cutoff = chrono::Utc::now() - chrono::Duration::from_std(max_idle).unwrap();
        let mut connections = self.connections.write().await;
        let mut sub_sessions = self.subscription_sessions.write().await;

        connections.retain(|_, conn| {
            if conn.last_active < cutoff {
                // Clean up subscription mappings
                for sub_id in &conn.subscriptions {
                    sub_sessions.remove(sub_id);
                }
                false
            } else {
                true
            }
        });
    }
}

/// WebSocket server statistics.
#[derive(Debug, Clone)]
pub struct WebSocketStats {
    /// Number of active connections.
    pub connections: usize,
    /// Total subscriptions across all connections.
    pub subscriptions: usize,
    /// Node ID.
    pub node_id: NodeId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_websocket_config_default() {
        let config = WebSocketConfig::default();
        assert_eq!(config.max_subscriptions_per_connection, 50);
        assert_eq!(config.subscription_rate_limit, 100);
        assert!(config.reconnect.enabled);
    }

    #[test]
    fn test_reconnect_config_default() {
        let config = ReconnectConfig::default();
        assert!(config.enabled);
        assert_eq!(config.max_attempts, 10);
        assert_eq!(config.backoff, BackoffStrategy::Exponential);
    }

    #[tokio::test]
    async fn test_websocket_server_creation() {
        let node_id = NodeId::new();
        let server = WebSocketServer::new(node_id, WebSocketConfig::default());

        assert_eq!(server.node_id(), node_id);
        assert_eq!(server.connection_count().await, 0);
        assert_eq!(server.subscription_count().await, 0);
    }

    #[tokio::test]
    async fn test_websocket_connection() {
        let node_id = NodeId::new();
        let server = WebSocketServer::new(node_id, WebSocketConfig::default());
        let session_id = SessionId::new();
        let (tx, _rx) = mpsc::channel(100);

        server.register_connection(session_id, tx).await;
        assert_eq!(server.connection_count().await, 1);

        let removed = server.remove_connection(session_id).await;
        assert!(removed.is_some());
        assert_eq!(server.connection_count().await, 0);
    }

    #[tokio::test]
    async fn test_websocket_subscription() {
        let node_id = NodeId::new();
        let server = WebSocketServer::new(node_id, WebSocketConfig::default());
        let session_id = SessionId::new();
        let subscription_id = SubscriptionId::new();
        let (tx, _rx) = mpsc::channel(100);

        server.register_connection(session_id, tx).await;
        server
            .add_subscription(session_id, subscription_id)
            .await
            .unwrap();

        assert_eq!(server.subscription_count().await, 1);

        server.remove_subscription(subscription_id).await;
        assert_eq!(server.subscription_count().await, 0);
    }

    #[tokio::test]
    async fn test_websocket_subscription_limit() {
        let node_id = NodeId::new();
        let config = WebSocketConfig {
            max_subscriptions_per_connection: 2,
            ..Default::default()
        };
        let server = WebSocketServer::new(node_id, config);
        let session_id = SessionId::new();
        let (tx, _rx) = mpsc::channel(100);

        server.register_connection(session_id, tx).await;

        // First two should succeed
        server
            .add_subscription(session_id, SubscriptionId::new())
            .await
            .unwrap();
        server
            .add_subscription(session_id, SubscriptionId::new())
            .await
            .unwrap();

        // Third should fail
        let result = server
            .add_subscription(session_id, SubscriptionId::new())
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_websocket_stats() {
        let node_id = NodeId::new();
        let server = WebSocketServer::new(node_id, WebSocketConfig::default());
        let session_id = SessionId::new();
        let (tx, _rx) = mpsc::channel(100);

        server.register_connection(session_id, tx).await;
        server
            .add_subscription(session_id, SubscriptionId::new())
            .await
            .unwrap();
        server
            .add_subscription(session_id, SubscriptionId::new())
            .await
            .unwrap();

        let stats = server.stats().await;
        assert_eq!(stats.connections, 1);
        assert_eq!(stats.subscriptions, 2);
        assert_eq!(stats.node_id, node_id);
    }
}

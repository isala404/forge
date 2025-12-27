use std::str::FromStr;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::cluster::NodeId;

/// Unique session identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(pub Uuid);

impl SessionId {
    /// Generate a new random session ID.
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

impl Default for SessionId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SessionId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Session status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionStatus {
    /// Session is connecting.
    Connecting,
    /// Session is connected and active.
    Connected,
    /// Session is reconnecting.
    Reconnecting,
    /// Session is disconnected.
    Disconnected,
}

impl SessionStatus {
    /// Convert to string.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Connecting => "connecting",
            Self::Connected => "connected",
            Self::Reconnecting => "reconnecting",
            Self::Disconnected => "disconnected",
        }
    }
}

impl FromStr for SessionStatus {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_str() {
            "connecting" => Self::Connecting,
            "connected" => Self::Connected,
            "reconnecting" => Self::Reconnecting,
            "disconnected" => Self::Disconnected,
            _ => Self::Disconnected,
        })
    }
}

/// Information about a WebSocket session.
#[derive(Debug, Clone)]
pub struct SessionInfo {
    /// Unique session ID.
    pub id: SessionId,
    /// Node hosting this session.
    pub node_id: NodeId,
    /// User ID if authenticated.
    pub user_id: Option<String>,
    /// Current status.
    pub status: SessionStatus,
    /// Number of active subscriptions.
    pub subscription_count: u32,
    /// When the session was created.
    pub created_at: DateTime<Utc>,
    /// When the session was last active.
    pub last_active_at: DateTime<Utc>,
    /// Client IP address.
    pub client_ip: Option<String>,
    /// User agent string.
    pub user_agent: Option<String>,
}

impl SessionInfo {
    /// Create a new session info.
    pub fn new(node_id: NodeId) -> Self {
        let now = Utc::now();
        Self {
            id: SessionId::new(),
            node_id,
            user_id: None,
            status: SessionStatus::Connecting,
            subscription_count: 0,
            created_at: now,
            last_active_at: now,
            client_ip: None,
            user_agent: None,
        }
    }

    /// Set user ID after authentication.
    pub fn with_user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
    }

    /// Set client metadata.
    pub fn with_client_info(
        mut self,
        client_ip: Option<String>,
        user_agent: Option<String>,
    ) -> Self {
        self.client_ip = client_ip;
        self.user_agent = user_agent;
        self
    }

    /// Mark session as connected.
    pub fn connect(&mut self) {
        self.status = SessionStatus::Connected;
        self.last_active_at = Utc::now();
    }

    /// Mark session as disconnected.
    pub fn disconnect(&mut self) {
        self.status = SessionStatus::Disconnected;
        self.last_active_at = Utc::now();
    }

    /// Mark session as reconnecting.
    pub fn reconnecting(&mut self) {
        self.status = SessionStatus::Reconnecting;
        self.last_active_at = Utc::now();
    }

    /// Update last activity time.
    pub fn touch(&mut self) {
        self.last_active_at = Utc::now();
    }

    /// Check if session is connected.
    pub fn is_connected(&self) -> bool {
        matches!(self.status, SessionStatus::Connected)
    }

    /// Check if session is authenticated.
    pub fn is_authenticated(&self) -> bool {
        self.user_id.is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_session_id_generation() {
        let id1 = SessionId::new();
        let id2 = SessionId::new();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_session_status_conversion() {
        assert_eq!(
            "connected".parse::<SessionStatus>(),
            Ok(SessionStatus::Connected)
        );
        assert_eq!(
            "disconnected".parse::<SessionStatus>(),
            Ok(SessionStatus::Disconnected)
        );
        assert_eq!(SessionStatus::Connected.as_str(), "connected");
    }

    #[test]
    fn test_session_info_creation() {
        let node_id = NodeId::new();
        let session = SessionInfo::new(node_id);

        assert_eq!(session.status, SessionStatus::Connecting);
        assert!(!session.is_connected());
        assert!(!session.is_authenticated());
    }

    #[test]
    fn test_session_lifecycle() {
        let node_id = NodeId::new();
        let mut session = SessionInfo::new(node_id);

        // Connect
        session.connect();
        assert!(session.is_connected());

        // Authenticate
        session = session.with_user_id("user123");
        assert!(session.is_authenticated());

        // Disconnect
        session.disconnect();
        assert!(!session.is_connected());
    }
}

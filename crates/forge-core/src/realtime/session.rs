use std::str::FromStr;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::cluster::NodeId;

/// Unique session identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct SessionId(pub Uuid);

impl SessionId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn from_uuid(id: Uuid) -> Self {
        Self(id)
    }

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
#[non_exhaustive]
pub enum SessionStatus {
    Connecting,
    Connected,
    Reconnecting,
    Disconnected,
}

impl SessionStatus {
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
    pub id: SessionId,
    pub node_id: NodeId,
    pub user_id: Option<String>,
    pub status: SessionStatus,
    pub subscription_count: u32,
    pub created_at: DateTime<Utc>,
    pub last_active_at: DateTime<Utc>,
    pub client_ip: Option<String>,
    pub user_agent: Option<String>,
}

impl SessionInfo {
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

    pub fn with_user_id(mut self, user_id: impl Into<String>) -> Self {
        self.user_id = Some(user_id.into());
        self
    }

    pub fn with_client_info(
        mut self,
        client_ip: Option<String>,
        user_agent: Option<String>,
    ) -> Self {
        self.client_ip = client_ip;
        self.user_agent = user_agent;
        self
    }

    pub fn connect(&mut self) {
        self.status = SessionStatus::Connected;
        self.last_active_at = Utc::now();
    }

    pub fn disconnect(&mut self) {
        self.status = SessionStatus::Disconnected;
        self.last_active_at = Utc::now();
    }

    pub fn reconnecting(&mut self) {
        self.status = SessionStatus::Reconnecting;
        self.last_active_at = Utc::now();
    }

    pub fn touch(&mut self) {
        self.last_active_at = Utc::now();
    }

    pub fn is_connected(&self) -> bool {
        matches!(self.status, SessionStatus::Connected)
    }

    pub fn is_authenticated(&self) -> bool {
        self.user_id.is_some()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
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

        session.connect();
        assert!(session.is_connected());

        session = session.with_user_id("user123");
        assert!(session.is_authenticated());

        session.disconnect();
        assert!(!session.is_connected());
    }
}

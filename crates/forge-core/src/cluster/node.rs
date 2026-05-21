use std::net::IpAddr;
use std::str::FromStr;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::roles::NodeRole;

/// Unique node identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub Uuid);

impl NodeId {
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

impl Default for NodeId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for NodeId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Node status in the cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum NodeStatus {
    /// Node is starting up.
    Joining,
    /// Node is healthy and active.
    Active,
    /// Node is shutting down gracefully.
    Draining,
    /// Node has stopped sending heartbeats.
    Dead,
}

impl NodeStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Joining => "joining",
            Self::Active => "active",
            Self::Draining => "draining",
            Self::Dead => "dead",
        }
    }

    pub fn can_accept_work(&self) -> bool {
        matches!(self, Self::Active)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseNodeStatusError(pub String);

impl std::fmt::Display for ParseNodeStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid node status: '{}'", self.0)
    }
}

impl std::error::Error for ParseNodeStatusError {}

impl FromStr for NodeStatus {
    type Err = ParseNodeStatusError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "joining" => Ok(Self::Joining),
            "active" => Ok(Self::Active),
            "draining" => Ok(Self::Draining),
            "dead" => Ok(Self::Dead),
            _ => Err(ParseNodeStatusError(s.to_string())),
        }
    }
}

/// Information about a node in the cluster.
#[derive(Debug, Clone)]
pub struct NodeInfo {
    pub id: NodeId,
    pub hostname: String,
    pub ip_address: IpAddr,
    pub http_port: u16,
    pub grpc_port: u16,
    pub roles: Vec<NodeRole>,
    pub worker_capabilities: Vec<String>,
    pub status: NodeStatus,
    pub last_heartbeat: DateTime<Utc>,
    pub version: String,
    pub started_at: DateTime<Utc>,
    pub current_connections: u32,
    pub current_jobs: u32,
    pub cpu_usage: f32,
    pub memory_usage: f32,
}

impl NodeInfo {
    pub fn new_local(
        hostname: String,
        ip_address: IpAddr,
        http_port: u16,
        grpc_port: u16,
        roles: Vec<NodeRole>,
        worker_capabilities: Vec<String>,
        version: String,
    ) -> Self {
        Self {
            id: NodeId::new(),
            hostname,
            ip_address,
            http_port,
            grpc_port,
            roles,
            worker_capabilities,
            status: NodeStatus::Joining,
            last_heartbeat: Utc::now(),
            version,
            started_at: Utc::now(),
            current_connections: 0,
            current_jobs: 0,
            cpu_usage: 0.0,
            memory_usage: 0.0,
        }
    }

    pub fn has_role(&self, role: NodeRole) -> bool {
        self.roles.contains(&role)
    }

    pub fn has_capability(&self, capability: &str) -> bool {
        self.worker_capabilities.iter().any(|c| c == capability)
    }

    /// Average of CPU and memory, normalized to 0.0–1.0.
    pub fn load(&self) -> f32 {
        (self.cpu_usage + self.memory_usage) / 2.0 / 100.0
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn test_node_id_generation() {
        let id1 = NodeId::new();
        let id2 = NodeId::new();
        assert_ne!(id1, id2);
    }

    #[test]
    fn test_node_status_conversion() {
        assert_eq!("active".parse::<NodeStatus>(), Ok(NodeStatus::Active));
        assert_eq!("draining".parse::<NodeStatus>(), Ok(NodeStatus::Draining));
        assert!("invalid".parse::<NodeStatus>().is_err());
        assert_eq!(NodeStatus::Active.as_str(), "active");
    }

    #[test]
    fn test_node_can_accept_work() {
        assert!(NodeStatus::Active.can_accept_work());
        assert!(!NodeStatus::Draining.can_accept_work());
        assert!(!NodeStatus::Dead.can_accept_work());
    }

    #[test]
    fn test_node_info_creation() {
        let info = NodeInfo::new_local(
            "test-node".to_string(),
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            8080,
            9000,
            vec![NodeRole::Gateway, NodeRole::Worker],
            vec!["general".to_string()],
            "0.1.0".to_string(),
        );

        assert!(info.has_role(NodeRole::Gateway));
        assert!(info.has_role(NodeRole::Worker));
        assert!(!info.has_role(NodeRole::Scheduler));
        assert!(info.has_capability("general"));
        assert!(!info.has_capability("media"));
    }

    #[test]
    fn test_node_load_calculation() {
        let mut info = NodeInfo::new_local(
            "test".to_string(),
            IpAddr::V4(Ipv4Addr::new(127, 0, 0, 1)),
            8080,
            9000,
            vec![],
            vec![],
            "0.1.0".to_string(),
        );
        info.cpu_usage = 50.0;
        info.memory_usage = 30.0;
        assert!((info.load() - 0.4).abs() < 0.001);
    }
}

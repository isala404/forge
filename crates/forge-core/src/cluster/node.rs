use std::net::IpAddr;

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::roles::NodeRole;

/// Unique node identifier.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct NodeId(pub Uuid);

impl NodeId {
    /// Generate a new random node ID.
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
    /// Convert to string for database storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Joining => "joining",
            Self::Active => "active",
            Self::Draining => "draining",
            Self::Dead => "dead",
        }
    }

    /// Parse from string.
    pub fn from_str(s: &str) -> Self {
        match s {
            "joining" => Self::Joining,
            "active" => Self::Active,
            "draining" => Self::Draining,
            "dead" => Self::Dead,
            _ => Self::Dead,
        }
    }

    /// Check if node can accept new work.
    pub fn can_accept_work(&self) -> bool {
        matches!(self, Self::Active)
    }
}

/// Information about a node in the cluster.
#[derive(Debug, Clone)]
pub struct NodeInfo {
    /// Unique node ID.
    pub id: NodeId,
    /// Hostname.
    pub hostname: String,
    /// IP address.
    pub ip_address: IpAddr,
    /// HTTP port.
    pub http_port: u16,
    /// gRPC port for inter-node communication.
    pub grpc_port: u16,
    /// Enabled roles.
    pub roles: Vec<NodeRole>,
    /// Worker capabilities.
    pub worker_capabilities: Vec<String>,
    /// Current status.
    pub status: NodeStatus,
    /// Last heartbeat time.
    pub last_heartbeat: DateTime<Utc>,
    /// Version string.
    pub version: String,
    /// When the node started.
    pub started_at: DateTime<Utc>,
    /// Current connection count.
    pub current_connections: u32,
    /// Current job count.
    pub current_jobs: u32,
    /// CPU usage percentage.
    pub cpu_usage: f32,
    /// Memory usage percentage.
    pub memory_usage: f32,
}

impl NodeInfo {
    /// Create a new node info for the local node.
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

    /// Check if this node has a specific role.
    pub fn has_role(&self, role: NodeRole) -> bool {
        self.roles.contains(&role)
    }

    /// Check if this node has a specific worker capability.
    pub fn has_capability(&self, capability: &str) -> bool {
        self.worker_capabilities.iter().any(|c| c == capability)
    }

    /// Calculate node load (0.0 to 1.0).
    pub fn load(&self) -> f32 {
        // Simple average of CPU and memory
        (self.cpu_usage + self.memory_usage) / 2.0 / 100.0
    }
}

#[cfg(test)]
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
        assert_eq!(NodeStatus::from_str("active"), NodeStatus::Active);
        assert_eq!(NodeStatus::from_str("draining"), NodeStatus::Draining);
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

use super::node::{NodeId, NodeInfo};
use super::roles::LeaderRole;

/// Information about the cluster.
#[derive(Debug, Clone)]
pub struct ClusterInfo {
    /// Cluster name.
    pub name: String,
    /// Total number of registered nodes.
    pub total_nodes: usize,
    /// Number of active nodes.
    pub active_nodes: usize,
    /// Number of draining nodes.
    pub draining_nodes: usize,
    /// Number of dead nodes.
    pub dead_nodes: usize,
    /// Current scheduler leader node ID.
    pub scheduler_leader: Option<NodeId>,
}

impl ClusterInfo {
    /// Create empty cluster info.
    pub fn empty(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            total_nodes: 0,
            active_nodes: 0,
            draining_nodes: 0,
            dead_nodes: 0,
            scheduler_leader: None,
        }
    }
}

/// Leadership information for a role.
#[derive(Debug, Clone)]
pub struct LeaderInfo {
    /// The leader role.
    pub role: LeaderRole,
    /// Node ID of the leader.
    pub node_id: NodeId,
    /// When leadership was acquired.
    pub acquired_at: chrono::DateTime<chrono::Utc>,
    /// When the lease expires.
    pub lease_until: chrono::DateTime<chrono::Utc>,
}

impl LeaderInfo {
    /// Check if the lease is still valid.
    pub fn is_valid(&self) -> bool {
        self.lease_until > chrono::Utc::now()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cluster_info_empty() {
        let info = ClusterInfo::empty("test-cluster");
        assert_eq!(info.name, "test-cluster");
        assert_eq!(info.total_nodes, 0);
        assert_eq!(info.active_nodes, 0);
        assert!(info.scheduler_leader.is_none());
    }

    #[test]
    fn test_leader_info_validity() {
        let node_id = NodeId::new();
        let info = LeaderInfo {
            role: LeaderRole::Scheduler,
            node_id,
            acquired_at: chrono::Utc::now(),
            lease_until: chrono::Utc::now() + chrono::Duration::minutes(1),
        };
        assert!(info.is_valid());

        let expired_info = LeaderInfo {
            role: LeaderRole::Scheduler,
            node_id,
            acquired_at: chrono::Utc::now() - chrono::Duration::minutes(5),
            lease_until: chrono::Utc::now() - chrono::Duration::minutes(1),
        };
        assert!(!expired_info.is_valid());
    }
}

/// Node role in the cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NodeRole {
    /// HTTP gateway for client requests.
    Gateway,
    /// Function executor.
    Function,
    /// Background job worker.
    Worker,
    /// Scheduler (leader-only) for crons and job assignment.
    Scheduler,
}

impl NodeRole {
    /// Convert to string for database storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::Function => "function",
            Self::Worker => "worker",
            Self::Scheduler => "scheduler",
        }
    }

    /// Parse from string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "gateway" => Some(Self::Gateway),
            "function" => Some(Self::Function),
            "worker" => Some(Self::Worker),
            "scheduler" => Some(Self::Scheduler),
            _ => None,
        }
    }

    /// Get all default roles.
    pub fn all() -> Vec<Self> {
        vec![Self::Gateway, Self::Function, Self::Worker, Self::Scheduler]
    }
}

impl std::fmt::Display for NodeRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Leader role for coordinated operations.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum LeaderRole {
    /// Job assignment and cron triggering.
    Scheduler,
    /// Metrics aggregation.
    MetricsAggregator,
    /// Log compaction.
    LogCompactor,
}

impl LeaderRole {
    /// Get the PostgreSQL advisory lock ID for this role.
    pub fn lock_id(&self) -> i64 {
        // Use a unique ID based on "FORGE" + role number
        // 0x464F524745 = "FORGE" in hex
        match self {
            Self::Scheduler => 0x464F5247_0001,
            Self::MetricsAggregator => 0x464F5247_0002,
            Self::LogCompactor => 0x464F5247_0003,
        }
    }

    /// Convert to string for database storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Scheduler => "scheduler",
            Self::MetricsAggregator => "metrics_aggregator",
            Self::LogCompactor => "log_compactor",
        }
    }

    /// Parse from string.
    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "scheduler" => Some(Self::Scheduler),
            "metrics_aggregator" => Some(Self::MetricsAggregator),
            "log_compactor" => Some(Self::LogCompactor),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_role_conversion() {
        assert_eq!(NodeRole::from_str("gateway"), Some(NodeRole::Gateway));
        assert_eq!(NodeRole::from_str("worker"), Some(NodeRole::Worker));
        assert_eq!(NodeRole::from_str("invalid"), None);
        assert_eq!(NodeRole::Gateway.as_str(), "gateway");
    }

    #[test]
    fn test_all_roles() {
        let roles = NodeRole::all();
        assert_eq!(roles.len(), 4);
        assert!(roles.contains(&NodeRole::Gateway));
        assert!(roles.contains(&NodeRole::Scheduler));
    }

    #[test]
    fn test_leader_role_lock_ids() {
        // Each leader role should have a unique lock ID
        let scheduler_id = LeaderRole::Scheduler.lock_id();
        let metrics_id = LeaderRole::MetricsAggregator.lock_id();
        let log_id = LeaderRole::LogCompactor.lock_id();

        assert_ne!(scheduler_id, metrics_id);
        assert_ne!(metrics_id, log_id);
        assert_ne!(scheduler_id, log_id);
    }

    #[test]
    fn test_leader_role_conversion() {
        assert_eq!(
            LeaderRole::from_str("scheduler"),
            Some(LeaderRole::Scheduler)
        );
        assert_eq!(LeaderRole::from_str("invalid"), None);
        assert_eq!(LeaderRole::Scheduler.as_str(), "scheduler");
    }
}

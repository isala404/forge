use std::str::FromStr;

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

    /// Get all default roles.
    pub fn all() -> Vec<Self> {
        vec![Self::Gateway, Self::Function, Self::Worker, Self::Scheduler]
    }
}

/// Error for parsing NodeRole from string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseNodeRoleError(pub String);

impl std::fmt::Display for ParseNodeRoleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid node role: {}", self.0)
    }
}

impl std::error::Error for ParseNodeRoleError {}

impl FromStr for NodeRole {
    type Err = ParseNodeRoleError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "gateway" => Ok(Self::Gateway),
            "function" => Ok(Self::Function),
            "worker" => Ok(Self::Worker),
            "scheduler" => Ok(Self::Scheduler),
            _ => Err(ParseNodeRoleError(s.to_string())),
        }
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
            Self::Scheduler => 0x464F_5247_0001,
            Self::MetricsAggregator => 0x464F_5247_0002,
            Self::LogCompactor => 0x464F_5247_0003,
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
}

/// Error for parsing LeaderRole from string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseLeaderRoleError(pub String);

impl std::fmt::Display for ParseLeaderRoleError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid leader role: {}", self.0)
    }
}

impl std::error::Error for ParseLeaderRoleError {}

impl FromStr for LeaderRole {
    type Err = ParseLeaderRoleError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "scheduler" => Ok(Self::Scheduler),
            "metrics_aggregator" => Ok(Self::MetricsAggregator),
            "log_compactor" => Ok(Self::LogCompactor),
            _ => Err(ParseLeaderRoleError(s.to_string())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_node_role_conversion() {
        assert_eq!("gateway".parse::<NodeRole>(), Ok(NodeRole::Gateway));
        assert_eq!("worker".parse::<NodeRole>(), Ok(NodeRole::Worker));
        assert!("invalid".parse::<NodeRole>().is_err());
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
        assert_eq!("scheduler".parse::<LeaderRole>(), Ok(LeaderRole::Scheduler));
        assert!("invalid".parse::<LeaderRole>().is_err());
        assert_eq!(LeaderRole::Scheduler.as_str(), "scheduler");
    }
}

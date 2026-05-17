use std::str::FromStr;

/// Node role in the cluster.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
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
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LeaderRole {
    /// Job assignment and cron triggering.
    Scheduler,
    /// Metrics aggregation.
    MetricsAggregator,
    /// Log compaction.
    LogCompactor,
    /// Leader-elected daemon instance.
    Daemon(String),
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
            Self::Daemon(name) => {
                // FNV-1a 64-bit hash in the FORGE daemon namespace.
                // Standard FNV-1a: XOR byte, then multiply by the FNV prime.
                const FNV_OFFSET: i64 = 0x464F_5247_4000;
                const FNV_PRIME: i64 = 0x0100_0000_01b3; // 1099511628211
                let mut h: i64 = FNV_OFFSET;
                for b in name.bytes() {
                    h ^= b as i64;
                    h = h.wrapping_mul(FNV_PRIME);
                }
                h
            }
        }
    }

    /// Convert to string for database storage.
    pub fn as_str(&self) -> &str {
        match self {
            Self::Scheduler => "scheduler",
            Self::MetricsAggregator => "metrics_aggregator",
            Self::LogCompactor => "log_compactor",
            Self::Daemon(name) => name.as_str(),
        }
    }
}

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
            other => Ok(Self::Daemon(other.to_string())),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
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
        assert_eq!(
            "my_daemon".parse::<LeaderRole>(),
            Ok(LeaderRole::Daemon("my_daemon".to_string()))
        );
        assert_eq!(LeaderRole::Scheduler.as_str(), "scheduler");
        assert_eq!(
            LeaderRole::Daemon("my_daemon".to_string()).as_str(),
            "my_daemon"
        );
    }

    #[test]
    fn test_daemon_lock_ids_are_unique_and_stable() {
        let a = LeaderRole::Daemon("daemon_a".to_string()).lock_id();
        let b = LeaderRole::Daemon("daemon_b".to_string()).lock_id();
        assert_ne!(a, b);
        assert_eq!(a, LeaderRole::Daemon("daemon_a".to_string()).lock_id());

        assert_ne!(a, LeaderRole::Scheduler.lock_id());
        assert_ne!(a, LeaderRole::MetricsAggregator.lock_id());
        assert_ne!(a, LeaderRole::LogCompactor.lock_id());
    }
}

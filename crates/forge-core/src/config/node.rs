//! Node role and capability configuration.

use serde::{Deserialize, Serialize};

/// Node role configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct NodeConfig {
    /// Roles this node should assume.
    #[serde(default = "default_roles")]
    pub roles: Vec<NodeRole>,

    /// Worker capabilities for job routing.
    #[serde(default = "default_capabilities")]
    pub worker_capabilities: Vec<String>,
}

impl Default for NodeConfig {
    fn default() -> Self {
        Self {
            roles: default_roles(),
            worker_capabilities: default_capabilities(),
        }
    }
}

fn default_roles() -> Vec<NodeRole> {
    vec![
        NodeRole::Gateway,
        NodeRole::Function,
        NodeRole::Worker,
        NodeRole::Scheduler,
    ]
}

fn default_capabilities() -> Vec<String> {
    vec!["general".to_string()]
}

/// Available node roles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum NodeRole {
    /// Serves HTTP/gRPC traffic.
    Gateway,
    /// Executes queries and mutations.
    Function,
    /// Processes background jobs.
    Worker,
    /// Runs cron schedules and leader-elected tasks.
    Scheduler,
}

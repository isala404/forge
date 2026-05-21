//! Cluster coordination and node management.
//!
//! Nodes discover each other via PostgreSQL (`forge_nodes` table, heartbeat-based).
//! Singleton processes (scheduler, daemons) use advisory locks for leader election:
//! if the leader crashes, PostgreSQL releases the lock and a standby node acquires it.

mod node;
mod roles;
mod traits;

pub use node::{NodeId, NodeInfo, NodeStatus, ParseNodeStatusError};
pub use roles::{LeaderRole, NodeRole, ParseLeaderRoleError, ParseNodeRoleError};
pub use traits::{ClusterInfo, LeaderInfo};

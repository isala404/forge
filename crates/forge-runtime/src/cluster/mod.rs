pub mod discovery;
mod heartbeat;
pub(crate) mod metrics;
mod registry;
mod shutdown;

pub use discovery::{PeerAddress, discover_peers};
pub use heartbeat::{HeartbeatConfig, HeartbeatLoop};
pub use registry::{NodeCounts, NodeRegistry};
pub use shutdown::{GracefulShutdown, InFlightGuard, ShutdownConfig};

//! Daemon runtime components.

mod registry;
mod runner;

pub use registry::{BoxedDaemonHandler, DaemonEntry, DaemonRegistry};
pub use runner::{DaemonRunner, DaemonRunnerConfig};

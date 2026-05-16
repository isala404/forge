// TODO(pre-1.0): Collapse daemons into long-running jobs per 07-DELETION-LIST.md.
// Daemons duplicate job infrastructure (registry, runner, leader election) and should
// instead be modeled as never-completing jobs with restart-on-failure semantics.
// Deferred because the migration requires changes to the job worker's timeout and
// completion semantics (jobs currently assume finite execution).

//! Daemon runtime components.

mod registry;
mod runner;

pub use registry::{BoxedDaemonHandler, DaemonEntry, DaemonRegistry};
pub use runner::{DaemonRunner, DaemonRunnerConfig};

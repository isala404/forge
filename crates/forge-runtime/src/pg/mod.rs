//! PostgreSQL primitives module.
//!
//! Centralises all direct Postgres interactions (connection pooling, leader
//! election, schema migrations) into a single public surface. No other
//! subsystem should call `pg_advisory_lock`, `pg_notify`, or build pools
//! directly; they go through this module.

mod change_log;
mod leader;
pub mod migration;
mod notify;
mod pool;

pub use change_log::{ChangeRow, drain_change_log, min_seq, trim_change_log};
pub use leader::{LeaderConfig, LeaderElection};
pub use migration::{
    AppliedMigration, DriftStatus, Migration, MigrationRunner, MigrationStatus,
    load_migrations_from_dir,
};
pub use notify::{MAX_PAYLOAD_BYTES, NotifyChannel};
pub use pool::{Database, DatabasePool};

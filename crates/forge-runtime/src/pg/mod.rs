//! PostgreSQL primitives module.
//!
//! Centralises all direct Postgres interactions — connection pooling, leader
//! election, and schema migrations — into a single public surface. No other
//! subsystem should call `pg_advisory_lock`, `pg_notify`, or build pools
//! directly; they go through this module.

mod leader;
mod migration;
mod pool;

pub use leader::{LeaderConfig, LeaderElection, LeaderGuard};
pub use migration::{
    AppliedMigration, Migration, MigrationRunner, MigrationStatus, load_migrations_from_dir,
};
pub use pool::{Database, DatabasePool};

//! PostgreSQL migration runner.
//!
//! Re-exports the migration primitives from the migrations subsystem so the
//! `pg` module is the single public surface for all Postgres-touching
//! infrastructure.

pub use crate::migrations::runner::{
    AppliedMigration, DriftStatus, Migration, MigrationRunner, MigrationStatus,
    load_migrations_from_dir,
};

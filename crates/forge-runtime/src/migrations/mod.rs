//! Database migration system.
//!
//! Provides both built-in FORGE schema migrations and support for user migrations.

mod builtin;
mod diff;
mod executor;
mod generator;
mod runner;

pub use diff::{DiffAction, DiffEntry, SchemaDiff};
pub use executor::MigrationExecutor;
pub use generator::MigrationGenerator;
pub use runner::{load_migrations_from_dir, Migration, MigrationRunner};

// Re-export for internal use

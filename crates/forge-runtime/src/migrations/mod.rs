//! Database migration system.
//!
//! Provides both built-in FORGE schema migrations and support for user migrations.
//!
//! # Migration Types
//!
//! - **System migrations**: Internal FORGE schema, versioned as `__forge_vXXX`.
//!   Always applied before user migrations. New forge features can be added
//!   without conflicting with user migration numbering.
//!
//! - **User migrations**: Application schema, named `XXXX_name.sql`.
//!   Sorted alphabetically and applied after system migrations.

pub(crate) mod builtin;
pub(crate) mod runner;

pub use builtin::{
    SYSTEM_MIGRATION_PREFIX, SystemMigration, extract_version, get_all_system_sql,
    get_builtin_migrations, get_system_migrations, is_system_migration,
};
pub use runner::{
    AppliedMigration, DriftStatus, Migration, MigrationConfig, MigrationRunner, MigrationStatus,
    load_migrations_from_dir,
};

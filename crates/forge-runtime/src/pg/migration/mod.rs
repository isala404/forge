//! Database migration system. Lives under `pg/` with the rest of the
//! Postgres primitives so the doctrine "all PG-touching code goes through
//! `forge_runtime::pg`" is enforced by layout, not just by convention.
//!
//! # Migration types
//!
//! - **System migrations**: internal Forge schema, versioned as `__forge_vXXX`.
//!   Applied before user migrations so new framework features can ship without
//!   conflicting with user migration numbering.
//! - **User migrations**: application schema, named `XXXX_name.sql`. Sorted
//!   alphabetically and applied after system migrations.

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

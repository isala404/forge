//! Built-in FORGE schema migrations.
//!
//! These migrations create all internal tables required by the FORGE runtime.
//! They use a version-based naming scheme (`__forge_vXXX`) to avoid conflicts
//! with user migrations.
//!
//! # Migration Naming
//!
//! - System migrations: `__forge_vXXX` (pre-1.0: a single `__forge_v001`)
//! - User migrations: `0001_xxx`, `0002_xxx`, etc.
//!
//! System migrations are always applied before user migrations, regardless of
//! naming. This allows new forge features to be added without conflicting with
//! existing user migration numbering.

use super::runner::Migration;

/// System migration prefix. All forge internal migrations use this prefix.
pub const SYSTEM_MIGRATION_PREFIX: &str = "__forge_v";

const V001_INITIAL: &str = include_str!("../../../migrations/system/v001_initial.sql");

/// A system migration with a version number.
#[derive(Debug, Clone)]
pub struct SystemMigration {
    /// Version number (1, 2, 3, ...)
    pub version: u32,
    /// The SQL to execute
    pub sql: &'static str,
    /// Description of what this migration does
    pub description: &'static str,
}

impl SystemMigration {
    /// Get the migration version string used in the database (e.g., `__forge_v001`).
    pub fn name(&self) -> String {
        format!("{}{:03}", SYSTEM_MIGRATION_PREFIX, self.version)
    }

    /// Convert to a Migration struct.
    pub fn to_migration(&self) -> Migration {
        Migration::new(self.name(), self.sql)
    }
}

/// Get all built-in FORGE system migrations in version order.
///
/// These are applied in order before any user migrations.
pub fn get_system_migrations() -> Vec<SystemMigration> {
    vec![SystemMigration {
        version: 1,
        sql: V001_INITIAL,
        description: "FORGE internal schema",
    }]
}

/// Get system migrations as Migration structs.
pub fn get_builtin_migrations() -> Vec<Migration> {
    get_system_migrations()
        .into_iter()
        .map(|m| m.to_migration())
        .collect()
}

/// Get all system migrations SQL concatenated.
///
/// Use for test setup before running user migrations.
/// In production, use [`get_builtin_migrations`] for versioned application.
pub fn get_all_system_sql() -> String {
    get_system_migrations()
        .into_iter()
        .map(|m| m.sql)
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// Check if a migration name is a system migration.
pub fn is_system_migration(name: &str) -> bool {
    name.starts_with(SYSTEM_MIGRATION_PREFIX)
}

/// Extract version number from a system migration name.
/// Returns None if not a valid system migration name.
pub fn extract_version(name: &str) -> Option<u32> {
    name.strip_prefix(SYSTEM_MIGRATION_PREFIX)
        .and_then(|suffix| suffix.parse().ok())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn test_get_system_migrations() {
        let migrations = get_system_migrations();
        assert_eq!(migrations.len(), 1);
        assert_eq!(migrations[0].version, 1);
        assert_eq!(migrations[0].name(), "__forge_v001");
    }

    #[test]
    fn test_migration_sql_not_empty() {
        let migrations = get_system_migrations();
        for m in migrations {
            assert!(!m.sql.is_empty(), "Migration v{} has empty SQL", m.version);
        }
    }

    #[test]
    fn test_migration_sql_contains_expected_objects() {
        let v001 = get_system_migrations()[0].sql;

        // Core tables
        for table in [
            "forge_nodes",
            "forge_leaders",
            "forge_kv",
            "forge_kv_counters",
            "forge_jobs",
            "forge_jobs_history",
            "forge_cron_runs",
            "forge_workflow_definitions",
            "forge_workflow_runs",
            "forge_workflow_state",
            "forge_workflow_events",
            "forge_workflow_steps",
            "forge_admin_audit",
            "forge_paused_queues",
            "forge_rate_limits",
            "forge_sessions",
            "forge_subscriptions",
            "forge_change_log",
            "forge_invalidations",
            "forge_daemons",
            "forge_webhook_events",
            "forge_refresh_tokens",
            "forge_oauth_clients",
            "forge_oauth_codes",
            "forge_signals_events",
            "forge_signals_sessions",
            "forge_signals_users",
            "forge_signals_hourly_stats",
            "forge_signals_daily_rollup",
        ] {
            assert!(v001.contains(table), "expected table {table} in v001 SQL");
        }

        // Key functions
        for fn_name in [
            "forge_validate_identifier",
            "forge_notify_change",
            "forge_notify_change_statement",
            "forge_notify_job_available",
            "forge_enable_reactivity",
            "forge_disable_reactivity",
            "forge_workflow_event_notify",
            "forge_workflow_runs_cancel_notify",
            "forge_cleanup_webhook_events",
            "forge_cleanup_expired_jobs",
            "forge_archive_completed_jobs",
            "forge_purge_expired_refresh_tokens",
            "forge_purge_expired_oauth_codes",
            "forge_purge_expired_invalidations",
            "forge_trim_change_log",
            "forge_signals_ensure_partition",
            "forge_signals_drop_old_partitions",
            "forge_signals_roll_up_hour",
            "forge_signals_roll_up_day",
        ] {
            assert!(
                v001.contains(fn_name),
                "expected function {fn_name} in v001 SQL"
            );
        }

        // Notable columns and replay fields
        for needle in [
            "owner_subject",
            "token_family",
            "raw_body",
            "raw_headers",
            "tenant_id",
        ] {
            assert!(v001.contains(needle), "expected {needle} in v001 SQL");
        }
    }

    #[test]
    fn test_is_system_migration() {
        assert!(is_system_migration("__forge_v001"));
        assert!(is_system_migration("__forge_v002"));
        assert!(is_system_migration("__forge_v100"));
        assert!(!is_system_migration("0001_create_users"));
        assert!(!is_system_migration("user_migration"));
    }

    #[test]
    fn test_extract_version() {
        assert_eq!(extract_version("__forge_v001"), Some(1));
        assert_eq!(extract_version("__forge_v002"), Some(2));
        assert_eq!(extract_version("__forge_v100"), Some(100));
        assert_eq!(extract_version("0001_create_users"), None);
        assert_eq!(extract_version("invalid"), None);
    }

    #[test]
    fn test_system_migration_to_migration() {
        let sys = SystemMigration {
            version: 1,
            sql: "SELECT 1;",
            description: "Test",
        };
        let m = sys.to_migration();
        assert_eq!(m.version, "__forge_v001");
        assert_eq!(m.up_sql, "SELECT 1;");
    }
}

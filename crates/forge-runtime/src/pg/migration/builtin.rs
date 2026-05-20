//! Built-in FORGE schema migrations.
//!
//! These migrations create all internal tables required by the FORGE runtime.
//! They use a version-based naming scheme (`__forge_vXXX`) to avoid conflicts
//! with user migrations.
//!
//! # Migration Naming
//!
//! - System migrations: `__forge_v001` (single migration, pre-1.0)
//! - User migrations: `0001_xxx`, `0002_xxx`, etc.
//!
//! System migrations are always applied before user migrations, regardless of
//! naming. This allows new forge features to be added without conflicting with
//! existing user migration numbering.

use super::runner::Migration;

/// System migration prefix. All forge internal migrations use this prefix.
pub const SYSTEM_MIGRATION_PREFIX: &str = "__forge_v";

const V001_INITIAL: &str = include_str!("../../../migrations/system/v001_initial.sql");
const V002_CHANGE_LOG: &str = include_str!("../../../migrations/system/v002_change_log.sql");
const V003_JOB_WAKEUP: &str = include_str!("../../../migrations/system/v003_job_wakeup.sql");
const V004_KV: &str = include_str!("../../../migrations/system/v004_kv.sql");
const V005_WORKFLOW_STATUS: &str =
    include_str!("../../../migrations/system/v005_workflow_status.sql");
const V006_WORKFLOW_INDEXES: &str =
    include_str!("../../../migrations/system/v006_workflow_indexes.sql");
const V007_STATEMENT_TRIGGER: &str =
    include_str!("../../../migrations/system/v007_statement_trigger.sql");
const V008_WORKFLOW_STATE: &str =
    include_str!("../../../migrations/system/v008_workflow_state.sql");
const V009_JOBS_HISTORY: &str =
    include_str!("../../../migrations/system/v009_jobs_history.sql");
const V010_SIGNALS_ROLLUPS: &str =
    include_str!("../../../migrations/system/v010_signals_rollups.sql");
const V011_WEBHOOK_REPLAY: &str =
    include_str!("../../../migrations/system/v011_webhook_replay.sql");

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
    vec![
        SystemMigration {
            version: 1,
            sql: V001_INITIAL,
            description: "Complete FORGE schema",
        },
        SystemMigration {
            version: 2,
            sql: V002_CHANGE_LOG,
            description: "Change log for gap-free reactivity",
        },
        SystemMigration {
            version: 3,
            sql: V003_JOB_WAKEUP,
            description: "NOTIFY trigger for job wakeup",
        },
        SystemMigration {
            version: 4,
            sql: V004_KV,
            description: "Key-value store tables",
        },
        SystemMigration {
            version: 5,
            sql: V005_WORKFLOW_STATUS,
            description: "Simplify workflow status to 6 variants",
        },
        SystemMigration {
            version: 6,
            sql: V006_WORKFLOW_INDEXES,
            description: "Fix partial indexes after status split",
        },
        SystemMigration {
            version: 7,
            sql: V007_STATEMENT_TRIGGER,
            description: "Statement-level trigger mode and reactivity controls",
        },
        SystemMigration {
            version: 8,
            sql: V008_WORKFLOW_STATE,
            description: "Separate workflow state from runs table",
        },
        SystemMigration {
            version: 9,
            sql: V009_JOBS_HISTORY,
            description: "Archive table for completed jobs",
        },
        SystemMigration {
            version: 10,
            sql: V010_SIGNALS_ROLLUPS,
            description: "Incremental rollup tables replacing materialized views",
        },
        SystemMigration {
            version: 11,
            sql: V011_WEBHOOK_REPLAY,
            description: "Webhook replay storage",
        },
    ]
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
        assert_eq!(migrations.len(), 11);
        assert_eq!(migrations[0].version, 1);
        assert_eq!(migrations[0].name(), "__forge_v001");
        assert_eq!(migrations[1].name(), "__forge_v002");
        assert_eq!(migrations[2].name(), "__forge_v003");
        assert_eq!(migrations[3].name(), "__forge_v004");
        assert_eq!(migrations[4].name(), "__forge_v005");
        assert_eq!(migrations[5].name(), "__forge_v006");
        assert_eq!(migrations[6].name(), "__forge_v007");
        assert_eq!(migrations[7].name(), "__forge_v008");
        assert_eq!(migrations[8].name(), "__forge_v009");
        assert_eq!(migrations[9].name(), "__forge_v010");
        assert_eq!(migrations[10].name(), "__forge_v011");
    }

    #[test]
    fn test_migration_sql_not_empty() {
        let migrations = get_system_migrations();
        for m in migrations {
            assert!(!m.sql.is_empty(), "Migration v{} has empty SQL", m.version);
        }
    }

    #[test]
    fn test_migration_sql_contains_tables() {
        let migrations = get_system_migrations();

        // v001: core schema
        let v001 = migrations[0].sql;
        assert!(v001.contains("forge_nodes"));
        assert!(v001.contains("forge_leaders"));
        assert!(v001.contains("forge_jobs"));
        assert!(v001.contains("forge_cron_runs"));
        assert!(v001.contains("forge_workflow_runs"));
        assert!(v001.contains("forge_workflow_steps"));
        assert!(v001.contains("forge_sessions"));
        assert!(v001.contains("forge_subscriptions"));
        assert!(v001.contains("forge_daemons"));
        assert!(v001.contains("forge_webhook_events"));
        assert!(v001.contains("forge_refresh_tokens"));
        assert!(v001.contains("forge_oauth_clients"));
        assert!(v001.contains("forge_oauth_codes"));
        assert!(v001.contains("forge_signals_events"));
        assert!(v001.contains("forge_signals_sessions"));
        assert!(v001.contains("forge_signals_users"));
        assert!(v001.contains("owner_subject"));
        assert!(v001.contains("token_family"));
        assert!(v001.contains("compensation_state"));
        assert!(v001.contains("saved_state"));

        // v002: change log
        let v002 = migrations[1].sql;
        assert!(v002.contains("forge_change_log"));
        assert!(v002.contains("forge_trim_change_log"));

        // v003: job wakeup
        let v003 = migrations[2].sql;
        assert!(v003.contains("forge_notify_job_available"));

        // v004: KV store
        let v004 = migrations[3].sql;
        assert!(v004.contains("forge_kv"));
        assert!(v004.contains("forge_kv_counters"));

        // v006: workflow indexes
        let v006 = migrations[5].sql;
        assert!(v006.contains("idx_forge_workflow_runs_sleeping"));
        assert!(v006.contains("idx_forge_workflow_runs_pending"));

        // v007: statement trigger
        let v007 = migrations[6].sql;
        assert!(v007.contains("forge_notify_change_statement"));
        assert!(v007.contains("forge_enable_reactivity"));

        // v008: workflow state separation
        let v008 = migrations[7].sql;
        assert!(v008.contains("forge_workflow_state"));

        // v009: jobs history
        let v009 = migrations[8].sql;
        assert!(v009.contains("forge_jobs_history"));
        assert!(v009.contains("forge_archive_completed_jobs"));

        // v010: signals rollups
        let v010 = migrations[9].sql;
        assert!(v010.contains("forge_signals_hourly_stats"));
        assert!(v010.contains("forge_signals_daily_rollup"));

        // v011: webhook replay
        let v011 = migrations[10].sql;
        assert!(v011.contains("raw_body"));
        assert!(v011.contains("raw_headers"));
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

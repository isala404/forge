//! Built-in FORGE schema migrations.
//!
//! These migrations create all internal tables required by the FORGE runtime.
//! They are versioned and only applied once (tracked in forge_migrations).

use super::runner::Migration;

/// The internal FORGE schema SQL, embedded from the migrations directory.
const FORGE_INTERNAL_SQL: &str = include_str!("../../migrations/0000_forge_internal.sql");

/// Get all built-in FORGE migrations.
///
/// These are applied in order before any user migrations.
pub fn get_builtin_migrations() -> Vec<Migration> {
    vec![Migration::new("0000_forge_internal", FORGE_INTERNAL_SQL)]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_get_builtin_migrations() {
        let migrations = get_builtin_migrations();
        assert!(!migrations.is_empty());
        assert_eq!(migrations[0].name, "0000_forge_internal");
    }

    #[test]
    fn test_migration_sql_not_empty() {
        let migrations = get_builtin_migrations();
        for m in migrations {
            assert!(!m.sql.is_empty(), "Migration {} has empty SQL", m.name);
        }
    }

    #[test]
    fn test_migration_sql_contains_tables() {
        let migrations = get_builtin_migrations();
        let sql = &migrations[0].sql;

        // Verify all core tables are defined
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS forge_nodes"));
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS forge_leaders"));
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS forge_jobs"));
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS forge_cron_runs"));
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS forge_workflow_runs"));
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS forge_workflow_steps"));
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS forge_metrics"));
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS forge_logs"));
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS forge_traces"));
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS forge_sessions"));
        assert!(sql.contains("CREATE TABLE IF NOT EXISTS forge_subscriptions"));
    }
}

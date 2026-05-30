use anyhow::Result;
use std::path::Path;

// Derived from crates/forge-runtime/migrations/system/v00*_*.sql. Keep in sync
// when a new system table is added there; `forge check` must fail closed when
// a user migration shadows a runtime-owned name.
pub(super) const RESERVED_SYSTEM_TABLES: &[&str] = &[
    "forge_admin_audit",
    "forge_change_log",
    "forge_cron_runs",
    "forge_daemons",
    "forge_jobs",
    "forge_jobs_history",
    "forge_kv",
    "forge_kv_counters",
    "forge_leaders",
    "forge_nodes",
    "forge_oauth_clients",
    "forge_oauth_codes",
    "forge_paused_queues",
    "forge_rate_limits",
    "forge_refresh_tokens",
    "forge_signals_daily_rollup",
    "forge_signals_events",
    "forge_signals_hourly_stats",
    "forge_signals_sessions",
    "forge_signals_users",
    "forge_system_migrations",
    "forge_webhook_events",
    "forge_workflow_definitions",
    "forge_workflow_events",
    "forge_workflow_runs",
    "forge_workflow_state",
    "forge_workflow_steps",
];

/// System tables a handler may legitimately write to directly.
///
/// `forge_workflow_events` is the workflow event inbox: a handler delivers an
/// external event to a `ctx.wait_for_event(...)` workflow by inserting a row
/// (there is no higher-level API for this, and the runtime's own harness does
/// the same). It stays in `RESERVED_SYSTEM_TABLES` for the migration-shadow
/// check, but writing to it is a supported pattern, not a leak.
const HANDLER_WRITABLE_SYSTEM_TABLES: &[&str] = &["forge_workflow_events"];

pub(super) fn scan_system_table_writes(
    dir: &Path,
    out: &mut Vec<(std::path::PathBuf, &'static str)>,
) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            scan_system_table_writes(&path, out)?;
            continue;
        }

        if !file_type.is_file() || path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }

        let content = std::fs::read_to_string(&path)?;
        let lower = content.to_ascii_lowercase();

        for table in RESERVED_SYSTEM_TABLES {
            if HANDLER_WRITABLE_SYSTEM_TABLES.contains(table) {
                continue;
            }
            let needles = [
                format!("insert into {table}"),
                format!("update {table}"),
                format!("delete from {table}"),
            ];
            if needles.iter().any(|n| lower.contains(n.as_str())) {
                out.push((path.clone(), *table));
                break;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn flags_state_table_writes_but_allows_the_workflow_event_inbox() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("jobs.rs"),
            r#"sqlx::query("INSERT INTO forge_jobs (id) VALUES ($1)")"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("events.rs"),
            r#"sqlx::query("INSERT INTO forge_workflow_events (id) VALUES ($1)")"#,
        )
        .unwrap();

        let mut out = Vec::new();
        scan_system_table_writes(dir.path(), &mut out).unwrap();
        let flagged: Vec<&str> = out.iter().map(|(_, t)| *t).collect();

        assert!(
            flagged.contains(&"forge_jobs"),
            "direct write to a state table must be flagged"
        );
        assert!(
            !flagged.contains(&"forge_workflow_events"),
            "the workflow event inbox is a supported handler write target"
        );
    }
}

use anyhow::Result;
use std::path::Path;

pub(super) const RESERVED_SYSTEM_TABLES: &[&str] = &[
    "forge_jobs",
    "forge_workflow_runs",
    "forge_workflow_definitions",
    "forge_cron_runs",
    "forge_system_migrations",
    "forge_sessions",
    "forge_refresh_tokens",
    "forge_signals_events",
];

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

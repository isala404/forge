use anyhow::Result;
use std::path::Path;
use std::process::{Command as StdCommand, Stdio};
use tokio::process::Command as TokioCommand;

use super::super::ui;
use super::sqlx::{
    SqlxCacheCheck, inspect_sqlx_cache, project_uses_compile_time_sqlx_macros,
    resolve_database_url, sqlx_cache_staleness,
};
use super::system_tables::scan_system_table_writes;
use super::{CheckCommand, CheckResult};

impl CheckCommand {
    pub(super) fn check_system_table_writes(&self, result: &mut CheckResult) -> Result<()> {
        let src_dir = Path::new("src");
        if !src_dir.exists() {
            return Ok(());
        }

        let mut offenses = Vec::new();
        scan_system_table_writes(src_dir, &mut offenses)?;

        if offenses.is_empty() {
            result.pass("No direct writes to forge_* system tables");
        } else {
            for (path, table) in offenses.iter().take(5) {
                result.fail(
                    &format!("Direct write to {} in {}", table, path.display()),
                    &format!(
                        "Use ctx.dispatch_job()/ctx.start_workflow()/ctx.issue_token_pair() instead of writing to {} directly",
                        table
                    ),
                );
            }
            if offenses.len() > 5 {
                result.info(&format!("... and {} more", offenses.len() - 5));
            }
        }

        Ok(())
    }

    pub(super) fn refresh_sqlx_cache_if_stale(&self, result: &mut CheckResult) -> Result<()> {
        let src_dir = Path::new("src");
        let sqlx_dir = Path::new(".sqlx");

        if !project_uses_compile_time_sqlx_macros(src_dir)? {
            result.info(".sqlx/ refresh skipped (no sqlx::query!() macros in src/)");
            return Ok(());
        }

        let stale_reason = sqlx_cache_staleness(sqlx_dir, src_dir)?;
        let Some(reason) = stale_reason else {
            result.pass(".sqlx/ is up to date");
            return Ok(());
        };

        result.info(&format!(".sqlx/ refresh needed: {reason}"));

        let has_cargo_sqlx = super::super::project_root::cargo_sqlx_available();
        if !has_cargo_sqlx {
            result.fail(
                "cargo-sqlx is required to refresh .sqlx/",
                "cargo install sqlx-cli --no-default-features --features postgres \
                 (or pass --no-prepare to forge check)",
            );
            return Ok(());
        }

        let database_url = match resolve_database_url(&self.config) {
            Ok(u) => u,
            Err(e) => {
                result.fail(
                    &format!("DATABASE_URL not resolvable: {e}"),
                    "Set DATABASE_URL to a running Postgres instance, or pass --no-prepare",
                );
                return Ok(());
            }
        };

        println!("  {} Running cargo sqlx prepare --workspace", ui::step());
        let output = StdCommand::new("cargo")
            .args(["sqlx", "prepare", "--workspace"])
            .env("DATABASE_URL", &database_url)
            .output()?;
        if output.status.success() {
            result.pass(".sqlx/ refreshed");
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            result.fail(
                ".sqlx/ refresh failed",
                "Inspect cargo sqlx prepare output below; if intentional, pass --no-prepare",
            );
            eprintln!("{}", stderr);
        }

        Ok(())
    }

    pub(super) fn check_sqlx_cache(&self, result: &mut CheckResult) -> Result<()> {
        let sqlx_dir = Path::new(".sqlx");
        let uses_compile_time_macros = project_uses_compile_time_sqlx_macros(Path::new("src"))?;
        let cache_status = inspect_sqlx_cache(sqlx_dir)?;

        match cache_status {
            SqlxCacheCheck::Missing => {
                if uses_compile_time_macros {
                    result.fail(
                        ".sqlx/ directory missing",
                        "Run 'forge migrate prepare' to generate the offline query cache",
                    );
                } else {
                    result.info("No .sqlx/ cache yet (no compile-time sqlx macros found)");
                }
                return Ok(());
            }
            SqlxCacheCheck::Empty => {
                if uses_compile_time_macros {
                    result.fail(
                        ".sqlx/ has no cached queries",
                        "Run 'forge migrate prepare' to populate the offline cache",
                    );
                } else {
                    result.pass(".sqlx/ directory present");
                }
                return Ok(());
            }
            SqlxCacheCheck::Ready(query_file_count) => {
                result.pass(&format!(
                    ".sqlx/ cache with {} query file(s)",
                    query_file_count
                ));
            }
        }

        let query_files: Vec<_> = std::fs::read_dir(sqlx_dir)?
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().starts_with("query-"))
            .collect();

        let migrations_dir = Path::new("migrations");
        if migrations_dir.exists() {
            let cache_mtime = query_files
                .iter()
                .filter_map(|e| e.metadata().ok())
                .filter_map(|m| m.modified().ok())
                .min();

            let migration_mtime = std::fs::read_dir(migrations_dir)?
                .filter_map(|e| e.ok())
                .filter(|e| e.path().extension().is_some_and(|ext| ext == "sql"))
                .filter_map(|e| e.metadata().ok())
                .filter_map(|m| m.modified().ok())
                .max();

            if let (Some(oldest_cache), Some(newest_migration)) = (cache_mtime, migration_mtime)
                && newest_migration > oldest_cache
            {
                result.warn(
                    "Migrations are newer than .sqlx/ cache",
                    "Run 'forge migrate prepare' to refresh the cache",
                );
            }
        }

        let sqlx_toml = Path::new("sqlx.toml");
        if sqlx_toml.exists() {
            let content = std::fs::read_to_string(sqlx_toml)?;
            if content.contains("offline = true") {
                result.pass("sqlx.toml configured with offline = true");
            } else {
                result.warn(
                    "sqlx.toml missing offline = true",
                    "Add [common] offline = true to sqlx.toml",
                );
            }
        } else {
            result.warn(
                "sqlx.toml not found",
                "Create sqlx.toml with [common] offline = true",
            );
        }

        Ok(())
    }

    pub(super) async fn check_rust_linting(&self, result: &mut CheckResult) {
        println!();

        let fmt_result = TokioCommand::new("cargo")
            .args(["fmt", "--check"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;

        match fmt_result {
            Ok(status) if status.success() => {
                result.pass("cargo fmt check passed");
            }
            Ok(_) => {
                result.fail(
                    "Code formatting issues found",
                    "Run 'cargo fmt' to fix formatting",
                );
            }
            Err(_) => {
                result.warn(
                    "Could not run cargo fmt",
                    "Ensure rustfmt is installed: rustup component add rustfmt",
                );
            }
        }

        let clippy_output = TokioCommand::new("cargo")
            .args(["clippy", "--", "-D", "warnings"])
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .output()
            .await;

        match clippy_output {
            Ok(output) if output.status.success() => {
                result.pass("cargo clippy check passed");
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                result.fail(
                    "Clippy warnings found",
                    "Run 'cargo clippy' to see warnings",
                );
                if !stderr.is_empty() {
                    eprintln!("{}", stderr);
                }
            }
            Err(_) => {
                result.warn(
                    "Could not run cargo clippy",
                    "Ensure clippy is installed: rustup component add clippy",
                );
            }
        }
    }
}

pub(super) fn collect_rs_files(entries: std::fs::ReadDir, out: &mut Vec<std::path::PathBuf>) {
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            if let Ok(sub) = std::fs::read_dir(&path) {
                collect_rs_files(sub, out);
            }
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

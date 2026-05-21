mod bindings;
mod checks;
mod config;
mod frontend;
mod project;
mod sqlx;
mod system_tables;

use anyhow::Result;
use clap::{Parser, ValueEnum};
use console::style;

use super::ui;

/// Output format for `forge check`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum, Default)]
pub enum CheckFormat {
    /// Human-readable output with colours (default).
    #[default]
    Human,
    /// Machine-readable JSON: `{ "status": "ok"|"error", "checks": [...] }`
    /// where each check has `{name, status: "ok"|"warn"|"error", error?}`.
    Json,
}

/// Validate project configuration and dependencies.
///
/// Checks that the project is correctly configured and all required
/// files are in place with valid content.
#[derive(Parser)]
pub struct CheckCommand {
    /// Path to forge.toml (default: ./forge.toml)
    #[arg(short, long, default_value = "forge.toml")]
    pub config: String,

    /// Skip the auto-refresh of `.sqlx/` and treat a stale cache as a real failure.
    #[arg(long)]
    pub no_prepare: bool,

    /// Run the auto-refresh of `.sqlx/` and exit, skipping the rest of the check pipeline.
    #[arg(long)]
    pub prepare_only: bool,

    /// Output format: `human` (default) or `json`.
    #[arg(long, value_enum, default_value = "human")]
    pub format: CheckFormat,
}

#[derive(Debug, Clone)]
pub(super) struct CheckEntry {
    pub(super) name: String,
    pub(super) status: &'static str,
    pub(super) error: Option<String>,
}

pub(super) struct CheckResult {
    pub(super) passed: bool,
    pub(super) warnings: Vec<String>,
    pub(super) errors: Vec<String>,
    /// Individual check records for JSON output.
    pub(super) entries: Vec<CheckEntry>,
    pub(super) format: CheckFormat,
}

impl CheckResult {
    pub(super) fn new(format: CheckFormat) -> Self {
        Self {
            passed: true,
            warnings: Vec::new(),
            errors: Vec::new(),
            entries: Vec::new(),
            format,
        }
    }

    pub(super) fn pass(&mut self, msg: &str) {
        if self.format == CheckFormat::Human {
            println!("  {} {}", ui::ok(), msg);
        }
        self.entries.push(CheckEntry {
            name: msg.to_string(),
            status: "ok",
            error: None,
        });
    }

    pub(super) fn warn(&mut self, msg: &str, fix: &str) {
        if self.format == CheckFormat::Human {
            println!("  {} {}", ui::warn(), msg);
        }
        self.warnings.push(fix.to_string());
        self.entries.push(CheckEntry {
            name: msg.to_string(),
            status: "warn",
            error: Some(fix.to_string()),
        });
    }

    pub(super) fn fail(&mut self, msg: &str, fix: &str) {
        if self.format == CheckFormat::Human {
            println!("  {} {}", ui::error(), msg);
        }
        self.errors.push(fix.to_string());
        self.entries.push(CheckEntry {
            name: msg.to_string(),
            status: "error",
            error: Some(fix.to_string()),
        });
        self.passed = false;
    }

    pub(super) fn info(&mut self, msg: &str) {
        if self.format == CheckFormat::Human {
            println!("    {} {}", ui::info(), msg);
        }
    }

    pub(super) fn section(&mut self, title: &str) {
        if self.format == CheckFormat::Human {
            println!();
            println!("  {} {}", ui::step(), style(title).bold());
        }
    }

    pub(super) fn print_json(&self) {
        let status = if self.passed { "ok" } else { "error" };
        let checks: Vec<serde_json::Value> = self
            .entries
            .iter()
            .map(|e| {
                let mut obj = serde_json::Map::new();
                obj.insert("name".to_string(), serde_json::json!(e.name));
                obj.insert("status".to_string(), serde_json::json!(e.status));
                if let Some(err) = &e.error {
                    obj.insert("error".to_string(), serde_json::json!(err));
                }
                serde_json::Value::Object(obj)
            })
            .collect();
        let payload = serde_json::json!({ "status": status, "checks": checks });
        println!("{}", payload);
    }
}

impl CheckCommand {
    pub async fn execute(self) -> Result<()> {
        let root = super::project_root::enter_project_root()?;

        if self.format == CheckFormat::Human {
            ui::section("FORGE Project Check");
            println!(
                "  {} Scanning project configuration and dependencies",
                ui::tool()
            );
            println!(
                "  {} Project root: {}",
                ui::info(),
                style(root.display()).cyan()
            );
        }

        let mut result = CheckResult::new(self.format);

        // Refresh the offline cache before downstream checks so cache-miss noise
        // doesn't bury real type errors. `--no-prepare` opts out.
        if !self.no_prepare {
            result.section("Offline Cache Refresh");
            self.refresh_sqlx_cache_if_stale(&mut result)?;
            if self.prepare_only {
                println!();
                println!("{} Prepare-only mode: skipping remaining checks.", ui::ok());
                return Ok(());
            }
        }

        result.section("Configuration");
        self.check_forge_toml(&mut result)?;
        self.check_cargo_toml(&mut result)?;

        result.section("Project Structure");
        self.check_directory_structure(&mut result);

        result.section("Migrations");
        self.check_migrations(&mut result)?;

        result.section("Functions");
        self.check_functions(&mut result)?;

        result.section("Schema");
        self.check_schema(&mut result)?;

        result.section("System Tables");
        self.check_system_table_writes(&mut result)?;

        result.section("SQLx Cache");
        self.check_sqlx_cache(&mut result)?;

        result.section("Rust Tooling");
        self.check_rust_linting(&mut result).await;

        result.section("Frontend");
        self.check_frontend(&mut result)?;

        result.section("Generated Bindings");
        self.check_generated_bindings(&mut result)?;

        result.section("Frontend Tooling");
        self.check_frontend_linting(&mut result).await;

        if self.format == CheckFormat::Json {
            result.print_json();
            if !result.passed {
                return Err(anyhow::anyhow!("Project check failed"));
            }
            return Ok(());
        }

        // Human summary
        println!();
        if result.passed && result.warnings.is_empty() {
            println!("{} All checks passed! Ready for development.", ui::ok());
            println!();
            println!("Next steps:");
            println!(
                "  {} Start development",
                style("docker compose up --build").cyan()
            );
        } else if result.passed {
            println!(
                "{} Checks passed with {} warning(s)",
                ui::warn(),
                result.warnings.len()
            );
            println!();
            println!("Suggestions:");
            for warning in &result.warnings {
                println!("  {} {}", ui::step(), warning);
            }
        } else {
            println!(
                "{} {} error(s) found. Fix the issues and run 'forge check' again.",
                ui::error(),
                result.errors.len()
            );
            println!();
            println!("To fix:");
            for error in &result.errors {
                println!("  {} {}", ui::step(), error);
            }
            return Err(anyhow::anyhow!("Project check failed"));
        }

        println!();
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use self::sqlx::{
        SqlxCacheCheck, file_uses_sqlx_macros, inspect_sqlx_cache,
        project_uses_compile_time_sqlx_macros,
    };
    use self::system_tables::scan_system_table_writes;
    use super::*;

    #[test]
    fn test_check_result() {
        let result = CheckResult::new(CheckFormat::Human);
        assert!(result.passed);
        assert!(result.warnings.is_empty());
        assert!(result.errors.is_empty());
    }

    #[test]
    fn json_output_shape() {
        let mut result = CheckResult::new(CheckFormat::Json);
        result.pass("config ok");
        result.warn("missing file", "add file");
        result.fail("bad \"setting\"", "fix\nsetting");
        assert!(!result.passed);
        assert_eq!(result.entries.len(), 3);
        assert_eq!(result.entries[0].status, "ok");
        assert_eq!(result.entries[1].status, "warn");
        assert_eq!(result.entries[2].status, "error");
        // Embedded quotes and newlines must round-trip through serde_json
        // without producing invalid JSON.
        let entry = &result.entries[2];
        let value = serde_json::json!({
            "name": entry.name,
            "status": entry.status,
            "error": entry.error,
        });
        let serialized = value.to_string();
        let parsed: serde_json::Value = serde_json::from_str(&serialized).unwrap();
        assert_eq!(parsed["status"], "error");
        assert_eq!(parsed["name"], "bad \"setting\"");
        assert_eq!(parsed["error"], "fix\nsetting");
    }

    #[test]
    fn test_detect_compile_time_sqlx_macros() {
        let dir = tempfile::tempdir().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(
            src_dir.join("queries.rs"),
            r#"fn demo() { let _ = sqlx::query!("SELECT 1"); }"#,
        )
        .unwrap();

        assert!(project_uses_compile_time_sqlx_macros(&src_dir).unwrap());
    }

    #[test]
    fn test_ignore_runtime_sqlx_calls() {
        let dir = tempfile::tempdir().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(
            src_dir.join("queries.rs"),
            r#"fn demo() { let _ = sqlx::query("SELECT 1"); }"#,
        )
        .unwrap();

        assert!(!project_uses_compile_time_sqlx_macros(&src_dir).unwrap());
    }

    #[test]
    fn test_empty_sqlx_directory_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let sqlx_dir = dir.path().join(".sqlx");
        std::fs::create_dir_all(&sqlx_dir).unwrap();

        assert_eq!(
            inspect_sqlx_cache(&sqlx_dir).unwrap(),
            SqlxCacheCheck::Empty
        );
    }

    #[test]
    fn test_sqlx_directory_with_query_cache_is_detected() {
        let dir = tempfile::tempdir().unwrap();
        let sqlx_dir = dir.path().join(".sqlx");
        std::fs::create_dir_all(&sqlx_dir).unwrap();
        std::fs::write(sqlx_dir.join("query-demo.json"), "{}").unwrap();

        assert_eq!(
            inspect_sqlx_cache(&sqlx_dir).unwrap(),
            SqlxCacheCheck::Ready(1)
        );
    }

    #[test]
    fn test_detect_manual_forge_jobs_insert() {
        let dir = tempfile::tempdir().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(
            src_dir.join("bad.rs"),
            r#"fn demo() { sqlx::query!("INSERT INTO forge_jobs (id) VALUES ($1)"); }"#,
        )
        .unwrap();

        let mut out = Vec::new();
        scan_system_table_writes(&src_dir, &mut out).unwrap();
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].1, "forge_jobs");
    }

    #[test]
    fn test_allow_user_tables() {
        let dir = tempfile::tempdir().unwrap();
        let src_dir = dir.path().join("src");
        std::fs::create_dir_all(&src_dir).unwrap();
        std::fs::write(
            src_dir.join("ok.rs"),
            r#"fn demo() { sqlx::query!("INSERT INTO todos (id) VALUES ($1)"); }"#,
        )
        .unwrap();

        let mut out = Vec::new();
        scan_system_table_writes(&src_dir, &mut out).unwrap();
        assert!(out.is_empty());
    }

    #[test]
    fn test_file_uses_sqlx_macros_detects_comment() {
        // A commented-out macro invocation should not count.
        let content = r#"// let _ = sqlx::query!("SELECT 1");"#;
        assert!(!file_uses_sqlx_macros(content));
    }
}

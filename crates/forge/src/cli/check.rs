use anyhow::Result;
use clap::{Parser, ValueEnum};
use console::style;
use serde_json::json;
use std::path::Path;
use std::process::{Command as StdCommand, Stdio};
use tokio::process::Command as TokioCommand;

use super::frontend_codegen::BindingGeneratorInput;
use super::frontend_target::FrontendTarget;
use super::ui;

use forge_codegen::find_duplicate_handlers;

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
struct CheckEntry {
    name: String,
    status: &'static str,
    error: Option<String>,
}

struct CheckResult {
    passed: bool,
    warnings: Vec<String>,
    errors: Vec<String>,
    /// Individual check records for JSON output.
    entries: Vec<CheckEntry>,
    format: CheckFormat,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SqlxCacheCheck {
    Missing,
    Empty,
    Ready(usize),
}

impl CheckResult {
    fn new(format: CheckFormat) -> Self {
        Self {
            passed: true,
            warnings: Vec::new(),
            errors: Vec::new(),
            entries: Vec::new(),
            format,
        }
    }

    fn pass(&mut self, msg: &str) {
        if self.format == CheckFormat::Human {
            println!("  {} {}", ui::ok(), msg);
        }
        self.entries.push(CheckEntry {
            name: msg.to_string(),
            status: "ok",
            error: None,
        });
    }

    fn warn(&mut self, msg: &str, fix: &str) {
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

    fn fail(&mut self, msg: &str, fix: &str) {
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

    fn info(&mut self, msg: &str) {
        if self.format == CheckFormat::Human {
            println!("    {} {}", ui::info(), msg);
        }
    }

    fn section(&mut self, title: &str) {
        if self.format == CheckFormat::Human {
            println!();
            println!("  {} {}", ui::step(), style(title).bold());
        }
    }

    fn print_json(&self) {
        let status = if self.passed { "ok" } else { "error" };
        let checks: Vec<serde_json::Value> = self
            .entries
            .iter()
            .map(|e| {
                let mut obj = serde_json::Map::new();
                obj.insert("name".to_string(), json!(e.name));
                obj.insert("status".to_string(), json!(e.status));
                if let Some(err) = &e.error {
                    obj.insert("error".to_string(), json!(err));
                }
                serde_json::Value::Object(obj)
            })
            .collect();
        let payload = json!({ "status": status, "checks": checks });
        println!("{}", payload);
    }
}

impl CheckCommand {
    /// Execute the check command.
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

        // Auto-refresh the offline cache before downstream checks so cache-miss
        // noise doesn't bury real type errors. `--no-prepare` opts out (CI),
        // `--prepare-only` exits after this step.
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

    fn check_forge_toml(&self, result: &mut CheckResult) -> Result<()> {
        let config_path = Path::new(&self.config);

        if !config_path.exists() {
            result.fail(
                "forge.toml not found",
                "Create a new project with: forge new my-app --template with-svelte/minimal",
            );
            return Ok(());
        }

        let content = std::fs::read_to_string(config_path)?;
        let content = forge_core::config::substitute_env_vars(&content);
        let config: toml::Value = match toml::from_str(&content) {
            Ok(c) => {
                result.pass("forge.toml is valid TOML");
                c
            }
            Err(e) => {
                result.fail(
                    &format!("forge.toml parse error: {}", e),
                    "Fix the TOML syntax errors in forge.toml",
                );
                return Ok(());
            }
        };

        // Check [project] section
        if let Some(project) = config.get("project") {
            if project.get("name").is_some() {
                result.pass("[project] section configured");
            } else {
                result.warn(
                    "[project].name missing",
                    "Add name = \"your-app\" to [project] section",
                );
            }
        } else {
            result.fail(
                "[project] section missing",
                "Add [project] section with name to forge.toml",
            );
        }

        // Check [database] section
        if let Some(db) = config.get("database") {
            if let Some(url) = db.get("url").and_then(|v| v.as_str()) {
                if url.starts_with("${") || url.starts_with("postgres://") {
                    result.pass("[database] configured");
                } else {
                    result.warn(
                        "[database].url format looks incorrect",
                        "Use postgres://user:pass@host:port/db or ${DATABASE_URL}",
                    );
                }
            } else {
                result.warn(
                    "[database].url not set",
                    "Add url = \"${DATABASE_URL}\" to [database]",
                );
            }
        } else {
            result.fail(
                "[database] section missing",
                "Add [database] section with url to forge.toml",
            );
        }

        // Check [gateway] section
        if let Some(gateway) = config.get("gateway")
            && let Some(port) = gateway.get("port")
            && let Some(p) = port.as_integer()
        {
            if (1..=65535).contains(&p) {
                result.pass(&format!("[gateway] configured (port {})", p));
            } else {
                result.fail(
                    &format!("[gateway].port {} is out of range", p),
                    "Use a port between 1 and 65535",
                );
            }
        }

        // Strict-shape parse: catches half-set TLS, OAuth-without-secret,
        // file-size-exceeds-body-size, and other cross-field invariants that
        // the loose `toml::Value` walk above doesn't see. Without this,
        // `forge check` would silently accept configs that startup later
        // rejects.
        //
        // When env vars are unresolved (e.g. ${JWT_SECRET} not set in CI),
        // validation may reject placeholder values. Downgrade to a warning
        // so `forge check` remains useful in environments without secrets.
        let has_unresolved_vars = content.contains("${");
        match forge_core::config::ForgeConfig::parse_toml(&content) {
            Ok(_) => result.pass("forge.toml passed strict validation"),
            Err(e) if has_unresolved_vars => result.warn(
                &format!("forge.toml validation skipped (unresolved env vars): {}", e),
                "Set the referenced environment variables for full validation",
            ),
            Err(e) => result.fail(
                &format!("forge.toml validation failed: {}", e),
                "Fix the configuration error reported above",
            ),
        }

        // Check [observability] section: warn on full sampling.
        if let Some(obs) = config.get("observability")
            && let Some(ratio) = obs.get("sampling_ratio").and_then(|v| v.as_float())
            && ratio >= 1.0
        {
            result.warn(
                &format!(
                    "[observability].sampling_ratio = {ratio} sends every span to OTLP"
                ),
                "Lower to 0.05-0.1 in production builds; full sampling can saturate the collector and inflate cost",
            );
        }

        Ok(())
    }

    fn check_cargo_toml(&self, result: &mut CheckResult) -> Result<()> {
        let cargo_path = Path::new("Cargo.toml");

        if !cargo_path.exists() {
            result.fail(
                "Cargo.toml not found",
                "This doesn't appear to be a Rust project",
            );
            return Ok(());
        }

        let content = std::fs::read_to_string(cargo_path)?;
        let cargo: toml::Value = match toml::from_str(&content) {
            Ok(c) => c,
            Err(e) => {
                result.fail(
                    &format!("Cargo.toml parse error: {}", e),
                    "Fix the TOML syntax errors in Cargo.toml",
                );
                return Ok(());
            }
        };

        // Check for forge/forgex dependency
        let has_forge_dep = cargo
            .get("dependencies")
            .and_then(|deps| deps.get("forge").or_else(|| deps.get("forgex")))
            .is_some();

        if has_forge_dep {
            result.pass("forge dependency found in Cargo.toml");
        } else {
            result.fail(
                "forge dependency not found",
                &format!(
                    "Add forge = {{ version = \"{}\", package = \"forgex\" }} to [dependencies]",
                    env!("CARGO_PKG_VERSION")
                ),
            );
        }

        Ok(())
    }

    fn check_directory_structure(&self, result: &mut CheckResult) {
        let dirs = [
            ("src/", "Source directory"),
            ("src/schema/", "Schema directory"),
            ("src/functions/", "Functions directory"),
            ("migrations/", "Migrations directory"),
        ];

        for (dir, name) in dirs {
            if Path::new(dir).exists() {
                result.pass(&format!("{} exists", name));
            } else {
                result.fail(
                    &format!("{} missing", name),
                    &format!("Create {} directory", dir),
                );
            }
        }
    }

    fn check_migrations(&self, result: &mut CheckResult) -> Result<()> {
        let migrations_dir = Path::new("migrations");
        if !migrations_dir.exists() {
            return Ok(());
        }

        let mut migration_count = 0;
        let mut valid_count = 0;
        let mut issues = Vec::new();

        for entry in std::fs::read_dir(migrations_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().is_some_and(|ext| ext == "sql") {
                migration_count += 1;
                let Some(file_name) = path.file_name() else {
                    continue;
                };
                let filename = file_name.to_string_lossy();

                // Check naming convention: NNNN_name.sql
                let name_valid = filename
                    .split('_')
                    .next()
                    .map(|prefix| prefix.chars().all(|c| c.is_ascii_digit()))
                    .unwrap_or(false);

                if !name_valid {
                    issues.push(format!("{} - should be NNNN_name.sql", filename));
                    continue;
                }

                // Check for @up marker
                let content = std::fs::read_to_string(&path)?;
                if content.contains("-- @up") {
                    valid_count += 1;
                } else {
                    issues.push(format!("{} - missing '-- @up' marker", filename));
                }
            }
        }

        if migration_count == 0 {
            result.warn(
                "No migration files found",
                "Create migrations/0001_initial.sql with schema",
            );
        } else if issues.is_empty() {
            result.pass(&format!("{} migration file(s) valid", valid_count));
        } else {
            result.warn(
                &format!(
                    "{}/{} migrations have issues",
                    issues.len(),
                    migration_count
                ),
                "Fix migration file naming or add '-- @up' marker",
            );
            for issue in issues.iter().take(3) {
                result.info(issue);
            }
            if issues.len() > 3 {
                result.info(&format!("... and {} more", issues.len() - 3));
            }
        }

        Ok(())
    }

    fn check_functions(&self, result: &mut CheckResult) -> Result<()> {
        let functions_dir = Path::new("src/functions");
        if !functions_dir.exists() {
            return Ok(());
        }

        let mod_file = functions_dir.join("mod.rs");
        if !mod_file.exists() {
            result.fail(
                "src/functions/mod.rs not found",
                "Create mod.rs to export your functions",
            );
            return Ok(());
        }

        // Count function files and check for forge macros
        let mut function_count = 0;
        let mut macro_count = 0;

        for entry in std::fs::read_dir(functions_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().is_some_and(|ext| ext == "rs") {
                let Some(file_name) = path.file_name() else {
                    continue;
                };
                if file_name == "mod.rs" {
                    continue;
                }

                function_count += 1;
                let content = std::fs::read_to_string(&path)?;

                // Check for any forge macro
                if content.contains("#[forge::query")
                    || content.contains("#[forge::mutation")
                    || content.contains("#[forge::webhook")
                    || content.contains("#[forge::daemon")
                    || content.contains("#[forge::mcp_tool")
                    || content.contains("#[forge::job")
                    || content.contains("#[forge::cron")
                    || content.contains("#[forge::workflow")
                {
                    macro_count += 1;
                }
            }
        }

        if function_count == 0 {
            result.warn(
                "No function files found",
                "Create handlers in src/functions/ with #[forge::*] macros, then run forge generate",
            );
        } else if macro_count == function_count {
            result.pass(&format!(
                "{} function file(s) with forge macros",
                macro_count
            ));
        } else {
            result.warn(
                &format!("{}/{} files have forge macros", macro_count, function_count),
                "Ensure all function files use #[forge::*] macros",
            );
        }

        // Duplicate handler name check
        match find_duplicate_handlers(functions_dir) {
            Ok(dupes) if dupes.is_empty() => {}
            Ok(dupes) => {
                for (key, paths) in &dupes {
                    let (kind, name) = key.split_once(':').unwrap_or(("handler", key));
                    let file_list = paths
                        .iter()
                        .filter_map(|p| p.to_str())
                        .collect::<Vec<_>>()
                        .join(", ");
                    result.fail(
                        &format!("Duplicate {} name \"{name}\"", kind),
                        &format!(
                            "Found in: {file_list}. Use name = \"...\" in the macro attribute or rename one of the functions.",
                        ),
                    );
                }
            }
            Err(e) => {
                result.warn(
                    "Could not scan for duplicate handler names",
                    &format!("Parse error: {e}"),
                );
            }
        }

        Ok(())
    }

    fn check_schema(&self, result: &mut CheckResult) -> Result<()> {
        let schema_dir = Path::new("src/schema");
        if !schema_dir.exists() {
            return Ok(());
        }

        let mod_file = schema_dir.join("mod.rs");
        if !mod_file.exists() {
            result.fail(
                "src/schema/mod.rs not found",
                "Create mod.rs to export your models",
            );
            return Ok(());
        }

        // Count model files and check for forge::model or standard derive patterns
        let mut model_count = 0;
        let mut forge_model_count = 0;
        let mut derive_count = 0;

        for entry in std::fs::read_dir(schema_dir)? {
            let entry = entry?;
            let path = entry.path();

            if path.extension().is_some_and(|ext| ext == "rs") {
                let Some(file_name) = path.file_name() else {
                    continue;
                };
                if file_name == "mod.rs" {
                    continue;
                }

                model_count += 1;
                let content = std::fs::read_to_string(&path)?;

                if content.contains("#[forge::model") {
                    forge_model_count += 1;
                } else if content.contains("Serialize") || content.contains("FromRow") {
                    derive_count += 1;
                }
            }
        }

        let recognized = forge_model_count + derive_count;

        if model_count == 0 {
            result.warn(
                "No schema files found",
                "Create models in src/schema/, then run forge generate",
            );
        } else if recognized == model_count {
            if forge_model_count > 0 {
                result.pass(&format!(
                    "{} model file(s) with #[forge::model]",
                    forge_model_count
                ));
            }
            if derive_count > 0 {
                result.pass(&format!(
                    "{} model file(s) with standard derives (Serialize, FromRow)",
                    derive_count
                ));
            }
        } else {
            result.warn(
                &format!(
                    "{}/{} schema files have model definitions",
                    recognized, model_count
                ),
                "Add #[forge::model] or #[derive(Serialize, Deserialize, sqlx::FromRow)] to model structs",
            );
        }

        Ok(())
    }

    fn check_system_table_writes(&self, result: &mut CheckResult) -> Result<()> {
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

    fn refresh_sqlx_cache_if_stale(&self, result: &mut CheckResult) -> Result<()> {
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

        let has_cargo_sqlx = super::project_root::cargo_sqlx_available();
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

    fn check_sqlx_cache(&self, result: &mut CheckResult) -> Result<()> {
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

        // Warn if migrations are newer than cache
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

        // Check sqlx.toml
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

    fn check_frontend(&self, result: &mut CheckResult) -> Result<()> {
        let frontend_dir = Path::new("frontend");
        if !frontend_dir.exists() {
            result.info("No frontend/ directory (backend-only project)");
            return Ok(());
        }

        println!();
        result.pass("frontend/ directory exists");
        let target = FrontendTarget::detect(frontend_dir).unwrap_or(FrontendTarget::SvelteKit);

        match target {
            FrontendTarget::SvelteKit => {
                let package_json = frontend_dir.join("package.json");
                if !package_json.exists() {
                    result.fail(
                        "frontend/package.json not found",
                        "Run 'cd frontend && bun init' to initialize",
                    );
                    return Ok(());
                }

                let content = std::fs::read_to_string(&package_json)?;
                let package: serde_json::Value = match serde_json::from_str(&content) {
                    Ok(p) => p,
                    Err(e) => {
                        result.fail(
                            &format!("package.json parse error: {}", e),
                            "Fix JSON syntax in package.json",
                        );
                        return Ok(());
                    }
                };

                let has_svelte = package
                    .get("devDependencies")
                    .or_else(|| package.get("dependencies"))
                    .and_then(|deps| deps.get("svelte"))
                    .is_some();

                if has_svelte {
                    result.pass("Svelte dependency found");
                } else {
                    result.warn(
                        "Svelte not found in dependencies",
                        "This might not be a FORGE frontend project",
                    );
                }

                if frontend_dir.join("node_modules").exists() {
                    result.pass("Frontend dependencies installed");
                } else {
                    result.warn(
                        "Frontend dependencies not installed",
                        "Run 'cd frontend && bun install'",
                    );
                }
            }
            FrontendTarget::Dioxus => {
                if frontend_dir.join("Cargo.toml").exists() {
                    result.pass("Dioxus Cargo.toml found");
                } else {
                    result.fail(
                        "frontend/Cargo.toml not found",
                        "Add a Dioxus frontend crate in frontend/",
                    );
                }

                if frontend_dir.join("Dioxus.toml").exists() {
                    result.pass("Dioxus.toml found");
                } else {
                    result.fail(
                        "frontend/Dioxus.toml not found",
                        "Create frontend/Dioxus.toml for dx build/serve",
                    );
                }
            }
        }

        Ok(())
    }

    fn check_generated_bindings(&self, result: &mut CheckResult) -> Result<()> {
        let frontend_dir = Path::new("frontend");
        if !frontend_dir.exists() {
            result.info("No frontend/ directory, skipping binding check");
            return Ok(());
        }

        let target = FrontendTarget::detect(frontend_dir).unwrap_or(FrontendTarget::SvelteKit);
        let output_dir = target.default_output_dir();
        let output_path = Path::new(output_dir);

        if !output_path.exists() {
            result.warn(
                "Generated bindings directory not found",
                &format!("Run 'forge generate' to create {}", output_dir),
            );
            return Ok(());
        }

        let src_path = Path::new("src");
        let registry = if src_path.exists() {
            match forge_codegen::parse_project(src_path) {
                Ok(r) => r,
                Err(e) => {
                    result.warn(
                        &format!("Could not parse source: {}", e),
                        "Fix source errors and re-run",
                    );
                    return Ok(());
                }
            }
        } else {
            forge_core::schema::SchemaRegistry::new()
        };

        if let Err(errors) = forge_codegen::validate_registry(&registry) {
            result.fail(
                &format!(
                    "Unsupported types in handler signatures ({} found)",
                    errors.len()
                ),
                &errors.join("; "),
            );
            return Ok(());
        }

        let has_schema = !registry.all_tables().is_empty()
            || !registry.all_enums().is_empty()
            || !registry.all_functions().is_empty();

        let tmp_dir = frontend_dir.join(format!("forge-check-{}", std::process::id()));
        let tmp_output = tmp_dir.join("bindings");
        std::fs::create_dir_all(&tmp_output)?;
        let tmp_output_str = tmp_output.to_string_lossy().to_string();

        let gen_result = target.generate_bindings(&BindingGeneratorInput {
            output_dir: &tmp_output_str,
            output_path: &tmp_output,
            registry: &registry,
            has_schema,
            force: true,
        });

        let cleanup = || {
            let _ = std::fs::remove_dir_all(&tmp_dir);
        };

        if let Err(e) = gen_result {
            cleanup();
            result.warn(
                &format!("Could not regenerate bindings: {}", e),
                "Run 'forge generate' to check manually",
            );
            return Ok(());
        }

        if let Err(e) =
            format_generated_bindings_for_check(target, frontend_dir, output_path, &tmp_output)
        {
            cleanup();
            result.warn(
                &format!("Could not format regenerated bindings: {}", e),
                "Run 'forge generate --force' to restore generated bindings",
            );
            return Ok(());
        }

        let mut modified = Vec::new();
        let mut missing = Vec::new();

        if let Ok(entries) = std::fs::read_dir(&tmp_output) {
            for entry in entries.flatten() {
                if !entry.file_type().map(|t| t.is_file()).unwrap_or(false) {
                    continue;
                }
                let filename = entry.file_name();
                let Ok(expected) = std::fs::read(entry.path()) else {
                    continue;
                };
                let on_disk = output_path.join(&filename);

                if !on_disk.exists() {
                    missing.push(filename.to_string_lossy().to_string());
                    continue;
                }

                let Ok(actual) = std::fs::read(&on_disk) else {
                    missing.push(filename.to_string_lossy().to_string());
                    continue;
                };

                if actual != expected {
                    modified.push(filename.to_string_lossy().to_string());
                }
            }
        }

        cleanup();

        if modified.is_empty() && missing.is_empty() {
            result.pass("Generated bindings are up to date");
        } else {
            if !modified.is_empty() {
                result.warn(
                    &format!(
                        "{} binding file(s) modified: {}",
                        modified.len(),
                        modified.join(", ")
                    ),
                    "Run 'forge generate --force' to restore generated bindings",
                );
            }
            if !missing.is_empty() {
                result.warn(
                    &format!(
                        "{} binding file(s) missing: {}",
                        missing.len(),
                        missing.join(", ")
                    ),
                    "Run 'forge generate' to recreate missing bindings",
                );
            }
        }

        Ok(())
    }

    async fn check_rust_linting(&self, result: &mut CheckResult) {
        println!();

        // Check cargo fmt
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

        // Check cargo clippy
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

    async fn check_frontend_linting(&self, result: &mut CheckResult) {
        let frontend_dir = Path::new("frontend");
        if !frontend_dir.exists() {
            return;
        }
        let target = FrontendTarget::detect(frontend_dir).unwrap_or(FrontendTarget::SvelteKit);

        println!();

        if target == FrontendTarget::Dioxus {
            // Use rustfmt directly to avoid cargo fmt dependency resolution issues
            let mut rs_files = Vec::new();
            if let Ok(entries) = std::fs::read_dir(frontend_dir.join("src")) {
                collect_rs_files(entries, &mut rs_files);
            }

            if !rs_files.is_empty() {
                let mut cmd = TokioCommand::new("rustfmt");
                cmd.args(["--check", "--edition", "2024"]);
                for f in &rs_files {
                    cmd.arg(f);
                }
                let fmt_result = cmd
                    .stdout(Stdio::null())
                    .stderr(Stdio::null())
                    .status()
                    .await;

                match fmt_result {
                    Ok(status) if status.success() => result.pass("Dioxus rustfmt check passed"),
                    Ok(_) => result.fail(
                        "Dioxus frontend formatting issues found",
                        "Run 'rustfmt --edition 2024 frontend/src/**/*.rs'",
                    ),
                    Err(_) => result.warn("Could not run rustfmt", "Ensure rustfmt is installed"),
                }
            }
        }

        if !frontend_dir.join("node_modules").exists() {
            return;
        }

        if target == FrontendTarget::SvelteKit {
            let eslint_result = TokioCommand::new("bunx")
                .args(["eslint", "."])
                .current_dir(frontend_dir)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .status()
                .await;

            match eslint_result {
                Ok(status) if status.success() => result.pass("ESLint check passed"),
                Ok(_) => result.fail(
                    "ESLint errors found",
                    "Run 'cd frontend && bunx eslint .' to see errors",
                ),
                Err(_) => result.warn(
                    "Could not run ESLint",
                    "Ensure eslint is installed in frontend/",
                ),
            }
        }

        let prettier_result = TokioCommand::new("bunx")
            .args(["prettier", "--check", "."])
            .current_dir(frontend_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;

        match prettier_result {
            Ok(status) if status.success() => {
                result.pass("Prettier check passed");
            }
            Ok(_) => {
                result.fail(
                    "Prettier formatting issues found",
                    "Run 'cd frontend && bun run format' to fix",
                );
            }
            Err(_) => {
                result.warn(
                    "Could not run Prettier check",
                    "Ensure prettier is installed in frontend/",
                );
            }
        }
    }
}

/// Decide if `.sqlx/` is stale relative to source. Returns `Some(reason)` if so.
fn sqlx_cache_staleness(sqlx_dir: &Path, src_dir: &Path) -> Result<Option<String>> {
    if !sqlx_dir.exists() {
        return Ok(Some(".sqlx/ missing".to_string()));
    }

    let entries: Vec<_> = match std::fs::read_dir(sqlx_dir) {
        Ok(it) => it.flatten().collect(),
        Err(e) => return Ok(Some(format!(".sqlx/ unreadable: {e}"))),
    };
    if entries.is_empty() {
        return Ok(Some(".sqlx/ empty".to_string()));
    }

    let cache_oldest = entries
        .iter()
        .filter(|e| e.file_name().to_string_lossy().starts_with("query-"))
        .filter_map(|e| e.metadata().ok())
        .filter_map(|m| m.modified().ok())
        .min();

    if cache_oldest.is_none() {
        return Ok(Some(".sqlx/ has no query entries".to_string()));
    }

    let mut newest_src: Option<std::time::SystemTime> = None;
    let mut stack = vec![src_dir.to_path_buf()];
    while let Some(path) = stack.pop() {
        let Ok(meta) = std::fs::metadata(&path) else {
            continue;
        };
        if meta.is_dir() {
            if let Ok(it) = std::fs::read_dir(&path) {
                for e in it.flatten() {
                    stack.push(e.path());
                }
            }
        } else if path.extension().and_then(|s| s.to_str()) == Some("rs")
            && let Ok(modified) = meta.modified()
        {
            newest_src = Some(newest_src.map(|n| n.max(modified)).unwrap_or(modified));
        }
    }

    if let (Some(src), Some(cache)) = (newest_src, cache_oldest)
        && src > cache
    {
        return Ok(Some("Rust source newer than .sqlx/".to_string()));
    }

    Ok(None)
}

/// Resolve `DATABASE_URL`, preferring the env var, then `forge.toml [database].url`
/// (with `${VAR}` substitution applied).
fn resolve_database_url(config_path: &str) -> Result<String> {
    if let Ok(url) = std::env::var("DATABASE_URL")
        && !url.is_empty()
    {
        return Ok(url);
    }
    let path = Path::new(config_path);
    if !path.exists() {
        anyhow::bail!("DATABASE_URL not set and {} not found", config_path);
    }
    let cfg = forge_core::config::ForgeConfig::from_file(config_path)
        .map_err(|e| anyhow::anyhow!("failed to load {config_path}: {e}"))?;
    Ok(cfg.database.url().to_string())
}

fn project_uses_compile_time_sqlx_macros(src_dir: &Path) -> Result<bool> {
    if !src_dir.exists() {
        return Ok(false);
    }

    for entry in std::fs::read_dir(src_dir)? {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;

        if file_type.is_dir() {
            if project_uses_compile_time_sqlx_macros(&path)? {
                return Ok(true);
            }
            continue;
        }

        if !file_type.is_file() || path.extension().is_none_or(|ext| ext != "rs") {
            continue;
        }

        let content = std::fs::read_to_string(&path)?;
        if file_uses_sqlx_macros(&content) {
            return Ok(true);
        }
    }

    Ok(false)
}

fn file_uses_sqlx_macros(content: &str) -> bool {
    const NEEDLES: &[&str] = &[
        "sqlx::query!(",
        "sqlx::query_as!(",
        "sqlx::query_scalar!(",
        "sqlx::query_file!(",
        "sqlx::query_file_as!(",
    ];
    content.lines().any(|line| {
        let code = match line.split_once("//") {
            Some((before, _)) => before,
            None => line,
        };
        NEEDLES.iter().any(|needle| code.contains(needle))
    })
}

fn inspect_sqlx_cache(sqlx_dir: &Path) -> Result<SqlxCacheCheck> {
    if !sqlx_dir.exists() {
        return Ok(SqlxCacheCheck::Missing);
    }

    let query_file_count = std::fs::read_dir(sqlx_dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.file_name().to_string_lossy().starts_with("query-"))
        .count();

    if query_file_count == 0 {
        Ok(SqlxCacheCheck::Empty)
    } else {
        Ok(SqlxCacheCheck::Ready(query_file_count))
    }
}

fn format_generated_bindings_for_check(
    target: FrontendTarget,
    frontend_dir: &Path,
    output_path: &Path,
    tmp_output: &Path,
) -> Result<()> {
    if target != FrontendTarget::SvelteKit {
        return Ok(());
    }

    if generated_bindings_are_prettier_ignored(frontend_dir, output_path)? {
        return Ok(());
    }

    let prettier_target = tmp_output
        .canonicalize()
        .unwrap_or_else(|_| tmp_output.to_path_buf());

    let local_prettier = frontend_dir
        .join("node_modules/.bin/prettier")
        .canonicalize()
        .ok();
    let mut prettier = if let Some(local_prettier) = local_prettier {
        let mut cmd = StdCommand::new(local_prettier);
        cmd.arg("--write");
        cmd
    } else {
        let mut cmd = StdCommand::new("bunx");
        cmd.args(["prettier", "--write"]);
        cmd
    };

    let status = prettier
        .arg(prettier_target.to_string_lossy().to_string())
        .current_dir(frontend_dir)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;

    if status.success() {
        Ok(())
    } else {
        anyhow::bail!("bunx prettier --write failed for temporary generated bindings")
    }
}

fn generated_bindings_are_prettier_ignored(
    frontend_dir: &Path,
    output_path: &Path,
) -> Result<bool> {
    let ignore_path = frontend_dir.join(".prettierignore");
    if !ignore_path.exists() {
        return Ok(false);
    }

    let relative_output = output_path
        .strip_prefix(frontend_dir)
        .unwrap_or(output_path)
        .to_string_lossy()
        .replace('\\', "/");
    let content = std::fs::read_to_string(ignore_path)?;

    for line in content.lines() {
        let pattern = line.trim().trim_end_matches('/');
        if pattern.is_empty() || pattern.starts_with('#') {
            continue;
        }

        if relative_output == pattern || relative_output.starts_with(&format!("{pattern}/")) {
            return Ok(true);
        }
    }

    Ok(false)
}

const RESERVED_SYSTEM_TABLES: &[&str] = &[
    "forge_jobs",
    "forge_workflow_runs",
    "forge_workflow_definitions",
    "forge_cron_runs",
    "forge_migrations",
    "forge_sessions",
    "forge_refresh_tokens",
    "forge_signals_events",
];

fn scan_system_table_writes(
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

fn collect_rs_files(entries: std::fs::ReadDir, out: &mut Vec<std::path::PathBuf>) {
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

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
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
}

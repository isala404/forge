use anyhow::Result;
use std::path::Path;

use forge_codegen::find_duplicate_handlers;

use super::{CheckCommand, CheckResult};

impl CheckCommand {
    pub(super) fn check_directory_structure(&self, result: &mut CheckResult) {
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

    pub(super) fn check_migrations(&self, result: &mut CheckResult) -> Result<()> {
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

    pub(super) fn check_functions(&self, result: &mut CheckResult) -> Result<()> {
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

    pub(super) fn check_schema(&self, result: &mut CheckResult) -> Result<()> {
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
}

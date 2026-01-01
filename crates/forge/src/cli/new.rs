use anyhow::Result;
use clap::Parser;
use console::style;
use std::fs;
use std::path::Path;

use super::template::render;
use crate::template_vars;

// Project templates
const CARGO_TOML: &str = include_str!("../../templates/project/Cargo.toml.tmpl");
const FORGE_TOML: &str = include_str!("../../templates/project/forge.toml.tmpl");
const MAIN_RS: &str = include_str!("../../templates/project/main.rs.tmpl");
const GITIGNORE: &str = include_str!("../../templates/project/gitignore.tmpl");
const ENV: &str = include_str!("../../templates/project/env.tmpl");
const MIGRATION_INITIAL: &str =
    include_str!("../../templates/project/migrations/0001_initial.sql.tmpl");
const SCHEMA_MOD: &str = include_str!("../../templates/project/schema/mod.rs.tmpl");
const SCHEMA_USER: &str = include_str!("../../templates/project/schema/user.rs.tmpl");
const FUNCTIONS_MOD: &str = include_str!("../../templates/project/functions/mod.rs.tmpl");
const FUNCTIONS_USERS: &str = include_str!("../../templates/project/functions/users.rs.tmpl");
const FUNCTIONS_APP_STATS: &str =
    include_str!("../../templates/project/functions/app_stats.rs.tmpl");
const FUNCTIONS_EXPORT_USERS_JOB: &str =
    include_str!("../../templates/project/functions/export_users_job.rs.tmpl");
const FUNCTIONS_HEARTBEAT_CRON: &str =
    include_str!("../../templates/project/functions/heartbeat_stats_cron.rs.tmpl");
const FUNCTIONS_VERIFICATION_WORKFLOW: &str =
    include_str!("../../templates/project/functions/account_verification_workflow.rs.tmpl");
const FUNCTIONS_SEND_WELCOME_ACTION: &str =
    include_str!("../../templates/project/functions/send_welcome_action.rs.tmpl");
const FUNCTIONS_TESTS: &str = include_str!("../../templates/project/functions/tests.rs.tmpl");

// Frontend templates
const FRONTEND_PACKAGE_JSON: &str = include_str!("../../templates/frontend/package.json.tmpl");
const FRONTEND_SVELTE_CONFIG: &str = include_str!("../../templates/frontend/svelte.config.js.tmpl");
const FRONTEND_VITE_CONFIG: &str = include_str!("../../templates/frontend/vite.config.ts.tmpl");
const FRONTEND_TSCONFIG: &str = include_str!("../../templates/frontend/tsconfig.json.tmpl");
const FRONTEND_APP_HTML: &str = include_str!("../../templates/frontend/app.html.tmpl");
const FRONTEND_ENV_EXAMPLE: &str = include_str!("../../templates/frontend/env.tmpl");
const FRONTEND_LAYOUT_SVELTE: &str =
    include_str!("../../templates/frontend/routes/layout.svelte.tmpl");
const FRONTEND_LAYOUT_TS: &str = include_str!("../../templates/frontend/routes/layout.ts.tmpl");
const FRONTEND_PAGE_SVELTE: &str = include_str!("../../templates/frontend/routes/page.svelte.tmpl");
const FRONTEND_TYPES_TS: &str = include_str!("../../templates/frontend/lib/forge/types.ts.tmpl");
const FRONTEND_API_TS: &str = include_str!("../../templates/frontend/lib/forge/api.ts.tmpl");
const FRONTEND_INDEX_TS: &str = include_str!("../../templates/frontend/lib/forge/index.ts.tmpl");
const FRONTEND_PRETTIERIGNORE: &str = include_str!("../../templates/frontend/prettierignore.tmpl");
const FRONTEND_PRETTIERRC: &str = include_str!("../../templates/frontend/prettierrc.tmpl");

/// Create a new FORGE project.
#[derive(Parser)]
pub struct NewCommand {
    /// Project name.
    pub name: String,

    /// Use minimal template (no frontend).
    #[arg(long)]
    pub minimal: bool,

    /// Output directory (defaults to project name).
    #[arg(short, long)]
    pub output: Option<String>,
}

impl NewCommand {
    /// Execute the new project command.
    pub async fn execute(self) -> Result<()> {
        let project_dir = self.output.as_ref().unwrap_or(&self.name);
        let path = Path::new(project_dir);

        if path.exists() {
            anyhow::bail!("Directory already exists: {}", project_dir);
        }

        fs::create_dir_all(path)?;
        create_project(path, &self.name, self.minimal)?;

        println!();
        println!(
            "{} Created new FORGE project: {}",
            style("✅").green(),
            style(&self.name).cyan()
        );
        println!();
        println!("Next steps:");
        println!("  {} {}", style("cd").dim(), project_dir);
        println!("  {} to start the server", style("cargo run").dim());
        if !self.minimal {
            println!(
                "  {} to start the frontend",
                style("cd frontend && bun dev").dim()
            );
        }
        println!();

        Ok(())
    }
}

/// Create project files in the given directory.
pub fn create_project(dir: &Path, name: &str, minimal: bool) -> Result<()> {
    let vars = template_vars!("name" => name);

    // Create directory structure
    fs::create_dir_all(dir.join("src/schema"))?;
    fs::create_dir_all(dir.join("src/functions"))?;
    fs::create_dir_all(dir.join("migrations"))?;

    // Write project files
    fs::write(dir.join("Cargo.toml"), render(CARGO_TOML, &vars))?;
    fs::write(dir.join("forge.toml"), render(FORGE_TOML, &vars))?;
    fs::write(dir.join("src/main.rs"), MAIN_RS)?;
    fs::write(dir.join(".gitignore"), GITIGNORE)?;
    fs::write(dir.join(".env"), ENV)?;
    fs::write(dir.join("migrations/0001_initial.sql"), MIGRATION_INITIAL)?;

    // Schema files
    fs::write(dir.join("src/schema/mod.rs"), SCHEMA_MOD)?;
    fs::write(dir.join("src/schema/user.rs"), SCHEMA_USER)?;

    // Function files
    fs::write(dir.join("src/functions/mod.rs"), FUNCTIONS_MOD)?;
    fs::write(dir.join("src/functions/users.rs"), FUNCTIONS_USERS)?;
    fs::write(dir.join("src/functions/app_stats.rs"), FUNCTIONS_APP_STATS)?;
    fs::write(
        dir.join("src/functions/export_users_job.rs"),
        FUNCTIONS_EXPORT_USERS_JOB,
    )?;
    fs::write(
        dir.join("src/functions/heartbeat_stats_cron.rs"),
        FUNCTIONS_HEARTBEAT_CRON,
    )?;
    fs::write(
        dir.join("src/functions/account_verification_workflow.rs"),
        FUNCTIONS_VERIFICATION_WORKFLOW,
    )?;
    fs::write(
        dir.join("src/functions/send_welcome_action.rs"),
        FUNCTIONS_SEND_WELCOME_ACTION,
    )?;
    fs::write(dir.join("src/functions/tests.rs"), FUNCTIONS_TESTS)?;

    // Create frontend if not minimal
    if !minimal {
        create_frontend(dir, name)?;
    }

    Ok(())
}

/// Create frontend scaffolding.
fn create_frontend(dir: &Path, name: &str) -> Result<()> {
    let vars = template_vars!("name" => name);

    let frontend_dir = dir.join("frontend");
    fs::create_dir_all(&frontend_dir)?;
    fs::create_dir_all(frontend_dir.join("src/routes"))?;
    fs::create_dir_all(frontend_dir.join("src/lib/forge"))?;

    // Write frontend files
    fs::write(
        frontend_dir.join("package.json"),
        render(FRONTEND_PACKAGE_JSON, &vars),
    )?;
    fs::write(
        frontend_dir.join("svelte.config.js"),
        FRONTEND_SVELTE_CONFIG,
    )?;
    fs::write(frontend_dir.join("vite.config.ts"), FRONTEND_VITE_CONFIG)?;
    fs::write(frontend_dir.join("tsconfig.json"), FRONTEND_TSCONFIG)?;
    fs::write(frontend_dir.join("src/app.html"), FRONTEND_APP_HTML)?;
    fs::write(frontend_dir.join(".env"), FRONTEND_ENV_EXAMPLE)?;
    fs::write(frontend_dir.join(".prettierignore"), FRONTEND_PRETTIERIGNORE)?;
    fs::write(frontend_dir.join(".prettierrc"), FRONTEND_PRETTIERRC)?;

    // Routes
    fs::write(
        frontend_dir.join("src/routes/+layout.svelte"),
        FRONTEND_LAYOUT_SVELTE,
    )?;
    fs::write(
        frontend_dir.join("src/routes/+layout.ts"),
        FRONTEND_LAYOUT_TS,
    )?;
    fs::write(
        frontend_dir.join("src/routes/+page.svelte"),
        FRONTEND_PAGE_SVELTE,
    )?;

    // Lib/forge
    fs::write(
        frontend_dir.join("src/lib/forge/types.ts"),
        FRONTEND_TYPES_TS,
    )?;
    fs::write(frontend_dir.join("src/lib/forge/api.ts"), FRONTEND_API_TS)?;
    fs::write(
        frontend_dir.join("src/lib/forge/index.ts"),
        FRONTEND_INDEX_TS,
    )?;

    // Generate @forge/svelte runtime package
    super::runtime_generator::generate_runtime(&frontend_dir)?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_create_project() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test-project");
        fs::create_dir_all(&path).unwrap();

        create_project(&path, "test-project", false).unwrap();

        assert!(path.join("Cargo.toml").exists());
        assert!(path.join("forge.toml").exists());
        assert!(path.join("src/main.rs").exists());
        assert!(path.join("src/schema/mod.rs").exists());
        assert!(path.join("frontend/package.json").exists());
        assert!(path.join("frontend/src/lib/forge/types.ts").exists());
        assert!(path.join("frontend/src/lib/forge/api.ts").exists());
        assert!(path.join("frontend/src/routes/+layout.ts").exists());
        assert!(path.join("migrations/0001_initial.sql").exists());
    }

    #[test]
    fn test_create_minimal_project() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test-minimal");
        fs::create_dir_all(&path).unwrap();

        create_project(&path, "test-minimal", true).unwrap();

        assert!(path.join("Cargo.toml").exists());
        assert!(!path.join("frontend").exists());
    }
}

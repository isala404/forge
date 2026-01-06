use anyhow::Result;
use clap::Parser;
use console::style;
use std::fs;
use std::path::Path;

use super::template::render;
use crate::template_vars;

// Populated project templates (default)
const CARGO_TOML: &str = include_str!("../../templates/populated/project/Cargo.toml.tmpl");
const FORGE_TOML: &str = include_str!("../../templates/populated/project/forge.toml.tmpl");
const MAIN_RS: &str = include_str!("../../templates/populated/project/main.rs.tmpl");
const BUILD_RS: &str = include_str!("../../templates/populated/project/build.rs.tmpl");
const GITIGNORE: &str = include_str!("../../templates/populated/project/gitignore.tmpl");
const ENV: &str = include_str!("../../templates/populated/project/env.tmpl");
const DOCKERFILE: &str = include_str!("../../templates/populated/project/Dockerfile.tmpl");
const DOCKER_COMPOSE: &str =
    include_str!("../../templates/populated/project/docker-compose.yml.tmpl");
const DOCKERIGNORE: &str = include_str!("../../templates/populated/project/dockerignore.tmpl");
const README: &str = include_str!("../../templates/populated/project/README.md.tmpl");
const MIGRATION_INITIAL: &str =
    include_str!("../../templates/populated/project/migrations/0001_initial.sql.tmpl");
const SCHEMA_MOD: &str = include_str!("../../templates/populated/project/schema/mod.rs.tmpl");
const SCHEMA_USER: &str = include_str!("../../templates/populated/project/schema/user.rs.tmpl");
const FUNCTIONS_MOD: &str =
    include_str!("../../templates/populated/project/functions/mod.rs.tmpl");
const FUNCTIONS_USERS: &str =
    include_str!("../../templates/populated/project/functions/users.rs.tmpl");
const FUNCTIONS_APP_STATS: &str =
    include_str!("../../templates/populated/project/functions/app_stats.rs.tmpl");
const FUNCTIONS_EXPORT_USERS_JOB: &str =
    include_str!("../../templates/populated/project/functions/export_users_job.rs.tmpl");
const FUNCTIONS_HEARTBEAT_CRON: &str =
    include_str!("../../templates/populated/project/functions/heartbeat_stats_cron.rs.tmpl");
const FUNCTIONS_VERIFICATION_WORKFLOW: &str =
    include_str!("../../templates/populated/project/functions/account_verification_workflow.rs.tmpl");
const FUNCTIONS_GET_BITCOIN_PRICE_ACTION: &str =
    include_str!("../../templates/populated/project/functions/get_bitcoin_price_action.rs.tmpl");
const CLAUDE_MD: &str = include_str!("../../templates/populated/project/CLAUDE.md.tmpl");

// Populated frontend templates (default)
const FRONTEND_PACKAGE_JSON: &str =
    include_str!("../../templates/populated/frontend/package.json.tmpl");
const FRONTEND_SVELTE_CONFIG: &str =
    include_str!("../../templates/populated/frontend/svelte.config.js.tmpl");
const FRONTEND_VITE_CONFIG: &str =
    include_str!("../../templates/populated/frontend/vite.config.ts.tmpl");
const FRONTEND_TSCONFIG: &str =
    include_str!("../../templates/populated/frontend/tsconfig.json.tmpl");
const FRONTEND_APP_HTML: &str =
    include_str!("../../templates/populated/frontend/app.html.tmpl");
const FRONTEND_ENV_EXAMPLE: &str = include_str!("../../templates/populated/frontend/env.tmpl");
const FRONTEND_LAYOUT_SVELTE: &str =
    include_str!("../../templates/populated/frontend/routes/layout.svelte.tmpl");
const FRONTEND_LAYOUT_TS: &str =
    include_str!("../../templates/populated/frontend/routes/layout.ts.tmpl");
const FRONTEND_PAGE_SVELTE: &str =
    include_str!("../../templates/populated/frontend/routes/page.svelte.tmpl");
const FRONTEND_TYPES_TS: &str =
    include_str!("../../templates/populated/frontend/lib/forge/types.ts.tmpl");
const FRONTEND_API_TS: &str =
    include_str!("../../templates/populated/frontend/lib/forge/api.ts.tmpl");
const FRONTEND_INDEX_TS: &str =
    include_str!("../../templates/populated/frontend/lib/forge/index.ts.tmpl");
const FRONTEND_PRETTIERIGNORE: &str =
    include_str!("../../templates/populated/frontend/prettierignore.tmpl");
const FRONTEND_PRETTIERRC: &str =
    include_str!("../../templates/populated/frontend/prettierrc.tmpl");
const FRONTEND_ESLINT_CONFIG: &str =
    include_str!("../../templates/populated/frontend/eslint.config.js.tmpl");

// Empty project templates (for --empty flag)
const EMPTY_CARGO_TOML: &str = include_str!("../../templates/empty/project/Cargo.toml.tmpl");
const EMPTY_FORGE_TOML: &str = include_str!("../../templates/empty/project/forge.toml.tmpl");
const EMPTY_MAIN_RS: &str = include_str!("../../templates/empty/project/main.rs.tmpl");
const EMPTY_BUILD_RS: &str = include_str!("../../templates/empty/project/build.rs.tmpl");
const EMPTY_GITIGNORE: &str = include_str!("../../templates/empty/project/gitignore.tmpl");
const EMPTY_ENV: &str = include_str!("../../templates/empty/project/env.tmpl");
const EMPTY_DOCKERFILE: &str = include_str!("../../templates/empty/project/Dockerfile.tmpl");
const EMPTY_DOCKER_COMPOSE: &str =
    include_str!("../../templates/empty/project/docker-compose.yml.tmpl");
const EMPTY_DOCKERIGNORE: &str = include_str!("../../templates/empty/project/dockerignore.tmpl");
const EMPTY_README: &str = include_str!("../../templates/empty/project/README.md.tmpl");
const EMPTY_MIGRATION_INITIAL: &str =
    include_str!("../../templates/empty/project/migrations/0001_initial.sql.tmpl");
const EMPTY_SCHEMA_MOD: &str = include_str!("../../templates/empty/project/schema/mod.rs.tmpl");
const EMPTY_FUNCTIONS_MOD: &str =
    include_str!("../../templates/empty/project/functions/mod.rs.tmpl");
const EMPTY_CLAUDE_MD: &str = include_str!("../../templates/empty/project/CLAUDE.md.tmpl");

// Empty frontend templates (for --empty flag)
const EMPTY_FRONTEND_PACKAGE_JSON: &str =
    include_str!("../../templates/empty/frontend/package.json.tmpl");
const EMPTY_FRONTEND_SVELTE_CONFIG: &str =
    include_str!("../../templates/empty/frontend/svelte.config.js.tmpl");
const EMPTY_FRONTEND_VITE_CONFIG: &str =
    include_str!("../../templates/empty/frontend/vite.config.ts.tmpl");
const EMPTY_FRONTEND_TSCONFIG: &str =
    include_str!("../../templates/empty/frontend/tsconfig.json.tmpl");
const EMPTY_FRONTEND_APP_HTML: &str =
    include_str!("../../templates/empty/frontend/app.html.tmpl");
const EMPTY_FRONTEND_ENV_EXAMPLE: &str =
    include_str!("../../templates/empty/frontend/env.tmpl");
const EMPTY_FRONTEND_LAYOUT_SVELTE: &str =
    include_str!("../../templates/empty/frontend/routes/layout.svelte.tmpl");
const EMPTY_FRONTEND_LAYOUT_TS: &str =
    include_str!("../../templates/empty/frontend/routes/layout.ts.tmpl");
const EMPTY_FRONTEND_PAGE_SVELTE: &str =
    include_str!("../../templates/empty/frontend/routes/page.svelte.tmpl");
const EMPTY_FRONTEND_TYPES_TS: &str =
    include_str!("../../templates/empty/frontend/lib/forge/types.ts.tmpl");
const EMPTY_FRONTEND_API_TS: &str =
    include_str!("../../templates/empty/frontend/lib/forge/api.ts.tmpl");
const EMPTY_FRONTEND_INDEX_TS: &str =
    include_str!("../../templates/empty/frontend/lib/forge/index.ts.tmpl");
const EMPTY_FRONTEND_PRETTIERIGNORE: &str =
    include_str!("../../templates/empty/frontend/prettierignore.tmpl");
const EMPTY_FRONTEND_PRETTIERRC: &str =
    include_str!("../../templates/empty/frontend/prettierrc.tmpl");
const EMPTY_FRONTEND_ESLINT_CONFIG: &str =
    include_str!("../../templates/empty/frontend/eslint.config.js.tmpl");

/// Create a new FORGE project.
#[derive(Parser)]
pub struct NewCommand {
    /// Project name.
    pub name: String,

    /// Use minimal template (no frontend).
    #[arg(long)]
    pub minimal: bool,

    /// Use empty template (no example code, just scaffolding).
    #[arg(long)]
    pub empty: bool,

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
        create_project(path, &self.name, self.minimal, self.empty)?;

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
pub fn create_project(dir: &Path, name: &str, minimal: bool, empty: bool) -> Result<()> {
    let vars = template_vars!("name" => name, "project_name" => name);

    // Create directory structure
    fs::create_dir_all(dir.join("src/schema"))?;
    fs::create_dir_all(dir.join("src/functions"))?;
    fs::create_dir_all(dir.join("migrations"))?;

    if empty {
        // Empty templates - minimal scaffolding without example code
        fs::write(dir.join("Cargo.toml"), render(EMPTY_CARGO_TOML, &vars))?;
        fs::write(dir.join("forge.toml"), render(EMPTY_FORGE_TOML, &vars))?;
        fs::write(dir.join("build.rs"), EMPTY_BUILD_RS)?;
        fs::write(dir.join(".gitignore"), EMPTY_GITIGNORE)?;
        fs::write(dir.join(".env"), render(EMPTY_ENV, &vars))?;
        fs::write(dir.join("Dockerfile"), render(EMPTY_DOCKERFILE, &vars))?;
        fs::write(
            dir.join("docker-compose.yml"),
            render(EMPTY_DOCKER_COMPOSE, &vars),
        )?;
        fs::write(dir.join(".dockerignore"), EMPTY_DOCKERIGNORE)?;
        fs::write(dir.join("README.md"), render(EMPTY_README, &vars))?;
        fs::write(dir.join("src/main.rs"), EMPTY_MAIN_RS)?;
        fs::write(
            dir.join("migrations/0001_initial.sql"),
            EMPTY_MIGRATION_INITIAL,
        )?;
        fs::write(dir.join("src/schema/mod.rs"), EMPTY_SCHEMA_MOD)?;
        fs::write(dir.join("src/functions/mod.rs"), EMPTY_FUNCTIONS_MOD)?;
        fs::write(dir.join("CLAUDE.md"), EMPTY_CLAUDE_MD)?;
    } else {
        // Populated templates - full example code
        fs::write(dir.join("Cargo.toml"), render(CARGO_TOML, &vars))?;
        fs::write(dir.join("forge.toml"), render(FORGE_TOML, &vars))?;
        fs::write(dir.join("build.rs"), BUILD_RS)?;
        fs::write(dir.join(".gitignore"), GITIGNORE)?;
        fs::write(dir.join(".env"), render(ENV, &vars))?;
        fs::write(dir.join("Dockerfile"), render(DOCKERFILE, &vars))?;
        fs::write(
            dir.join("docker-compose.yml"),
            render(DOCKER_COMPOSE, &vars),
        )?;
        fs::write(dir.join(".dockerignore"), DOCKERIGNORE)?;
        fs::write(dir.join("README.md"), render(README, &vars))?;
        fs::write(dir.join("src/main.rs"), MAIN_RS)?;
        fs::write(dir.join("migrations/0001_initial.sql"), MIGRATION_INITIAL)?;
        fs::write(dir.join("src/schema/mod.rs"), SCHEMA_MOD)?;
        fs::write(dir.join("src/schema/user.rs"), SCHEMA_USER)?;
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
            dir.join("src/functions/get_bitcoin_price_action.rs"),
            FUNCTIONS_GET_BITCOIN_PRICE_ACTION,
        )?;
        fs::write(dir.join("CLAUDE.md"), CLAUDE_MD)?;
    }

    // Create frontend if not minimal
    if !minimal {
        create_frontend(dir, name, empty)?;
    }

    Ok(())
}

/// Create frontend scaffolding.
fn create_frontend(dir: &Path, name: &str, empty: bool) -> Result<()> {
    let vars = template_vars!("name" => name, "project_name" => name);

    let frontend_dir = dir.join("frontend");
    fs::create_dir_all(&frontend_dir)?;
    fs::create_dir_all(frontend_dir.join("src/routes"))?;
    fs::create_dir_all(frontend_dir.join("src/lib/forge"))?;

    if empty {
        // Empty templates - minimal frontend
        fs::write(
            frontend_dir.join("package.json"),
            render(EMPTY_FRONTEND_PACKAGE_JSON, &vars),
        )?;
        fs::write(
            frontend_dir.join("svelte.config.js"),
            EMPTY_FRONTEND_SVELTE_CONFIG,
        )?;
        fs::write(
            frontend_dir.join("vite.config.ts"),
            EMPTY_FRONTEND_VITE_CONFIG,
        )?;
        fs::write(frontend_dir.join("tsconfig.json"), EMPTY_FRONTEND_TSCONFIG)?;
        fs::write(frontend_dir.join("src/app.html"), EMPTY_FRONTEND_APP_HTML)?;
        fs::write(frontend_dir.join(".env"), EMPTY_FRONTEND_ENV_EXAMPLE)?;
        fs::write(
            frontend_dir.join(".prettierignore"),
            EMPTY_FRONTEND_PRETTIERIGNORE,
        )?;
        fs::write(frontend_dir.join(".prettierrc"), EMPTY_FRONTEND_PRETTIERRC)?;
        fs::write(
            frontend_dir.join("eslint.config.js"),
            EMPTY_FRONTEND_ESLINT_CONFIG,
        )?;
        fs::write(
            frontend_dir.join("src/routes/+layout.svelte"),
            EMPTY_FRONTEND_LAYOUT_SVELTE,
        )?;
        fs::write(
            frontend_dir.join("src/routes/+layout.ts"),
            EMPTY_FRONTEND_LAYOUT_TS,
        )?;
        fs::write(
            frontend_dir.join("src/routes/+page.svelte"),
            render(EMPTY_FRONTEND_PAGE_SVELTE, &vars),
        )?;
        fs::write(
            frontend_dir.join("src/lib/forge/types.ts"),
            EMPTY_FRONTEND_TYPES_TS,
        )?;
        fs::write(
            frontend_dir.join("src/lib/forge/api.ts"),
            EMPTY_FRONTEND_API_TS,
        )?;
        fs::write(
            frontend_dir.join("src/lib/forge/index.ts"),
            EMPTY_FRONTEND_INDEX_TS,
        )?;
    } else {
        // Populated templates - full demo
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
        fs::write(
            frontend_dir.join(".prettierignore"),
            FRONTEND_PRETTIERIGNORE,
        )?;
        fs::write(frontend_dir.join(".prettierrc"), FRONTEND_PRETTIERRC)?;
        fs::write(
            frontend_dir.join("eslint.config.js"),
            FRONTEND_ESLINT_CONFIG,
        )?;
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
        fs::write(
            frontend_dir.join("src/lib/forge/types.ts"),
            FRONTEND_TYPES_TS,
        )?;
        fs::write(frontend_dir.join("src/lib/forge/api.ts"), FRONTEND_API_TS)?;
        fs::write(
            frontend_dir.join("src/lib/forge/index.ts"),
            FRONTEND_INDEX_TS,
        )?;
    }

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

        create_project(&path, "test-project", false, false).unwrap();

        assert!(path.join("Cargo.toml").exists());
        assert!(path.join("forge.toml").exists());
        assert!(path.join("src/main.rs").exists());
        assert!(path.join("build.rs").exists());
        assert!(path.join("src/schema/mod.rs").exists());
        assert!(path.join("src/schema/user.rs").exists());
        assert!(path.join("src/functions/users.rs").exists());
        assert!(path.join("frontend/package.json").exists());
        assert!(path.join("frontend/src/lib/forge/types.ts").exists());
        assert!(path.join("frontend/src/lib/forge/api.ts").exists());
        assert!(path.join("frontend/src/routes/+layout.ts").exists());
        assert!(path.join("frontend/eslint.config.js").exists());
        assert!(path.join("migrations/0001_initial.sql").exists());
        assert!(path.join("Dockerfile").exists());
        assert!(path.join("docker-compose.yml").exists());
        assert!(path.join(".dockerignore").exists());
        assert!(path.join("README.md").exists());
        assert!(path.join("CLAUDE.md").exists());
    }

    #[test]
    fn test_create_minimal_project() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test-minimal");
        fs::create_dir_all(&path).unwrap();

        create_project(&path, "test-minimal", true, false).unwrap();

        assert!(path.join("Cargo.toml").exists());
        assert!(path.join("CLAUDE.md").exists());
        assert!(!path.join("frontend").exists());
    }

    #[test]
    fn test_create_empty_project() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test-empty");
        fs::create_dir_all(&path).unwrap();

        create_project(&path, "test-empty", false, true).unwrap();

        // Core files should exist
        assert!(path.join("Cargo.toml").exists());
        assert!(path.join("forge.toml").exists());
        assert!(path.join("src/main.rs").exists());
        assert!(path.join("src/schema/mod.rs").exists());
        assert!(path.join("src/functions/mod.rs").exists());
        assert!(path.join("migrations/0001_initial.sql").exists());
        assert!(path.join("CLAUDE.md").exists());

        // Example files should NOT exist
        assert!(!path.join("src/schema/user.rs").exists());
        assert!(!path.join("src/functions/users.rs").exists());
        assert!(!path.join("src/functions/export_users_job.rs").exists());
        assert!(!path.join("src/functions/heartbeat_stats_cron.rs").exists());
        assert!(
            !path
                .join("src/functions/account_verification_workflow.rs")
                .exists()
        );

        // Frontend should still exist (not minimal)
        assert!(path.join("frontend/package.json").exists());
        assert!(path.join("frontend/src/lib/forge/types.ts").exists());
        assert!(path.join("frontend/src/lib/forge/api.ts").exists());
    }

    #[test]
    fn test_create_empty_minimal_project() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test-empty-minimal");
        fs::create_dir_all(&path).unwrap();

        create_project(&path, "test-empty-minimal", true, true).unwrap();

        // Core backend files should exist
        assert!(path.join("Cargo.toml").exists());
        assert!(path.join("forge.toml").exists());
        assert!(path.join("src/main.rs").exists());
        assert!(path.join("src/schema/mod.rs").exists());
        assert!(path.join("src/functions/mod.rs").exists());
        assert!(path.join("CLAUDE.md").exists());

        // Example files should NOT exist
        assert!(!path.join("src/schema/user.rs").exists());
        assert!(!path.join("src/functions/users.rs").exists());

        // No frontend (minimal)
        assert!(!path.join("frontend").exists());
    }
}

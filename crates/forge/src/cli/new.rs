use anyhow::Result;
use clap::Parser;
use console::style;
use std::fs;
use std::path::Path;
use std::process::Command;

use super::template::render;
use super::ui;
use crate::template_vars;

// In debug builds, embed the path to the forge source directory
#[cfg(debug_assertions)]
const FORGE_MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// Get the forge workspace root directory (only available in debug builds).
/// CARGO_MANIFEST_DIR points to crates/forge, so we go up two levels.
#[cfg(debug_assertions)]
fn get_forge_workspace_dir() -> Option<&'static str> {
    let manifest_dir = Path::new(FORGE_MANIFEST_DIR);
    // Go up from crates/forge to the workspace root
    manifest_dir.parent()?.parent()?.to_str()
}

/// Append cargo patch section to use local forge crates (only in debug builds).
#[cfg(debug_assertions)]
fn append_cargo_patch(cargo_toml_path: &Path) -> Result<()> {
    let workspace_dir = get_forge_workspace_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine forge workspace directory"))?;

    // Paths point to /forge because docker-compose mounts the workspace there
    let _ = workspace_dir;
    let patch_section = r#"
# Local dev patches (debug build) - remove before publishing
[patch.crates-io]
forgex = { path = "/forge/crates/forge" }
forge-core = { path = "/forge/crates/forge-core" }
forge-macros = { path = "/forge/crates/forge-macros" }
forge-runtime = { path = "/forge/crates/forge-runtime" }
forge-codegen = { path = "/forge/crates/forge-codegen" }
"#;

    let mut content = fs::read_to_string(cargo_toml_path)?;
    content.push_str(patch_section);
    fs::write(cargo_toml_path, content)?;

    Ok(())
}

/// Add forge workspace volume to docker-compose.yml (only in debug builds).
#[cfg(debug_assertions)]
fn patch_docker_compose(docker_compose_path: &Path) -> Result<()> {
    let workspace_dir = get_forge_workspace_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine forge workspace directory"))?;

    let content = fs::read_to_string(docker_compose_path)?;
    let patched = content.replace(
        "      - target_cache:/app/target\n",
        &format!(
            "      - target_cache:/app/target\n      - {workspace}:/forge\n",
            workspace = workspace_dir
        ),
    );
    fs::write(docker_compose_path, patched)?;

    Ok(())
}

/// Extract project name from a path (last segment only).
/// Handles: "my-app", "path/to/my-app", "./my-app", "../my-app"
pub(super) fn extract_project_name(input: &str) -> String {
    Path::new(input)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(input)
        .to_string()
}

/// Check if git is available on the system.
fn is_git_available() -> bool {
    Command::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Check if the directory is inside an existing git repository.
fn is_inside_git_repo(dir: &Path) -> bool {
    Command::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// Run forge generate to create frontend types.
fn run_forge_generate(dir: &Path) -> Result<()> {
    println!("  {} Generating frontend types...", ui::step());

    // Get the current executable path to run forge generate
    let forge_exe = std::env::current_exe().unwrap_or_else(|_| "forge".into());

    let output = Command::new(&forge_exe)
        .args(["generate", "-y"])
        .current_dir(dir)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!(
            "  {} Failed to generate types: {}",
            ui::warn(),
            stderr.trim()
        );
        return Ok(());
    }

    println!("  {} Frontend types generated", ui::ok());
    Ok(())
}

/// Run formatters (bun format and cargo fmt) to ensure clean code.
fn run_formatters(dir: &Path) -> Result<()> {
    // Run bun format in frontend directory
    let frontend_dir = dir.join("frontend");
    if frontend_dir.exists() {
        println!("  {} Formatting frontend...", ui::step());
        let output = Command::new("bun")
            .args(["run", "format"])
            .current_dir(&frontend_dir)
            .output();

        match output {
            Ok(o) if o.status.success() => {
                println!("  {} Frontend formatted", ui::ok());
            }
            _ => {
                // Non-fatal: continue without formatting
            }
        }
    }

    // Run cargo fmt if cargo is available
    let cargo_check = Command::new("cargo").arg("--version").output();
    if matches!(cargo_check, Ok(ref o) if o.status.success()) {
        println!("  {} Formatting backend...", ui::step());
        let output = Command::new("cargo")
            .args(["fmt"])
            .current_dir(dir)
            .output();

        match output {
            Ok(o) if o.status.success() => {
                println!("  {} Backend formatted", ui::ok());
            }
            _ => {
                // Non-fatal: continue without formatting
            }
        }
    }

    Ok(())
}

/// Generate Cargo.lock before initial commit.
fn generate_cargo_lockfile(dir: &Path) -> Result<()> {
    println!("  {} Generating Cargo.lock...", ui::step());

    if !matches!(Command::new("cargo").arg("--version").output(), Ok(o) if o.status.success()) {
        eprintln!(
            "  {} cargo not found, skipping lockfile generation",
            ui::warn()
        );
        return Ok(());
    }

    let output = Command::new("cargo")
        .args(["generate-lockfile"])
        .current_dir(dir)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!(
            "  {} Failed to generate Cargo.lock: {}",
            ui::warn(),
            stderr.trim()
        );
        return Ok(());
    }

    println!("  {} Cargo.lock generated", ui::ok());
    Ok(())
}

/// Generate bun.lock file using native bun.
/// Runs `bun install --lockfile-only` in the frontend directory.
fn generate_bun_lockfile(dir: &Path) -> Result<()> {
    let frontend_dir = dir.join("frontend");

    println!("  {} Generating bun.lock...", ui::step());

    // Check if bun is available
    let bun_check = Command::new("bun").arg("--version").output();

    if !matches!(bun_check, Ok(ref o) if o.status.success()) {
        eprintln!(
            "  {} bun not found, skipping lockfile generation",
            ui::warn()
        );
        eprintln!(
            "    Run {} in frontend/ after installing bun",
            style("bun install").cyan()
        );
        return Ok(());
    }

    let output = Command::new("bun")
        .args(["install", "--lockfile-only"])
        .current_dir(&frontend_dir)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!(
            "  {} Failed to generate bun.lock: {}",
            ui::warn(),
            stderr.trim()
        );
        // Non-fatal: continue without lockfile
        return Ok(());
    }

    println!("  {} bun.lock generated", ui::ok());

    Ok(())
}

/// Install the forge-idiomatic-engineer skill for AI agents.
fn install_skill(dir: &Path) -> Result<()> {
    println!(
        "  {} Installing forge-idiomatic-engineer skill...",
        ui::step()
    );

    let bun_check = Command::new("bun").arg("--version").output();
    if !matches!(bun_check, Ok(ref o) if o.status.success()) {
        eprintln!(
            "  {} bun not found, skipping skill installation",
            ui::warn()
        );
        eprintln!(
            "    Run {} to install later",
            style("bunx skills add https://github.com/isala404/forge/tree/main/docs/skills/forge-idiomatic-engineer -y").cyan()
        );
        return Ok(());
    }

    let output = Command::new("bunx")
        .args([
            "skills",
            "add",
            "https://github.com/isala404/forge/tree/main/docs/skills/forge-idiomatic-engineer",
            "-y",
        ])
        .current_dir(dir)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!(
            "  {} Failed to install skill: {}",
            ui::warn(),
            stderr.trim()
        );
        return Ok(());
    }

    println!("  {} forge-idiomatic-engineer skill installed", ui::ok());
    Ok(())
}

/// Initialize git repository and create initial commit.
/// Skips if directory is already inside a git repository.
fn init_git_repo(dir: &Path) -> Result<()> {
    // Skip if already inside a git repo (parent or current)
    if is_inside_git_repo(dir) {
        return Ok(());
    }

    // git init
    let init = Command::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()?;

    if !init.status.success() {
        return Ok(()); // Silently skip if init fails
    }

    // git add .
    let add = Command::new("git")
        .args(["add", "."])
        .current_dir(dir)
        .output()?;

    if !add.status.success() {
        return Ok(());
    }

    // git commit
    let _ = Command::new("git")
        .args(["commit", "-m", "Initialize project with Forge"])
        .current_dir(dir)
        .output()?;

    Ok(())
}

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
const README: &str = include_str!("../../templates/populated/project/README.md.tmpl");
const MIGRATION_INITIAL: &str =
    include_str!("../../templates/populated/project/migrations/0001_initial.sql.tmpl");
const SCHEMA_MOD: &str = include_str!("../../templates/populated/project/schema/mod.rs.tmpl");
const SCHEMA_USER: &str = include_str!("../../templates/populated/project/schema/user.rs.tmpl");
const FUNCTIONS_MOD: &str = include_str!("../../templates/populated/project/functions/mod.rs.tmpl");
const FUNCTIONS_USERS: &str =
    include_str!("../../templates/populated/project/functions/users.rs.tmpl");
const FUNCTIONS_ISS: &str = include_str!("../../templates/populated/project/functions/iss.rs.tmpl");
const FUNCTIONS_TRADES: &str =
    include_str!("../../templates/populated/project/functions/trades.rs.tmpl");
const FUNCTIONS_EXPORT: &str =
    include_str!("../../templates/populated/project/functions/export.rs.tmpl");
const FUNCTIONS_VERIFICATION: &str =
    include_str!("../../templates/populated/project/functions/verification.rs.tmpl");
const FUNCTIONS_WEBHOOK: &str =
    include_str!("../../templates/populated/project/functions/webhook.rs.tmpl");
const IGNORE: &str = include_str!("../../templates/populated/project/ignore.tmpl");

// Populated frontend templates (default)
const FRONTEND_PACKAGE_JSON: &str =
    include_str!("../../templates/populated/frontend/package.json.tmpl");
const FRONTEND_SVELTE_CONFIG: &str =
    include_str!("../../templates/populated/frontend/svelte.config.js.tmpl");
const FRONTEND_VITE_CONFIG: &str =
    include_str!("../../templates/populated/frontend/vite.config.ts.tmpl");
const FRONTEND_TSCONFIG: &str =
    include_str!("../../templates/populated/frontend/tsconfig.json.tmpl");
const FRONTEND_APP_HTML: &str = include_str!("../../templates/populated/frontend/app.html.tmpl");
const FRONTEND_ENV_EXAMPLE: &str = include_str!("../../templates/populated/frontend/env.tmpl");
const FRONTEND_LAYOUT_SVELTE: &str =
    include_str!("../../templates/populated/frontend/routes/layout.svelte.tmpl");
const FRONTEND_LAYOUT_TS: &str =
    include_str!("../../templates/populated/frontend/routes/layout.ts.tmpl");
const FRONTEND_PAGE_SVELTE: &str =
    include_str!("../../templates/populated/frontend/routes/page.svelte.tmpl");
const FRONTEND_ESLINT_CONFIG: &str =
    include_str!("../../templates/populated/frontend/eslint.config.js.tmpl");
const FRONTEND_PRETTIERIGNORE: &str =
    include_str!("../../templates/populated/frontend/.prettierignore.tmpl");
const FRONTEND_PLAYWRIGHT_HOME_SPEC: &str =
    include_str!("../../templates/populated/frontend/tests/home.spec.ts.tmpl");
const FRONTEND_PLAYWRIGHT_CONFIG: &str =
    include_str!("../../templates/populated/frontend/playwright.config.ts.tmpl");
const FRONTEND_PLAYWRIGHT_GLOBAL_SETUP: &str =
    include_str!("../../templates/populated/frontend/tests/global-setup.ts.tmpl");
const FRONTEND_PLAYWRIGHT_FIXTURES: &str =
    include_str!("../../templates/populated/frontend/tests/fixtures.ts.tmpl");

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
const EMPTY_README: &str = include_str!("../../templates/empty/project/README.md.tmpl");
const EMPTY_MIGRATION_INITIAL: &str =
    include_str!("../../templates/empty/project/migrations/0001_initial.sql.example.tmpl");
const EMPTY_SCHEMA_MOD: &str = include_str!("../../templates/empty/project/schema/mod.rs.tmpl");
const EMPTY_FUNCTIONS_MOD: &str =
    include_str!("../../templates/empty/project/functions/mod.rs.tmpl");
const EMPTY_IGNORE: &str = include_str!("../../templates/empty/project/ignore.tmpl");

// Empty frontend templates (for --empty flag)
const EMPTY_FRONTEND_PACKAGE_JSON: &str =
    include_str!("../../templates/empty/frontend/package.json.tmpl");
const EMPTY_FRONTEND_SVELTE_CONFIG: &str =
    include_str!("../../templates/empty/frontend/svelte.config.js.tmpl");
const EMPTY_FRONTEND_VITE_CONFIG: &str =
    include_str!("../../templates/empty/frontend/vite.config.ts.tmpl");
const EMPTY_FRONTEND_TSCONFIG: &str =
    include_str!("../../templates/empty/frontend/tsconfig.json.tmpl");
const EMPTY_FRONTEND_APP_HTML: &str = include_str!("../../templates/empty/frontend/app.html.tmpl");
const EMPTY_FRONTEND_ENV_EXAMPLE: &str = include_str!("../../templates/empty/frontend/env.tmpl");
const EMPTY_FRONTEND_LAYOUT_SVELTE: &str =
    include_str!("../../templates/empty/frontend/routes/layout.svelte.tmpl");
const EMPTY_FRONTEND_LAYOUT_TS: &str =
    include_str!("../../templates/empty/frontend/routes/layout.ts.tmpl");
const EMPTY_FRONTEND_PAGE_SVELTE: &str =
    include_str!("../../templates/empty/frontend/routes/page.svelte.tmpl");
const EMPTY_FRONTEND_ESLINT_CONFIG: &str =
    include_str!("../../templates/empty/frontend/eslint.config.js.tmpl");
const EMPTY_FRONTEND_PRETTIERIGNORE: &str =
    include_str!("../../templates/empty/frontend/.prettierignore.tmpl");
const EMPTY_FRONTEND_PLAYWRIGHT_HOME_SPEC: &str =
    include_str!("../../templates/empty/frontend/tests/home.spec.ts.tmpl");
const EMPTY_FRONTEND_PLAYWRIGHT_CONFIG: &str =
    include_str!("../../templates/empty/frontend/playwright.config.ts.tmpl");
const EMPTY_FRONTEND_PLAYWRIGHT_GLOBAL_SETUP: &str =
    include_str!("../../templates/empty/frontend/tests/global-setup.ts.tmpl");
const EMPTY_FRONTEND_PLAYWRIGHT_FIXTURES: &str =
    include_str!("../../templates/empty/frontend/tests/fixtures.ts.tmpl");

/// Create a new FORGE project.
#[derive(Parser)]
#[command(after_help = NEW_AFTER_HELP)]
pub struct NewCommand {
    /// Project name.
    pub name: String,

    /// Create a full demo project with example code.
    ///
    /// Includes: User CRUD, background jobs, cron tasks, workflows,
    /// external API actions, and a complete frontend demo UI.
    /// Perfect for learning FORGE or starting with working examples.
    #[arg(long, conflicts_with = "minimal")]
    pub demo: bool,

    /// Create a clean project with minimal scaffolding.
    ///
    /// Includes: Empty schema, functions, and migrations directories
    /// with commented examples. Frontend has a starter page.
    /// Perfect for experienced developers starting fresh.
    #[arg(long, conflicts_with = "demo")]
    pub minimal: bool,

    /// Output directory (defaults to project name).
    #[arg(short, long)]
    pub output: Option<String>,

    /// Skip generating bun.lock file before initial commit.
    ///
    /// By default, forge new runs `bun install --lockfile-only` in Docker
    /// to generate the bun.lock file before the initial git commit.
    /// Use this flag to skip lockfile generation.
    #[arg(long)]
    pub no_lock: bool,
}

const NEW_AFTER_HELP: &str = r#"TEMPLATE MODES:
  You must choose one of --demo or --minimal:

  --demo      Full demo project with working examples
              - User model with CRUD operations
              - Background job (export users)
              - Cron task (ISS location tracker)
              - Durable workflow (account verification)
              - Mutations with HTTP support
              - Complete frontend demo UI

  --minimal   Clean slate with just the structure
              - Empty schema/ and functions/ directories
              - Commented examples showing patterns
              - Starter frontend page
              - Ready for your own code

EXAMPLES:
  forge new my-app --demo       Learn FORGE with working examples
  forge new my-app --minimal    Start fresh with clean scaffolding"#;

impl NewCommand {
    /// Execute the new project command.
    pub async fn execute(self) -> Result<()> {
        ui::section("Create FORGE Project");
        println!("  {} Generating project files...", ui::tool());

        // Require either --demo or --minimal
        if !self.demo && !self.minimal {
            eprintln!("{} You must specify a template mode", ui::error());
            eprintln!();
            eprintln!("Choose one of:");
            eprintln!();
            eprintln!(
                "  {} {} {}",
                ui::bullet(),
                style("--demo").cyan().bold(),
                style("Full demo project with working examples").dim()
            );
            eprintln!("          User CRUD, jobs, crons, workflows, actions, and demo UI");
            eprintln!();
            eprintln!(
                "  {} {} {}",
                ui::bullet(),
                style("--minimal").cyan().bold(),
                style("Clean slate with just the structure").dim()
            );
            eprintln!("          Empty directories with commented examples, starter frontend");
            eprintln!();
            eprintln!("Examples:");
            eprintln!(
                "  {} {} {}",
                ui::bullet(),
                style("forge new my-app --demo").green(),
                style("# Learn FORGE with examples").dim()
            );
            eprintln!(
                "  {} {} {}",
                ui::bullet(),
                style("forge new my-app --minimal").green(),
                style("# Start fresh").dim()
            );
            eprintln!();
            eprintln!("Run {} for more details", style("forge new --help").cyan());
            std::process::exit(1);
        }

        // Extract just the project name from paths like "path/to/my-app"
        let project_name = extract_project_name(&self.name);
        let project_dir = self.output.as_ref().unwrap_or(&self.name);
        let path = Path::new(project_dir);

        if path.exists() {
            anyhow::bail!("Directory already exists: {}", project_dir);
        }

        fs::create_dir_all(path)?;
        create_project(path, &project_name, self.demo)?;

        // Generate lockfiles before git commit (unless --no-lock)
        if !self.no_lock {
            generate_bun_lockfile(path)?;
            generate_cargo_lockfile(path)?;
        }

        // Generate frontend types
        run_forge_generate(path)?;

        // Run formatters before git commit
        run_formatters(path)?;

        // Install forge-idiomatic-engineer skill for AI agents
        install_skill(path)?;

        // Initialize git repository if git is available
        if is_git_available() {
            init_git_repo(path)?;
        }

        println!();
        println!(
            "{} Created new FORGE project: {}",
            ui::ok(),
            style(&project_name).cyan()
        );
        ui::section("Next Steps");
        println!("  1. {}", style(format!("cd {}", project_dir)).cyan());
        println!("  2. {}", style("forge dev").cyan());
        println!("     Start development environment (requires Docker)");

        ui::section("Useful Commands");
        ui::command("forge dev down", "Stop the development environment");
        ui::command(
            "forge dev down --clear",
            "Stop and remove volumes + target/",
        );

        ui::section("Default Service URLs");
        ui::kv("Frontend", "http://localhost:5173");
        ui::kv("Backend", "http://localhost:8080");
        ui::kv("Grafana", "http://localhost:3000");

        ui::section("Docs");
        println!("  {} https://tryforge.dev/docs", ui::info());
        println!();

        Ok(())
    }
}

/// Create project files in the given directory.
///
/// - `demo = true`: Full demo project with example code
/// - `demo = false`: Minimal scaffolding without example code
pub fn create_project(dir: &Path, name: &str, demo: bool) -> Result<()> {
    let vars = template_vars!("name" => name, "project_name" => name);

    // Create directory structure
    fs::create_dir_all(dir.join("src/schema"))?;
    fs::create_dir_all(dir.join("src/functions"))?;
    fs::create_dir_all(dir.join("migrations"))?;

    if demo {
        // Demo templates - full example code
        fs::write(dir.join("Cargo.toml"), render(CARGO_TOML, &vars))?;

        // In debug builds, patch for local forge development
        #[cfg(debug_assertions)]
        {
            append_cargo_patch(&dir.join("Cargo.toml"))?;
            println!("  {} Added cargo patch for local development", ui::step());
        }

        fs::write(dir.join("forge.toml"), render(FORGE_TOML, &vars))?;
        fs::write(dir.join("build.rs"), BUILD_RS)?;
        fs::write(dir.join(".gitignore"), GITIGNORE)?;
        fs::write(dir.join(".ignore"), IGNORE)?;
        fs::write(dir.join(".env"), render(ENV, &vars))?;
        fs::write(dir.join("Dockerfile"), render(DOCKERFILE, &vars))?;
        fs::write(
            dir.join("docker-compose.yml"),
            render(DOCKER_COMPOSE, &vars),
        )?;

        #[cfg(debug_assertions)]
        patch_docker_compose(&dir.join("docker-compose.yml"))?;
        fs::write(dir.join("README.md"), render(README, &vars))?;
        fs::write(dir.join("src/main.rs"), MAIN_RS)?;
        fs::write(dir.join("migrations/0001_initial.sql"), MIGRATION_INITIAL)?;
        fs::write(dir.join("src/schema/mod.rs"), SCHEMA_MOD)?;
        fs::write(dir.join("src/schema/user.rs"), SCHEMA_USER)?;
        fs::write(dir.join("src/functions/mod.rs"), FUNCTIONS_MOD)?;
        fs::write(dir.join("src/functions/users.rs"), FUNCTIONS_USERS)?;
        fs::write(dir.join("src/functions/iss.rs"), FUNCTIONS_ISS)?;
        fs::write(dir.join("src/functions/trades.rs"), FUNCTIONS_TRADES)?;
        fs::write(dir.join("src/functions/export.rs"), FUNCTIONS_EXPORT)?;
        fs::write(
            dir.join("src/functions/verification.rs"),
            FUNCTIONS_VERIFICATION,
        )?;
        fs::write(dir.join("src/functions/webhook.rs"), FUNCTIONS_WEBHOOK)?;
        // Demo frontend
        create_frontend(dir, name, true)?;
    } else {
        // Minimal templates - clean scaffolding without example code
        fs::write(dir.join("Cargo.toml"), render(EMPTY_CARGO_TOML, &vars))?;

        // In debug builds, patch for local forge development
        #[cfg(debug_assertions)]
        {
            append_cargo_patch(&dir.join("Cargo.toml"))?;
            println!("  {} Added cargo patch for local development", ui::step());
        }

        fs::write(dir.join("forge.toml"), render(EMPTY_FORGE_TOML, &vars))?;
        fs::write(dir.join("build.rs"), EMPTY_BUILD_RS)?;
        fs::write(dir.join(".gitignore"), EMPTY_GITIGNORE)?;
        fs::write(dir.join(".ignore"), EMPTY_IGNORE)?;
        fs::write(dir.join(".env"), render(EMPTY_ENV, &vars))?;
        fs::write(dir.join(".env.example"), render(EMPTY_ENV, &vars))?;
        fs::write(dir.join("Dockerfile"), render(EMPTY_DOCKERFILE, &vars))?;
        fs::write(
            dir.join("docker-compose.yml"),
            render(EMPTY_DOCKER_COMPOSE, &vars),
        )?;

        #[cfg(debug_assertions)]
        patch_docker_compose(&dir.join("docker-compose.yml"))?;
        fs::write(dir.join("README.md"), render(EMPTY_README, &vars))?;
        fs::write(dir.join("src/main.rs"), EMPTY_MAIN_RS)?;
        fs::write(
            dir.join("migrations/0001_initial.sql.example"),
            EMPTY_MIGRATION_INITIAL,
        )?;
        fs::write(dir.join("src/schema/mod.rs"), EMPTY_SCHEMA_MOD)?;
        fs::write(dir.join("src/functions/mod.rs"), EMPTY_FUNCTIONS_MOD)?;
        // Minimal frontend
        create_frontend(dir, name, false)?;
    }

    Ok(())
}

/// Create frontend scaffolding.
///
/// - `demo = true`: Full demo frontend with complete UI
/// - `demo = false`: Minimal frontend with starter page
fn create_frontend(dir: &Path, name: &str, demo: bool) -> Result<()> {
    let vars = template_vars!("name" => name, "project_name" => name);

    let frontend_dir = dir.join("frontend");
    fs::create_dir_all(&frontend_dir)?;
    fs::create_dir_all(frontend_dir.join("src/routes"))?;

    // Create tests directory
    fs::create_dir_all(frontend_dir.join("tests"))?;

    if demo {
        // Demo templates - full frontend with complete UI
        fs::write(
            frontend_dir.join("playwright.config.ts"),
            FRONTEND_PLAYWRIGHT_CONFIG,
        )?;
        fs::write(
            frontend_dir.join("tests/global-setup.ts"),
            FRONTEND_PLAYWRIGHT_GLOBAL_SETUP,
        )?;
        fs::write(
            frontend_dir.join("tests/fixtures.ts"),
            FRONTEND_PLAYWRIGHT_FIXTURES,
        )?;
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
        fs::write(frontend_dir.join(".env.example"), FRONTEND_ENV_EXAMPLE)?;
        fs::write(
            frontend_dir.join("eslint.config.js"),
            FRONTEND_ESLINT_CONFIG,
        )?;
        fs::write(
            frontend_dir.join(".prettierignore"),
            FRONTEND_PRETTIERIGNORE,
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
        // Demo test spec
        fs::write(
            frontend_dir.join("tests/home.spec.ts"),
            FRONTEND_PLAYWRIGHT_HOME_SPEC,
        )?;
    } else {
        // Minimal templates - starter frontend
        fs::write(
            frontend_dir.join("playwright.config.ts"),
            EMPTY_FRONTEND_PLAYWRIGHT_CONFIG,
        )?;
        fs::write(
            frontend_dir.join("tests/global-setup.ts"),
            EMPTY_FRONTEND_PLAYWRIGHT_GLOBAL_SETUP,
        )?;
        fs::write(
            frontend_dir.join("tests/fixtures.ts"),
            EMPTY_FRONTEND_PLAYWRIGHT_FIXTURES,
        )?;
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
            frontend_dir.join(".env.example"),
            EMPTY_FRONTEND_ENV_EXAMPLE,
        )?;
        fs::write(
            frontend_dir.join("eslint.config.js"),
            EMPTY_FRONTEND_ESLINT_CONFIG,
        )?;
        fs::write(
            frontend_dir.join(".prettierignore"),
            EMPTY_FRONTEND_PRETTIERIGNORE,
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
        // Minimal test spec
        fs::write(
            frontend_dir.join("tests/home.spec.ts"),
            EMPTY_FRONTEND_PLAYWRIGHT_HOME_SPEC,
        )?;
    }

    // Generate @forge/svelte runtime package
    super::runtime_generator::generate_runtime(&frontend_dir)?;

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_extract_project_name() {
        // Simple name
        assert_eq!(extract_project_name("my-app"), "my-app");

        // Path with slashes
        assert_eq!(extract_project_name("path/to/my-app"), "my-app");
        assert_eq!(extract_project_name("./my-app"), "my-app");
        assert_eq!(extract_project_name("../my-app"), "my-app");

        // Absolute path
        assert_eq!(extract_project_name("/home/user/projects/my-app"), "my-app");

        // Trailing slash (edge case)
        assert_eq!(extract_project_name("my-app/"), "my-app");
    }

    #[test]
    fn test_create_demo_project() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test-demo");
        fs::create_dir_all(&path).unwrap();

        create_project(&path, "test-demo", true).unwrap();

        // All demo files should exist
        assert!(path.join("Cargo.toml").exists());
        assert!(path.join("forge.toml").exists());
        assert!(path.join("src/main.rs").exists());
        assert!(path.join("build.rs").exists());
        assert!(path.join("src/schema/mod.rs").exists());
        assert!(path.join("src/schema/user.rs").exists());
        assert!(path.join("src/functions/users.rs").exists());
        assert!(path.join("src/functions/iss.rs").exists());
        assert!(path.join("src/functions/trades.rs").exists());
        assert!(path.join("src/functions/export.rs").exists());
        assert!(path.join("src/functions/verification.rs").exists());
        assert!(path.join("src/functions/webhook.rs").exists());
        assert!(path.join("frontend/package.json").exists());
        assert!(path.join("frontend/src/routes/+layout.ts").exists());
        // Note: frontend/src/lib/forge/ files are generated by `forge generate`, not scaffolded
        assert!(path.join("frontend/eslint.config.js").exists());
        assert!(path.join("migrations/0001_initial.sql").exists());
        assert!(path.join("Dockerfile").exists());
        assert!(path.join("docker-compose.yml").exists());
        assert!(path.join("README.md").exists());
        // Playwright test files
        assert!(path.join("frontend/playwright.config.ts").exists());
        assert!(path.join("frontend/tests/global-setup.ts").exists());
        assert!(path.join("frontend/tests/fixtures.ts").exists());
        assert!(path.join("frontend/tests/home.spec.ts").exists());
        // Observability baked into Docker image, not scaffolded
        assert!(!path.join("grafana").exists());
    }

    #[test]
    fn test_create_minimal_project() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("test-minimal");
        fs::create_dir_all(&path).unwrap();

        create_project(&path, "test-minimal", false).unwrap();

        // Core files should exist
        assert!(path.join("Cargo.toml").exists());
        assert!(path.join("forge.toml").exists());
        assert!(path.join("src/main.rs").exists());
        assert!(path.join("src/schema/mod.rs").exists());
        assert!(path.join("src/functions/mod.rs").exists());
        assert!(path.join("migrations/0001_initial.sql.example").exists());

        // Example files should NOT exist
        assert!(!path.join("src/schema/user.rs").exists());
        assert!(!path.join("src/functions/users.rs").exists());
        assert!(!path.join("src/functions/iss.rs").exists());
        assert!(!path.join("src/functions/trades.rs").exists());
        assert!(!path.join("src/functions/export.rs").exists());
        assert!(!path.join("src/functions/verification.rs").exists());
        assert!(!path.join("src/functions/webhook.rs").exists());

        // Frontend should exist with minimal templates
        assert!(path.join("frontend/package.json").exists());
        // Note: frontend/src/lib/forge/ files are generated by `forge generate`, not scaffolded
        // Playwright test files
        assert!(path.join("frontend/playwright.config.ts").exists());
        assert!(path.join("frontend/tests/global-setup.ts").exists());
        assert!(path.join("frontend/tests/fixtures.ts").exists());
        assert!(path.join("frontend/tests/home.spec.ts").exists());
        // Observability baked into Docker image, not scaffolded
        assert!(!path.join("grafana").exists());
    }
}

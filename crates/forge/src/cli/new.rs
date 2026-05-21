use anyhow::{Context, Result};
use clap::Parser;
use console::style;
use std::collections::HashMap;
use std::fs;
use std::io::IsTerminal;
use std::path::Path;
use std::process::{Command as StdCommand, Stdio};
use tokio::process::Command as TokioCommand;
use tokio::signal;

use super::frontend_target::FrontendTarget;
use super::template::render;
use super::template_catalog::{
    TemplateDefinition, load_template_definition, supported_template_ids,
};
use super::ui;

#[cfg(debug_assertions)]
const FORGE_MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// Returns the workspace root from `CARGO_MANIFEST_DIR` (crates/forge → two levels up).
#[cfg(debug_assertions)]
fn get_forge_workspace_dir() -> Option<&'static str> {
    let manifest_dir = Path::new(FORGE_MANIFEST_DIR);
    manifest_dir.parent()?.parent()?.to_str()
}

/// Append `[patch.crates-io]` entries pointing at the local workspace (debug builds only).
#[cfg(debug_assertions)]
fn append_cargo_patch(cargo_toml_path: &Path, patches: &[(&str, &str)]) -> Result<()> {
    let workspace_dir = get_forge_workspace_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine forge workspace directory"))?;

    let entries: String = patches
        .iter()
        .map(|(name, path)| format!("{name} = {{ path = \"{workspace_dir}/{path}\" }}"))
        .collect::<Vec<_>>()
        .join("\n");

    let patch_section =
        format!("\n# Local dev patches — remove before publishing\n[patch.crates-io]\n{entries}\n");

    let mut content = fs::read_to_string(cargo_toml_path)?;
    content.push_str(&patch_section);
    fs::write(cargo_toml_path, content)?;

    Ok(())
}

/// Mount the host workspace at the same absolute path inside the container so
/// cargo patch paths resolve identically in both environments (debug builds only).
#[cfg(debug_assertions)]
fn patch_docker_compose(docker_compose_path: &Path) -> Result<()> {
    let workspace_dir = get_forge_workspace_dir()
        .ok_or_else(|| anyhow::anyhow!("Could not determine forge workspace directory"))?;

    let content = fs::read_to_string(docker_compose_path)?;
    let patched = content.replace(
        "      - ./target:/app/target\n",
        &format!(
            "      - ./target:/app/target\n      - {ws}:{ws}\n",
            ws = workspace_dir
        ),
    );
    fs::write(docker_compose_path, patched)?;

    Ok(())
}

/// Extracts the last path segment as the project name.
pub(super) fn extract_project_name(input: &str) -> String {
    Path::new(input)
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or(input)
        .to_string()
}

fn is_git_available() -> bool {
    StdCommand::new("git")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn is_inside_git_repo(dir: &Path) -> bool {
    StdCommand::new("git")
        .args(["rev-parse", "--git-dir"])
        .current_dir(dir)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn run_forge_generate(dir: &Path) -> Result<()> {
    println!("  {} Generating frontend types...", ui::step());

    let forge_exe = std::env::current_exe().unwrap_or_else(|_| "forge".into());

    let output = StdCommand::new(&forge_exe)
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

    let frontend_dir = dir.join("frontend");
    if let Some(target) = FrontendTarget::detect(&frontend_dir) {
        target.post_generate(&frontend_dir)?;
    }

    Ok(())
}

fn run_formatters(dir: &Path, frontend: FrontendTarget) -> Result<()> {
    let frontend_dir = dir.join("frontend");

    if frontend_dir.join("package.json").exists() {
        println!("  {} Formatting frontend...", ui::step());
        let output = StdCommand::new("bun")
            .args(["run", "format"])
            .current_dir(&frontend_dir)
            .output();

        if matches!(output, Ok(ref o) if o.status.success()) {
            println!("  {} Frontend formatted", ui::ok());
        }
    }

    if frontend == FrontendTarget::Dioxus {
        frontend.extra_format(dir)?;
    }

    let cargo_check = StdCommand::new("cargo").arg("--version").output();
    if matches!(cargo_check, Ok(ref o) if o.status.success()) {
        println!("  {} Formatting backend...", ui::step());
        let output = StdCommand::new("cargo")
            .args(["fmt"])
            .current_dir(dir)
            .output();

        if matches!(output, Ok(ref o) if o.status.success()) {
            println!("  {} Backend formatted", ui::ok());
        }
    }

    Ok(())
}

fn generate_cargo_lockfile(dir: &Path, frontend: FrontendTarget) -> Result<()> {
    println!("  {} Generating Cargo.lock...", ui::step());

    if !matches!(StdCommand::new("cargo").arg("--version").output(), Ok(o) if o.status.success()) {
        eprintln!(
            "  {} cargo not found, skipping lockfile generation",
            ui::warn()
        );
        return Ok(());
    }

    let output = StdCommand::new("cargo")
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

    if frontend == FrontendTarget::Dioxus {
        let output = StdCommand::new("cargo")
            .args(["generate-lockfile"])
            .current_dir(dir.join("frontend"))
            .output()?;

        if output.status.success() {
            println!("  {} frontend/Cargo.lock generated", ui::ok());
        } else {
            let stderr = String::from_utf8_lossy(&output.stderr);
            eprintln!(
                "  {} Failed to generate frontend/Cargo.lock: {}",
                ui::warn(),
                stderr.trim()
            );
        }
    }

    Ok(())
}

fn install_frontend_deps(dir: &Path, _frontend: FrontendTarget) -> Result<()> {
    let frontend_dir = dir.join("frontend");
    if !frontend_dir.join("package.json").exists() {
        return Ok(());
    }

    println!("  {} Installing frontend dependencies...", ui::step());

    let bun_check = StdCommand::new("bun").arg("--version").output();

    if !matches!(bun_check, Ok(ref o) if o.status.success()) {
        eprintln!(
            "  {} bun not found, skipping dependency installation",
            ui::warn()
        );
        eprintln!(
            "    Run {} in frontend/ after installing bun",
            style("bun install").cyan()
        );
        return Ok(());
    }

    let output = StdCommand::new("bun")
        .args(["install"])
        .current_dir(&frontend_dir)
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        eprintln!(
            "  {} Failed to install frontend dependencies: {}",
            ui::warn(),
            stderr.trim()
        );
        return Ok(());
    }

    println!("  {} Frontend dependencies installed", ui::ok());

    Ok(())
}

const SKILL_INSTALL_URL: &str =
    "https://github.com/isala404/forge/tree/main/docs/skills/forge-idiomatic-engineer";

async fn install_skill(dir: &Path, non_interactive: bool) -> Result<()> {
    println!(
        "  {} Preparing forge-idiomatic-engineer skill installer...",
        ui::step()
    );

    let bun_check = StdCommand::new("bun").arg("--version").output();
    if !matches!(bun_check, Ok(ref o) if o.status.success()) {
        eprintln!(
            "  {} bun not found, skipping skill installation",
            ui::warn()
        );
        eprintln!(
            "    Run {} to install later",
            style(format!("bunx skills add {SKILL_INSTALL_URL}")).cyan()
        );
        return Ok(());
    }

    if non_interactive {
        let output = StdCommand::new("bunx")
            .args(["skills", "add", "-y", SKILL_INSTALL_URL])
            .current_dir(dir)
            .output();

        match output {
            Ok(o) if o.status.success() => {
                println!("  {} forge-idiomatic-engineer skill installed", ui::ok());
            }
            Ok(o) => {
                let stderr = String::from_utf8_lossy(&o.stderr);
                eprintln!(
                    "  {} Skill installation failed: {}",
                    ui::warn(),
                    stderr.trim()
                );
                eprintln!(
                    "    Run {} to install later",
                    style(format!("bunx skills add {SKILL_INSTALL_URL}")).cyan()
                );
            }
            Err(err) => {
                eprintln!("  {} Failed to run skill installer: {}", ui::warn(), err);
            }
        }
        return Ok(());
    }

    if !std::io::stdin().is_terminal()
        || !std::io::stdout().is_terminal()
        || !std::io::stderr().is_terminal()
    {
        eprintln!(
            "  {} Interactive terminal not available, skipping skill installer",
            ui::warn()
        );
        eprintln!(
            "    Run {} to install later",
            style(format!("bunx skills add {SKILL_INSTALL_URL}")).cyan()
        );
        return Ok(());
    }

    println!(
        "  {} Handing terminal control to the skill installer...",
        ui::step()
    );
    println!(
        "    Run completes when the installer exits. Press Ctrl+C in the installer to stop and continue."
    );

    let mut child = match TokioCommand::new("bunx")
        .args(["skills", "add", SKILL_INSTALL_URL])
        .current_dir(dir)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .spawn()
    {
        Ok(child) => child,
        Err(err) => {
            eprintln!("  {} Failed to start skill installer: {}", ui::warn(), err);
            eprintln!(
                "    Run {} to install later",
                style(format!("bunx skills add {SKILL_INSTALL_URL}")).cyan()
            );
            return Ok(());
        }
    };

    tokio::select! {
        status = child.wait() => {
            match status {
                Ok(status) if status.success() => {
                    println!("  {} forge-idiomatic-engineer skill installed", ui::ok());
                }
                Ok(status) => {
                    eprintln!(
                        "  {} Skill installer exited with status {}",
                        ui::warn(),
                        status
                    );
                    eprintln!(
                        "    Re-run {} if you still want the skill",
                        style(format!("bunx skills add {SKILL_INSTALL_URL}")).cyan()
                    );
                }
                Err(err) => {
                    eprintln!(
                        "  {} Failed to wait for skill installer: {}",
                        ui::warn(),
                        err
                    );
                }
            }
        }
        _ = signal::ctrl_c() => {
            println!();
            println!(
                "  {} Leaving skill installer and continuing project setup...",
                ui::stop()
            );

            #[cfg(unix)]
            if let Some(id) = child.id() {
                use nix::sys::signal::{Signal, kill};
                use nix::unistd::Pid;
                let _ = kill(Pid::from_raw(id as i32), Signal::SIGINT);
            }

            match child.wait().await {
                Ok(status) if status.success() => {
                    println!("  {} forge-idiomatic-engineer skill installed", ui::ok());
                }
                Ok(_) => {
                    eprintln!("  {} Skill installation left to the user", ui::warn());
                }
                Err(err) => {
                    eprintln!(
                        "  {} Failed to wait for skill installer after Ctrl+C: {}",
                        ui::warn(),
                        err
                    );
                }
            }
        }
    }

    Ok(())
}

fn init_git_repo(dir: &Path) -> Result<()> {
    if is_inside_git_repo(dir) {
        return Ok(());
    }

    let init = StdCommand::new("git")
        .args(["init"])
        .current_dir(dir)
        .output()?;

    if !init.status.success() {
        return Ok(());
    }

    let add = StdCommand::new("git")
        .args(["add", "."])
        .current_dir(dir)
        .output()?;

    if !add.status.success() {
        return Ok(());
    }

    let _ = StdCommand::new("git")
        .args(["commit", "-m", "Initialize project with Forge"])
        .current_dir(dir)
        .output()?;

    Ok(())
}

/// Create a new FORGE project — or scaffold a new handler inside an existing one.
///
/// Two modes, distinguished by the first positional arg:
///   * `forge new <project-name> --template <id>` → scaffold a new project
///   * `forge new <kind> <name>` where kind ∈ query|mutation|job|cron|workflow|daemon|webhook|mcp_tool|model|enum
#[derive(Parser)]
#[command(after_help = NEW_AFTER_HELP)]
pub struct NewCommand {
    /// Project name OR a handler kind (`query`, `mutation`, `job`, ...).
    pub name: String,

    /// Handler name (only used when `name` is a handler kind).
    #[arg(value_name = "HANDLER_NAME")]
    pub handler_name: Option<String>,

    /// Template id (for example: with-svelte/minimal). Only used in project mode.
    #[arg(long)]
    pub template: Option<String>,

    /// Output directory (defaults to project name). Only used in project mode.
    #[arg(short, long)]
    pub output: Option<String>,

    /// Skip generating Cargo.lock before initial commit. Only used in project mode.
    #[arg(long)]
    pub no_lock: bool,

    /// Auto-accept skill installer prompts (non-interactive). Only used in project mode.
    #[arg(short = 'y', long)]
    pub yes: bool,
}

const NEW_AFTER_HELP: &str = r#"TEMPLATES:
  with-svelte/minimal
  with-svelte/demo
  with-svelte/realtime-todo-list
  with-dioxus/minimal
  with-dioxus/demo
  with-dioxus/realtime-todo-list

HANDLER KINDS:
  query mutation job cron workflow daemon webhook mcp_tool model enum

EXAMPLES:
  forge new my-app --template with-svelte/minimal
  forge new query list_invoices
  forge new mutation create_invoice
  forge new model Invoice"#;

impl NewCommand {
    pub async fn execute(self) -> Result<()> {
        // Handler mode: first arg is a known kind and no --template was given.
        if self.template.is_none()
            && let Some(kind) = super::handler_scaffold::HandlerKind::from_str(&self.name)
        {
            let Some(handler_name) = self.handler_name.as_deref() else {
                anyhow::bail!(
                    "Missing handler name.\nUsage: forge new {} <name>",
                    kind.label()
                );
            };
            return super::handler_scaffold::scaffold_handler(kind, handler_name).await;
        }

        let template_id = self.template.as_deref().ok_or_else(|| {
            anyhow::anyhow!(
                "Missing --template.\n\nUsage:\n  \
                 forge new <project-name> --template <id>      (scaffold a new project)\n  \
                 forge new <kind> <handler-name>               (scaffold a handler in this project)"
            )
        })?;

        ui::section("Create FORGE Project");
        println!("  {} Generating project files...", ui::tool());

        if !supported_template_ids().contains(&template_id) {
            return Err(invalid_template_error(template_id));
        }

        let template = load_template_definition(template_id)?;

        let project_name = extract_project_name(&self.name);
        let project_dir = self.output.as_ref().unwrap_or(&self.name);
        let path = Path::new(project_dir);

        if path.exists() {
            anyhow::bail!("Directory already exists: {}", project_dir);
        }

        fs::create_dir_all(path)?;
        create_project_from_template(path, &project_name, &template)?;

        install_frontend_deps(path, template.frontend)?;
        run_forge_generate(path)?;
        run_formatters(path, template.frontend)?;

        if !self.no_lock {
            generate_cargo_lockfile(path, template.frontend)?;
        }
        install_skill(path, self.yes).await?;

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
        println!("  2. {}", style("docker compose up --build").cyan());
        println!("     Start development environment (requires Docker)");
        if template.frontend == FrontendTarget::Dioxus {
            println!(
                "  3. {}",
                style("cd frontend && dx serve --port 9080").cyan()
            );
            println!("     Start the Dioxus frontend natively (web by default)");
        }

        ui::section("Useful Commands");
        ui::command("docker compose down", "Stop the development environment");
        ui::command("docker compose down -v", "Stop and remove volumes");

        ui::section("Default Service URLs");
        if template.frontend == FrontendTarget::Dioxus {
            ui::kv("Frontend", "dx serve --port 9080");
        } else {
            ui::kv("Frontend", "http://localhost:9080");
        }
        ui::kv("Backend", "http://localhost:9081");
        ui::kv("Grafana", "http://localhost:3000");

        ui::section("Docs");
        println!("  {} https://tryforge.dev/docs", ui::info());
        println!();

        Ok(())
    }
}

pub fn create_project_from_template(
    dir: &Path,
    project_name: &str,
    template: &TemplateDefinition,
) -> Result<()> {
    for relative_dir in template.bundled_directories()? {
        if template.should_exclude(&relative_dir) {
            continue;
        }
        fs::create_dir_all(dir.join(relative_dir))?;
    }

    let project_db_name = project_name.replace('-', "_");
    let frontend_package_name = format!("{project_name}-frontend");
    let forge_version = env!("CARGO_PKG_VERSION");
    let rewrite_vars = HashMap::from([
        ("project_name", project_name),
        ("project_slug", project_name),
        ("project_db_name", project_db_name.as_str()),
        ("frontend_package_name", frontend_package_name.as_str()),
        (
            "canonical_internal_slug",
            template.canonical_internal_slug.as_str(),
        ),
        ("forge_version", forge_version),
    ]);

    for bundled_file in template.bundled_files()? {
        if template.should_exclude(&bundled_file.relative_path) {
            continue;
        }

        let output_path = dir.join(&bundled_file.relative_path);
        if let Some(parent) = output_path.parent() {
            fs::create_dir_all(parent)?;
        }

        if template.rewrite_file(&bundled_file.relative_path) {
            let content = std::str::from_utf8(bundled_file.file.contents()).with_context(|| {
                format!(
                    "template file '{}' is not valid UTF-8",
                    bundled_file.relative_path.display()
                )
            })?;
            let rewritten = apply_template_replacements(
                content,
                &bundled_file.relative_path,
                template,
                &rewrite_vars,
            );
            fs::write(output_path, rewritten)?;
        } else {
            fs::write(output_path, bundled_file.file.contents())?;
        }
    }

    #[cfg(debug_assertions)]
    {
        let backend_patches: &[(&str, &str)] = &[
            ("forgex", "crates/forge"),
            ("forge-core", "crates/forge-core"),
            ("forge-macros", "crates/forge-macros"),
            ("forge-runtime", "crates/forge-runtime"),
            ("forge-codegen", "crates/forge-codegen"),
        ];

        if dir.join("Cargo.toml").exists() {
            append_cargo_patch(&dir.join("Cargo.toml"), backend_patches)?;
            println!("  {} Added cargo patch for local development", ui::step());
        }

        if template.frontend == FrontendTarget::Dioxus && dir.join("frontend/Cargo.toml").exists() {
            let frontend_patches: &[(&str, &str)] = &[("forge-dioxus", "packages/forge-dioxus")];
            append_cargo_patch(&dir.join("frontend/Cargo.toml"), frontend_patches)?;
            println!(
                "  {} Added frontend cargo patch for local development",
                ui::step()
            );
        }

        if dir.join("docker-compose.yml").exists() {
            patch_docker_compose(&dir.join("docker-compose.yml"))?;
        }
    }

    Ok(())
}

fn apply_template_replacements(
    content: &str,
    relative_path: &Path,
    template: &TemplateDefinition,
    rewrite_vars: &HashMap<&str, &str>,
) -> String {
    let relative_path = relative_path.to_string_lossy();
    let mut rewritten = content.to_string();

    for replacement in &template.replacements {
        if !replacement.files.is_empty()
            && !replacement
                .files
                .iter()
                .any(|path| path == relative_path.as_ref())
        {
            continue;
        }

        let target = render(&replacement.to, rewrite_vars);
        rewritten = rewritten.replace(&replacement.from, &target);
    }

    if is_cargo_manifest(&relative_path) {
        rewritten = pin_package_version(&rewritten);
    }

    rewritten
}

fn is_cargo_manifest(relative_path: &str) -> bool {
    relative_path == "Cargo.toml" || relative_path.ends_with("/Cargo.toml")
}

/// Pin `[package].version` to `1.0.0`. The template version tracks the Forge
/// release and has no meaning for the scaffolded project.
fn pin_package_version(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut in_package = false;
    let mut pinned = false;

    for line in content.split_inclusive('\n') {
        if !pinned {
            let trimmed = line.trim_start();
            if trimmed.starts_with('[') {
                in_package = trimmed.starts_with("[package]");
            } else if in_package && trimmed.starts_with("version") {
                result.push_str("version = \"1.0.0\"\n");
                pinned = true;
                continue;
            }
        }
        result.push_str(line);
    }

    result
}

fn invalid_template_error(template_id: &str) -> anyhow::Error {
    let supported = supported_template_ids()
        .iter()
        .map(|id| format!("  - {id}"))
        .collect::<Vec<_>>()
        .join("\n");

    anyhow::anyhow!(
        "Unknown template '{}'.\n\nSupported templates:\n{}",
        template_id,
        supported
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_extract_project_name() {
        assert_eq!(extract_project_name("my-app"), "my-app");
        assert_eq!(extract_project_name("path/to/my-app"), "my-app");
        assert_eq!(extract_project_name("./my-app"), "my-app");
        assert_eq!(extract_project_name("../my-app"), "my-app");
        assert_eq!(extract_project_name("/home/user/projects/my-app"), "my-app");
        assert_eq!(extract_project_name("my-app/"), "my-app");
    }

    #[test]
    fn test_copy_svelte_minimal_template_rewrites_names() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("my-app");
        fs::create_dir_all(&path).unwrap();

        let template = load_template_definition("with-svelte/minimal").unwrap();
        create_project_from_template(&path, "my-app", &template).unwrap();

        assert!(path.join("Cargo.toml").exists());
        assert!(path.join("frontend/package.json").exists());
        assert!(path.join(".sqlx").exists());
        assert!(!path.join(".forge-template.toml").exists());

        let cargo_toml = fs::read_to_string(path.join("Cargo.toml")).unwrap();
        assert!(cargo_toml.contains("name = \"my-app\""));

        let package_json = fs::read_to_string(path.join("frontend/package.json")).unwrap();
        assert!(package_json.contains("\"my-app-frontend\""));

        let page = fs::read_to_string(path.join("frontend/src/routes/+page.svelte")).unwrap();
        assert!(page.contains("<h1>my-app</h1>"));
    }

    #[test]
    fn test_copy_realtime_todo_template_skips_transient_dirs() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("my-app");
        fs::create_dir_all(&path).unwrap();

        let template = load_template_definition("with-svelte/realtime-todo-list").unwrap();
        create_project_from_template(&path, "my-app", &template).unwrap();

        assert!(path.join(".sqlx").exists());
        assert!(!path.join("pg_data").exists());
        assert!(!path.join("test-results").exists());

        let cargo_toml = fs::read_to_string(path.join("Cargo.toml")).unwrap();
        assert!(cargo_toml.contains("name = \"my-app\""));
    }

    #[test]
    fn test_copy_dioxus_minimal_template_rewrites_frontend_manifest() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("my-app");
        fs::create_dir_all(&path).unwrap();

        let template = load_template_definition("with-dioxus/minimal").unwrap();
        create_project_from_template(&path, "my-app", &template).unwrap();

        assert!(path.join(".sqlx").exists());
        assert!(path.join("frontend/Cargo.toml").exists());
        assert!(!path.join("frontend/dist").exists());
        assert!(!path.join("frontend/target").exists());

        let frontend_manifest = fs::read_to_string(path.join("frontend/Cargo.toml")).unwrap();
        assert!(frontend_manifest.contains("name = \"my-app-frontend\""));

        let frontend_main = fs::read_to_string(path.join("frontend/src/main.rs")).unwrap();
        assert!(frontend_main.contains("ForgeProvider"));
        assert!(frontend_main.contains("Router::<Route>"));
    }

    #[test]
    fn test_copy_svelte_demo_template_includes_sqlx_cache() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("my-app");
        fs::create_dir_all(&path).unwrap();

        let template = load_template_definition("with-svelte/demo").unwrap();
        create_project_from_template(&path, "my-app", &template).unwrap();

        assert!(path.join(".sqlx").exists());
        assert!(path.join(".sqlx").read_dir().unwrap().next().is_some());
    }

    #[test]
    fn test_all_templates_rewrite_volume_mounts_to_standalone() {
        for template_id in supported_template_ids() {
            let dir = tempdir().unwrap();
            let path = dir.path().join("my-app");
            fs::create_dir_all(&path).unwrap();

            let template = load_template_definition(template_id).unwrap();
            create_project_from_template(&path, "my-app", &template).unwrap();

            let dc = fs::read_to_string(path.join("docker-compose.yml")).unwrap();

            assert!(
                !dc.contains("../../..:/workspace"),
                "{template_id}: workspace volume mount not rewritten"
            );
            assert!(
                !dc.contains("/workspace/examples/"),
                "{template_id}: workspace working_dir or target path not rewritten"
            );
            assert!(
                dc.contains("- .:/app"),
                "{template_id}: missing standalone volume mount .:/app"
            );
            assert!(
                dc.contains("- ./target:/app/target"),
                "{template_id}: missing standalone target mount"
            );
            assert!(
                dc.contains("working_dir: /app"),
                "{template_id}: missing standalone working_dir"
            );

            if template_id.starts_with("with-svelte/") {
                assert!(
                    !dc.contains("packages/forge-svelte"),
                    "{template_id}: workspace forge-svelte volume not removed"
                );
            }
        }
    }

    #[test]
    fn test_all_templates_rewrite_env_example_postgres_db() {
        for template_id in supported_template_ids() {
            let dir = tempdir().unwrap();
            let path = dir.path().join("my-app");
            fs::create_dir_all(&path).unwrap();

            let template = load_template_definition(template_id).unwrap();
            create_project_from_template(&path, "my-app", &template).unwrap();

            let env_example = fs::read_to_string(path.join(".env.example")).unwrap();
            assert!(
                env_example.contains("POSTGRES_DB=my_app"),
                "{template_id}: .env.example POSTGRES_DB not rewritten to project_db_name\n{env_example}"
            );
        }
    }

    #[test]
    fn test_dioxus_templates_rewrite_frontend_manifest_name() {
        for template_id in supported_template_ids() {
            if !template_id.starts_with("with-dioxus/") {
                continue;
            }
            let dir = tempdir().unwrap();
            let path = dir.path().join("my-app");
            fs::create_dir_all(&path).unwrap();

            let template = load_template_definition(template_id).unwrap();
            create_project_from_template(&path, "my-app", &template).unwrap();

            let dioxus_toml = fs::read_to_string(path.join("frontend/Dioxus.toml")).unwrap();
            assert!(
                dioxus_toml.contains("name = \"my-app-frontend\""),
                "{template_id}: frontend/Dioxus.toml name not rewritten to frontend_package_name\n{dioxus_toml}"
            );
        }
    }

    #[test]
    fn test_scaffolded_cargo_toml_pins_version_to_one() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("my-app");
        fs::create_dir_all(&path).unwrap();

        let template = load_template_definition("with-svelte/minimal").unwrap();
        create_project_from_template(&path, "my-app", &template).unwrap();

        let cargo_toml = fs::read_to_string(path.join("Cargo.toml")).unwrap();
        let forge_version = env!("CARGO_PKG_VERSION");

        assert!(cargo_toml.contains("version = \"1.0.0\""));
        assert!(
            cargo_toml.contains(&format!("version = \"{forge_version}\"")),
            "forge dependency should keep forge_version, only [package] version is pinned"
        );
    }

    #[test]
    fn test_pin_package_version_only_touches_first_version() {
        let input = "[package]\nname = \"x\"\nversion = \"9.9.9\"\nedition = \"2024\"\n\n[dependencies]\nfoo = { version = \"1.2.3\" }\n";
        let output = pin_package_version(input);
        assert!(output.contains("version = \"1.0.0\""));
        assert!(output.contains("foo = { version = \"1.2.3\" }"));
        assert!(!output.contains("version = \"9.9.9\""));
    }

    #[test]
    fn test_pin_package_version_leaves_non_package_tables_alone() {
        let input = "[workspace.package]\nversion = \"9.9.9\"\n";
        let output = pin_package_version(input);
        assert!(output.contains("version = \"9.9.9\""));
        assert!(!output.contains("1.0.0"));
    }

    #[test]
    fn test_invalid_template_error_lists_supported_templates() {
        let error = invalid_template_error("with-svelte/unknown");
        let message = error.to_string();

        assert!(message.contains("Unknown template 'with-svelte/unknown'"));
        assert!(message.contains("with-svelte/minimal"));
        assert!(message.contains("with-dioxus/realtime-todo-list"));
    }
}

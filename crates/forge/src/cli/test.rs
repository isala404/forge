use anyhow::Result;
use clap::Parser;
use console::style;
use std::net::TcpListener;
use std::path::Path;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::Command;

use super::frontend_target::FrontendTarget;
use super::ui;

/// Run project tests (backend unit tests and frontend Playwright tests).
///
/// Builds the project with an embedded frontend, starts a PostgreSQL
/// container, runs the binary on a random port, and executes Playwright
/// tests against the single-origin server.
#[derive(Parser)]
pub struct TestCommand {
    /// Skip backend unit tests
    #[arg(long)]
    pub skip_backend: bool,

    /// Skip frontend Playwright tests
    #[arg(long)]
    pub skip_frontend: bool,

    /// Run Playwright in interactive UI mode
    #[arg(long)]
    pub ui: bool,

    /// Run tests in a visible browser window
    #[arg(long)]
    pub headed: bool,

    /// Extra arguments passed through to the test runner
    #[arg(trailing_var_arg = true, allow_hyphen_values = true)]
    pub args: Vec<String>,
}

impl TestCommand {
    pub async fn execute(self) -> Result<()> {
        let root = super::project_root::enter_project_root()?;

        ui::section("FORGE Test");
        println!(
            "  {} Project root: {}",
            ui::info(),
            style(root.display()).cyan()
        );

        let mut any_failed = false;

        if !self.skip_backend && !self.run_backend_tests().await? {
            any_failed = true;
        }

        if !self.skip_frontend {
            let result = self.run_frontend_tests().await;
            match result {
                Ok(passed) => {
                    if !passed {
                        any_failed = true;
                    }
                }
                Err(e) => return Err(e),
            }
        }

        println!();
        if any_failed {
            println!("{} Some tests failed.", ui::error());
            std::process::exit(1);
        } else {
            println!("{} All tests passed.", ui::ok());
        }

        Ok(())
    }

    async fn run_backend_tests(&self) -> Result<bool> {
        println!();
        println!("  {} {}", ui::step(), style("Backend Tests").bold());

        let mut cargo_args = vec!["test"];

        if self.skip_frontend {
            for arg in &self.args {
                cargo_args.push(arg);
            }
        }

        println!("  {} Running: cargo {}", ui::step(), cargo_args.join(" "));
        println!();

        let status = Command::new("cargo")
            .args(&cargo_args)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .await?;

        if status.success() {
            println!();
            println!("  {} Backend tests passed.", ui::ok());
            Ok(true)
        } else {
            println!();
            println!("  {} Backend tests failed.", ui::error());
            Ok(false)
        }
    }

    async fn run_frontend_tests(&self) -> Result<bool> {
        let frontend_dir = Path::new("frontend");
        if !frontend_dir.exists() {
            println!();
            println!(
                "  {} No frontend/ directory, skipping frontend tests.",
                ui::info()
            );
            return Ok(true);
        }

        let tests_dir = frontend_dir.join("tests");
        if !tests_dir.exists() {
            println!();
            println!(
                "  {} No frontend/tests/ directory, skipping frontend tests.",
                ui::info()
            );
            return Ok(true);
        }

        println!();
        println!("  {} {}", ui::step(), style("Frontend Tests").bold());

        if let Some(url) = std::env::var("FORGE_TEST_URL")
            .ok()
            .filter(|v| !v.trim().is_empty())
        {
            print!("  {} Checking server...", ui::step());
            if wait_for_health(&url, Duration::from_secs(60)).await {
                println!(" {}", style("ready").green());
            } else {
                println!(" {}", style("not reachable").red());
                anyhow::bail!("FORGE_TEST_URL={url} is set but server is not reachable");
            }
            return self.execute_frontend_tests(frontend_dir, &url).await;
        }

        if !super::project_root::docker_available().await {
            anyhow::bail!(
                "Docker not available - install Docker to spin up a test Postgres,\n\
                or set FORGE_TEST_URL to point to a running server (and DATABASE_URL\n\
                for backend testcontainers tests)."
            );
        }

        let frontend_type = FrontendTarget::detect(frontend_dir);

        let db_name = read_db_name();
        println!("  {} Starting PostgreSQL...", ui::step());
        let (pg_container, pg_port) = start_postgres(&db_name).await?;
        let db_url = format!("postgres://postgres:forge@localhost:{pg_port}/{db_name}");

        let binary = match build_project(frontend_type).await {
            Ok(bin) => bin,
            Err(e) => {
                stop_postgres(&pg_container).await;
                return Err(e);
            }
        };

        let port = pick_random_port()?;
        let app_url = format!("http://localhost:{port}");

        println!("  {} Starting server on port {port}...", ui::step());
        let mut child = match start_server(&binary, port, &db_url).await {
            Ok(child) => child,
            Err(e) => {
                stop_postgres(&pg_container).await;
                return Err(e);
            }
        };

        print!("  {} Waiting for server...", ui::step());
        if !wait_for_health(&app_url, Duration::from_secs(120)).await {
            println!(" {}", style("timed out").red());
            let _ = child.kill().await;
            stop_postgres(&pg_container).await;
            anyhow::bail!(
                "Server did not become healthy within 120s.\n\
                Check the binary output for errors."
            );
        }
        println!(" {}", style("ready").green());

        let result = self.execute_frontend_tests(frontend_dir, &app_url).await;

        println!();
        println!("  {} Stopping server...", ui::step());
        let _ = child.kill().await;
        stop_postgres(&pg_container).await;

        result
    }

    async fn execute_frontend_tests(&self, frontend_dir: &Path, app_url: &str) -> Result<bool> {
        if !frontend_dir.join("node_modules").exists() {
            println!("  {} Installing frontend dependencies...", ui::step());
            let status = Command::new("bun")
                .args(["install"])
                .current_dir(frontend_dir)
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .await?;

            if !status.success() {
                anyhow::bail!("Failed to install frontend dependencies");
            }
        }

        let pw_check = Command::new("bunx")
            .args(["playwright", "test", "--list"])
            .current_dir(frontend_dir)
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;

        let needs_install = match pw_check {
            Ok(status) => !status.success(),
            Err(_) => true,
        };

        if needs_install {
            println!("  {} Installing Playwright browsers...", ui::step());
            let status = Command::new("bunx")
                .args(["playwright", "install", "chromium"])
                .current_dir(frontend_dir)
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .status()
                .await?;

            if !status.success() {
                anyhow::bail!("Failed to install Playwright browsers");
            }
        }

        let mut pw_args = vec!["playwright", "test"];

        if self.ui {
            pw_args.push("--ui");
        }

        if self.headed {
            pw_args.push("--headed");
        }

        if self.skip_backend {
            for arg in &self.args {
                pw_args.push(arg);
            }
        }

        println!();
        println!("  {} Running: bunx {}", ui::step(), pw_args.join(" "));
        println!();

        let status = Command::new("bunx")
            .args(&pw_args)
            .current_dir(frontend_dir)
            .env("FORGE_TEST_URL", app_url)
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .await?;

        if status.success() {
            println!();
            println!("  {} Frontend tests passed.", ui::ok());
            Ok(true)
        } else {
            println!();
            println!("  {} Frontend tests failed.", ui::error());
            println!(
                "  Debug with: {} or {}",
                style("forge test --skip-backend --ui").cyan(),
                style("forge test --skip-backend --headed").cyan()
            );
            Ok(false)
        }
    }
}

fn read_db_name() -> String {
    read_env_file(Path::new(".env"))
        .into_iter()
        .find(|(k, _)| k == "POSTGRES_DB")
        .map(|(_, v)| v)
        .unwrap_or_else(|| "test_db".to_string())
}

fn pick_random_port() -> Result<u16> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    drop(listener);
    Ok(port)
}

async fn start_postgres(db_name: &str) -> Result<(String, u16)> {
    let container_name = format!(
        "forge-test-pg-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    );

    let _ = Command::new("docker")
        .args(["rm", "-f", &container_name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;

    let status = Command::new("docker")
        .args([
            "run",
            "-d",
            "--name",
            &container_name,
            "-e",
            "POSTGRES_USER=postgres",
            "-e",
            "POSTGRES_PASSWORD=forge",
            "-e",
            &format!("POSTGRES_DB={db_name}"),
            "-p",
            "0:5432",
            "postgres:18",
        ])
        .stdout(Stdio::null())
        .stderr(Stdio::inherit())
        .status()
        .await?;

    if !status.success() {
        anyhow::bail!("Failed to start PostgreSQL container");
    }

    let output = Command::new("docker")
        .args(["port", &container_name, "5432"])
        .output()
        .await?;

    let port_str = String::from_utf8_lossy(&output.stdout);
    let port: u16 = port_str
        .trim()
        .rsplit(':')
        .next()
        .and_then(|p| p.parse().ok())
        .ok_or_else(|| anyhow::anyhow!("Could not parse PostgreSQL port from: {port_str}"))?;

    for _ in 0..30 {
        let check = Command::new("docker")
            .args([
                "exec",
                &container_name,
                "pg_isready",
                "-U",
                "postgres",
                "-d",
                db_name,
            ])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .await;

        if matches!(check, Ok(s) if s.success()) {
            return Ok((container_name, port));
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }

    let _ = Command::new("docker")
        .args(["rm", "-f", &container_name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;

    anyhow::bail!("PostgreSQL did not become ready within 30s")
}

async fn stop_postgres(container_name: &str) {
    let _ = Command::new("docker")
        .args(["rm", "-f", container_name])
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .await;
}

async fn build_project(frontend_type: Option<FrontendTarget>) -> Result<std::path::PathBuf> {
    println!("  {} Building project...", ui::step());

    let frontend_env = Path::new("frontend/.env");

    // SvelteKit reads PUBLIC_API_URL at build time; clear it so the frontend
    // uses relative URLs for same-origin serving in the test binary.
    let original_frontend_env = std::fs::read_to_string(frontend_env).ok();
    if matches!(frontend_type, Some(FrontendTarget::SvelteKit))
        && let Some(ref content) = original_frontend_env
    {
        let patched: String = content
            .lines()
            .map(|l| {
                if l.trim_start().starts_with("PUBLIC_API_URL=") {
                    "PUBLIC_API_URL="
                } else {
                    l
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        std::fs::write(frontend_env, patched)?;
    }

    // Build Dioxus WASM before cargo build: rust_embed requires real files in
    // frontend/dist/. Release mode is required — debug WASM embeds a hot-reload
    // WebSocket client that retries against the test backend, generating console
    // errors that flake the Playwright suite.
    if matches!(frontend_type, Some(FrontendTarget::Dioxus)) {
        println!("  {} Building Dioxus WASM frontend...", ui::step());
        let frontend_dir = Path::new("frontend");
        let status = Command::new("dx")
            .args(["build", "--platform", "web", "--release"])
            .current_dir(frontend_dir)
            .env("FORGE_API_URL", "")
            .stdin(Stdio::inherit())
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .status()
            .await?;

        if !status.success() {
            anyhow::bail!(
                "Dioxus frontend build failed.\n\
                Make sure dioxus-cli (dx) is installed: cargo install dioxus-cli"
            );
        }

        let dx_target = frontend_dir.join("target/dx");
        if let Ok(entries) = std::fs::read_dir(&dx_target) {
            for entry in entries.flatten() {
                for profile in ["release", "debug"] {
                    let public_dir = entry.path().join(profile).join("web/public");
                    if public_dir.exists() {
                        let dist_dir = frontend_dir.join("dist");
                        let _ = std::fs::remove_dir_all(&dist_dir);
                        copy_dir_recursive(&public_dir, &dist_dir)?;
                        break;
                    }
                }
            }
        }
    }

    let status = Command::new("cargo")
        .args(["build", "--features", "embedded-frontend"])
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit())
        .status()
        .await;

    // Restore before checking status so a failed build doesn't leave a patched .env
    if let Some(content) = original_frontend_env
        && let Err(e) = std::fs::write(frontend_env, content)
    {
        eprintln!("Warning: failed to restore frontend/.env: {e}");
    }

    if !status?.success() {
        anyhow::bail!("cargo build failed");
    }

    find_binary()
}

fn find_binary() -> Result<std::path::PathBuf> {
    let cargo_toml = std::fs::read_to_string("Cargo.toml")?;
    let name = cargo_toml
        .lines()
        .find(|l| l.starts_with("name"))
        .and_then(|l| l.split('=').nth(1))
        .map(|v| v.trim().trim_matches('"').to_string())
        .ok_or_else(|| anyhow::anyhow!("Could not find package name in Cargo.toml"))?;

    // Walk up to find the workspace root Cargo.toml; output lands there.
    let mut search_dir = std::env::current_dir()?;
    loop {
        let ws_toml = search_dir.join("Cargo.toml");
        if ws_toml.exists()
            && let Ok(content) = std::fs::read_to_string(&ws_toml)
            && content.contains("[workspace]")
        {
            let candidate = search_dir.join("target/debug").join(&name);
            if candidate.exists() {
                return Ok(candidate);
            }
        }
        if !search_dir.pop() {
            break;
        }
    }

    let local = std::path::PathBuf::from(format!("target/debug/{name}"));
    if local.exists() {
        return Ok(local);
    }

    anyhow::bail!(
        "Built binary not found for package '{name}'.\n\
        Checked workspace and local target/debug/ directories."
    )
}

fn read_env_file(path: &Path) -> Vec<(String, String)> {
    let content = match std::fs::read_to_string(path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };
    content
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let (key, value) = line.split_once('=')?;
            Some((key.trim().to_string(), value.trim().to_string()))
        })
        .collect()
}

async fn start_server(binary: &Path, port: u16, db_url: &str) -> Result<tokio::process::Child> {
    let mut cmd = Command::new(binary);

    for (key, value) in read_env_file(Path::new(".env")) {
        cmd.env(&key, &value);
    }

    cmd.env("PORT", port.to_string())
        .env("HOST", "0.0.0.0")
        .env("DATABASE_URL", db_url)
        .env("RUST_LOG", "warn")
        .stdout(Stdio::null())
        .stderr(Stdio::inherit());

    let child = cmd.spawn()?;
    Ok(child)
}

async fn wait_for_health(base_url: &str, timeout: Duration) -> bool {
    let health_url = format!("{base_url}/_api/health");
    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(2))
        .build()
        .unwrap_or_default();
    let start = std::time::Instant::now();

    while start.elapsed() < timeout {
        if client.get(&health_url).send().await.is_ok() {
            return true;
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
        print!(".");
    }
    false
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dest_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&entry.path(), &dest_path)?;
        } else {
            std::fs::copy(entry.path(), dest_path)?;
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn default_cmd() -> TestCommand {
        TestCommand {
            skip_backend: false,
            skip_frontend: false,
            ui: false,
            headed: false,
            args: vec![],
        }
    }

    #[test]
    fn test_command_default_runs_both() {
        let cmd = default_cmd();
        assert!(!cmd.skip_backend);
        assert!(!cmd.skip_frontend);
    }

    #[test]
    fn test_command_skip_backend() {
        let cmd = TestCommand {
            skip_backend: true,
            ..default_cmd()
        };
        assert!(cmd.skip_backend);
        assert!(!cmd.skip_frontend);
    }

    #[test]
    fn test_command_skip_frontend() {
        let cmd = TestCommand {
            skip_frontend: true,
            ..default_cmd()
        };
        assert!(!cmd.skip_backend);
        assert!(cmd.skip_frontend);
    }

    #[test]
    fn test_command_with_ui_and_args() {
        let cmd = TestCommand {
            ui: true,
            args: vec!["tests/todo.spec.ts".into()],
            ..default_cmd()
        };
        assert!(cmd.ui);
        assert_eq!(cmd.args.len(), 1);
    }

    #[test]
    fn test_command_headed() {
        let cmd = TestCommand {
            headed: true,
            ..default_cmd()
        };
        assert!(cmd.headed);
    }

    #[test]
    fn test_read_db_name_default() {
        assert!(!read_db_name().is_empty());
    }

    #[test]
    fn test_pick_random_port() {
        let port1 = pick_random_port().unwrap();
        let port2 = pick_random_port().unwrap();
        assert!(port1 > 0);
        assert!(port2 > 0);
    }
}

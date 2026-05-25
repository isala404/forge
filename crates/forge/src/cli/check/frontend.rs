use anyhow::Result;
use std::path::Path;
use std::process::Stdio;
use tokio::process::Command as TokioCommand;

use super::super::frontend_codegen::BindingGeneratorInput;
use super::super::frontend_target::FrontendTarget;

use super::bindings::format_generated_bindings_for_check;
use super::checks::collect_rs_files;
use super::{CheckCommand, CheckResult};

impl CheckCommand {
    pub(super) fn check_frontend(&self, result: &mut CheckResult) -> Result<()> {
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

    pub(super) fn check_generated_bindings(&self, result: &mut CheckResult) -> Result<()> {
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
                Ok(outcome) => {
                    for (path, msg) in &outcome.parse_failures {
                        result.warn(
                            &format!("Failed to parse {}: {}", path.display(), msg),
                            "Handlers in this file will be missing from generated bindings",
                        );
                    }
                    outcome.registry
                }
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

        // tempfile::tempdir() avoids PID-reuse collisions on long-lived CI containers.
        let tmp_handle = tempfile::tempdir_in(frontend_dir)?;
        let tmp_output = tmp_handle.path().join("bindings");
        std::fs::create_dir_all(&tmp_output)?;
        let tmp_output_str = tmp_output.to_string_lossy().to_string();

        let gen_result = target.generate_bindings(&BindingGeneratorInput {
            output_dir: &tmp_output_str,
            output_path: &tmp_output,
            registry: &registry,
            has_schema,
            force: true,
        });

        if let Err(e) = gen_result {
            result.warn(
                &format!("Could not regenerate bindings: {}", e),
                "Run 'forge generate' to check manually",
            );
            return Ok(());
        }

        if let Err(e) =
            format_generated_bindings_for_check(target, frontend_dir, output_path, &tmp_output)
        {
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

    pub(super) async fn check_frontend_linting(&self, result: &mut CheckResult) {
        let frontend_dir = Path::new("frontend");
        if !frontend_dir.exists() {
            return;
        }
        let target = FrontendTarget::detect(frontend_dir).unwrap_or(FrontendTarget::SvelteKit);

        println!();

        if target == FrontendTarget::Dioxus {
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

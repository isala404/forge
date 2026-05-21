use anyhow::Result;
use std::path::Path;
use std::process::{Command as StdCommand, Stdio};

use super::super::frontend_target::FrontendTarget;

pub(super) fn format_generated_bindings_for_check(
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

pub(super) fn generated_bindings_are_prettier_ignored(
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

use anyhow::Result;
use std::path::Path;

use super::{CheckCommand, CheckResult};

impl CheckCommand {
    pub(super) fn check_forge_toml(&self, result: &mut CheckResult) -> Result<()> {
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

    pub(super) fn check_cargo_toml(&self, result: &mut CheckResult) -> Result<()> {
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
}

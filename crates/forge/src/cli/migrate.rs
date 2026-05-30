use anyhow::Result;
use clap::{Parser, Subcommand};
use console::style;
use std::path::Path;

use forge_core::config::ForgeConfig;
use forge_runtime::Database;
use forge_runtime::pg::migration::{DriftStatus, MigrationRunner, load_migrations_from_dir};

use super::ui;

/// Manage database migrations.
#[derive(Parser)]
pub struct MigrateCommand {
    #[command(subcommand)]
    pub action: MigrateAction,

    /// Configuration file path.
    #[arg(short, long, default_value = "forge.toml", global = true)]
    pub config: String,

    /// Migrations directory path.
    #[arg(short, long, default_value = "migrations", global = true)]
    pub migrations_dir: String,
}

#[derive(Subcommand)]
pub enum MigrateAction {
    /// Run all pending migrations (default behavior).
    Up,

    /// Show migration status.
    Status,

    /// Generate .sqlx/ offline cache for compile-time query checking.
    Prepare {
        /// Apply pending migrations before generating the cache. Without this,
        /// prepare refuses to mutate a non-local DATABASE_URL unattended.
        #[arg(long)]
        with_up: bool,

        /// Skip the interactive confirmation prompt.
        #[arg(short = 'y', long)]
        yes: bool,
    },
}

impl MigrateCommand {
    pub async fn execute(self) -> Result<()> {
        let root = super::project_root::enter_project_root()?;

        dotenvy::dotenv().ok();

        println!(
            "  {} Project root: {}",
            ui::info(),
            style(root.display()).cyan()
        );

        let config_path = Path::new(&self.config);
        if !config_path.exists() {
            anyhow::bail!(
                "Configuration file not found: {}\nRun `forge new` or `forge init` to create a project.",
                self.config
            );
        }

        let config = ForgeConfig::from_file(&self.config)?;

        let db = Database::from_config_with_service(&config.database, &config.project.name).await?;
        let pool = db.primary().clone();
        let runner = MigrationRunner::new(pool);

        let migrations_dir = Path::new(&self.migrations_dir);
        let available = load_migrations_from_dir(migrations_dir)?;

        match self.action {
            MigrateAction::Up => {
                ui::section("FORGE Migrations");

                if available.is_empty() {
                    println!(
                        "  {} No migrations found in {}",
                        ui::info(),
                        self.migrations_dir
                    );
                    return Ok(());
                }

                println!("  {} Running pending migrations...", ui::step());
                runner.run(available).await?;
                println!("  {} Migrations complete", ui::ok());
                println!();
            }

            MigrateAction::Prepare { with_up, yes } => {
                ui::section("FORGE Prepare");

                let database_url_for_check = config.database.url().to_string();
                let is_local = database_url_is_local(&database_url_for_check);

                let pending = runner.status(&available).await?.pending;

                if !pending.is_empty() {
                    if !with_up {
                        let masked = mask_database_url(&database_url_for_check);
                        println!(
                            "  {} {} pending migration(s) detected.",
                            ui::warn(),
                            pending.len()
                        );
                        println!("    Target DATABASE_URL: {masked}");
                        if !is_local && !yes {
                            anyhow::bail!(
                                "Refusing to run pending migrations against a non-local database \
                                 without explicit consent.\n\n  \
                                 Re-run with `--with-up` to apply, or `--yes` to acknowledge the \
                                 target. Set DATABASE_URL to a localhost instance for unattended \
                                 use."
                            );
                        }
                        if !yes {
                            anyhow::bail!(
                                "Refusing to auto-run migrations from `forge migrate prepare`.\n  \
                                 Pass `--with-up` to apply, or run `forge migrate up` separately."
                            );
                        }
                    }
                    println!("  {} Running pending migrations...", ui::step());
                    runner.run(available).await?;
                    println!("  {} Migrations complete", ui::ok());
                }

                let has_cargo_sqlx = super::project_root::cargo_sqlx_available();

                if !has_cargo_sqlx {
                    anyhow::bail!(
                        "cargo-sqlx is required to generate the offline cache.\n\
                         Install it with:\n  \
                         cargo install sqlx-cli --no-default-features --features postgres"
                    );
                }

                let database_url = config.database.url();
                println!("  {} Generating .sqlx/ offline cache...", ui::step());

                let output = std::process::Command::new("cargo")
                    .args(["sqlx", "prepare", "--workspace"])
                    .env("DATABASE_URL", database_url)
                    .output()?;

                if output.status.success() {
                    println!("  {} Offline cache generated", ui::ok());
                } else {
                    let stderr = String::from_utf8_lossy(&output.stderr);
                    anyhow::bail!("cargo sqlx prepare failed:\n{}", stderr);
                }

                println!();
            }

            MigrateAction::Status => {
                ui::section("FORGE Migration Status");

                let status = runner.status(&available).await?;

                if status.applied.is_empty() && status.pending.is_empty() {
                    println!("  {} No migrations found", ui::info());
                    return Ok(());
                }

                let mut drifted = 0usize;
                let mut missing = 0usize;
                if !status.applied.is_empty() {
                    println!("  {} Applied:", ui::ok());
                    for m in &status.applied {
                        let drift_note = match &m.drift {
                            DriftStatus::Unchanged => String::new(),
                            DriftStatus::Drifted { current_checksum } => {
                                drifted += 1;
                                let short = current_checksum.get(..12).unwrap_or(current_checksum);
                                format!(" {}", style(format!("[DRIFT now={short}]")).yellow())
                            }
                            DriftStatus::SourceMissing => {
                                missing += 1;
                                format!(" {}", style("[SOURCE FILE MISSING]").red())
                            }
                        };
                        println!(
                            "    {} {} ({}){}",
                            style(&m.version).cyan(),
                            style("at").dim(),
                            m.applied_at.format("%Y-%m-%d %H:%M:%S"),
                            drift_note,
                        );
                    }
                }

                if !status.pending.is_empty() {
                    if !status.applied.is_empty() {
                        println!();
                    }
                    println!("  {} Pending:", ui::warn());
                    for name in &status.pending {
                        println!("    {} {}", ui::step(), style(name).yellow());
                    }
                }

                println!();
                println!(
                    "  {} {} applied, {} pending",
                    ui::info(),
                    status.applied.len(),
                    status.pending.len()
                );
                if drifted > 0 || missing > 0 {
                    println!(
                        "  {} {} drifted, {} missing source",
                        ui::warn(),
                        drifted,
                        missing,
                    );
                }
                println!();
            }
        }

        Ok(())
    }
}

/// True when the URL clearly targets a developer-local Postgres (no risk of
/// stomping a shared environment by accident).
fn database_url_is_local(url: &str) -> bool {
    let rest = match url
        .strip_prefix("postgres://")
        .or_else(|| url.strip_prefix("postgresql://"))
    {
        Some(r) => r,
        None => return false,
    };
    let host_section = rest.rsplit_once('@').map(|(_, r)| r).unwrap_or(rest);
    let host_port = host_section
        .split(['/', '?'])
        .next()
        .unwrap_or(host_section);
    const LOCAL: &[&str] = &["localhost", "127.0.0.1", "::1", "0.0.0.0"];
    if LOCAL.contains(&host_port) {
        return true;
    }
    // Strip trailing :port only when the suffix is all-digits and the
    // remaining host has no `:` (rules out IPv6 host without brackets).
    let host = match host_port.rsplit_once(':') {
        Some((h, p))
            if !p.is_empty() && p.chars().all(|c| c.is_ascii_digit()) && !h.contains(':') =>
        {
            h
        }
        _ => host_port,
    };
    LOCAL.contains(&host)
}

/// Replace the password in a `postgres[ql]://user:password@host…` URL with `***`.
fn mask_database_url(url: &str) -> String {
    let (scheme, rest) = match url.split_once("://") {
        Some(pair) => pair,
        None => return url.to_string(),
    };
    let Some((userinfo, host)) = rest.rsplit_once('@') else {
        return url.to_string();
    };
    let masked_userinfo = match userinfo.split_once(':') {
        Some((user, _pw)) => format!("{user}:***"),
        None => userinfo.to_string(),
    };
    format!("{scheme}://{masked_userinfo}@{host}")
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn database_url_is_local_basic() {
        assert!(database_url_is_local("postgres://u:p@localhost:5432/db"));
        assert!(database_url_is_local("postgres://u:p@127.0.0.1/db"));
        assert!(database_url_is_local("postgresql://u@::1/db"));
        assert!(!database_url_is_local("postgres://u:p@db.prod:5432/db"));
        assert!(!database_url_is_local("not-a-url"));
    }

    #[test]
    fn mask_database_url_basic() {
        assert_eq!(
            mask_database_url("postgres://u:secret@host:5432/db"),
            "postgres://u:***@host:5432/db"
        );
        assert_eq!(
            mask_database_url("postgres://host/db"),
            "postgres://host/db"
        );
    }
}

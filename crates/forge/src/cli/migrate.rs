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
    Prepare,
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

            MigrateAction::Prepare => {
                ui::section("FORGE Prepare");

                if !available.is_empty() {
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

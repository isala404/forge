use anyhow::Result;
use clap::{Parser, Subcommand};
use console::style;
use std::path::Path;

use forge_core::config::ForgeConfig;
use forge_runtime::migrations::{load_migrations_from_dir, MigrationRunner};
use forge_runtime::Database;

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

    /// Rollback the last N migrations.
    Down {
        /// Number of migrations to rollback.
        #[arg(default_value = "1")]
        count: usize,
    },

    /// Show migration status.
    Status,
}

impl MigrateCommand {
    pub async fn execute(self) -> Result<()> {
        // Load .env if present
        dotenvy::dotenv().ok();

        // Load configuration
        let config_path = Path::new(&self.config);
        if !config_path.exists() {
            anyhow::bail!(
                "Configuration file not found: {}\nRun `forge new` or `forge init` to create a project.",
                self.config
            );
        }

        let config = ForgeConfig::from_file(&self.config)?;

        // Connect to database
        let db = Database::from_config(&config.database).await?;
        let pool = db.primary().clone();
        let runner = MigrationRunner::new(pool);

        // Load available migrations
        let migrations_dir = Path::new(&self.migrations_dir);
        let available = load_migrations_from_dir(migrations_dir)?;

        match self.action {
            MigrateAction::Up => {
                println!();
                println!(
                    "  {}  {} Migrations",
                    style("⚒️").bold(),
                    style("FORGE").bold().cyan()
                );
                println!();

                if available.is_empty() {
                    println!(
                        "  {} No migrations found in {}",
                        style("ℹ").blue(),
                        self.migrations_dir
                    );
                    return Ok(());
                }

                println!("  {} Running pending migrations...", style("→").dim());
                runner.run(available).await?;
                println!("  {} Migrations complete", style("✓").green());
                println!();
            }

            MigrateAction::Down { count } => {
                println!();
                println!(
                    "  {}  {} Migrations",
                    style("⚒️").bold(),
                    style("FORGE").bold().cyan()
                );
                println!();

                if count == 0 {
                    println!("  {} Nothing to rollback (count=0)", style("ℹ").blue());
                    return Ok(());
                }

                println!(
                    "  {} Rolling back {} migration(s)...",
                    style("→").dim(),
                    count
                );

                let rolled_back = runner.rollback(count).await?;

                if rolled_back.is_empty() {
                    println!("  {} No migrations to rollback", style("ℹ").blue());
                } else {
                    for name in &rolled_back {
                        println!("  {} Rolled back: {}", style("✓").green(), name);
                    }
                    println!();
                    println!(
                        "  {} Rolled back {} migration(s)",
                        style("✓").green(),
                        rolled_back.len()
                    );
                }
                println!();
            }

            MigrateAction::Status => {
                println!();
                println!(
                    "  {}  {} Migration Status",
                    style("⚒️").bold(),
                    style("FORGE").bold().cyan()
                );
                println!();

                let status = runner.status(&available).await?;

                if status.applied.is_empty() && status.pending.is_empty() {
                    println!("  {} No migrations found", style("ℹ").blue());
                    return Ok(());
                }

                // Show applied migrations
                if !status.applied.is_empty() {
                    println!("  {} Applied:", style("✓").green());
                    for m in &status.applied {
                        let down_marker = if m.has_down {
                            style("↓").green().to_string()
                        } else {
                            style("-").dim().to_string()
                        };
                        println!(
                            "    {} {} {} ({})",
                            down_marker,
                            style(&m.name).cyan(),
                            style("at").dim(),
                            m.applied_at.format("%Y-%m-%d %H:%M:%S")
                        );
                    }
                }

                // Show pending migrations
                if !status.pending.is_empty() {
                    if !status.applied.is_empty() {
                        println!();
                    }
                    println!("  {} Pending:", style("○").yellow());
                    for name in &status.pending {
                        println!("    {} {}", style("→").dim(), style(name).yellow());
                    }
                }

                println!();
                println!(
                    "  {} {} applied, {} pending",
                    style("ℹ").blue(),
                    status.applied.len(),
                    status.pending.len()
                );
                println!();

                // Legend
                println!(
                    "  {} = has down migration, {} = no down migration",
                    style("↓").green(),
                    style("-").dim()
                );
                println!();
            }
        }

        Ok(())
    }
}

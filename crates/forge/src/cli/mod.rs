mod check;
mod doctor;
mod env;
mod frontend_codegen;
mod frontend_target;
mod generate;
mod handler_scaffold;
mod migrate;
mod new;
mod project_root;
mod template;
mod template_catalog;
mod test;
mod ui;
mod webhook;

pub use check::CheckCommand;
pub use doctor::DoctorCommand;
pub use env::EnvCommand;
pub use generate::GenerateCommand;
pub use migrate::MigrateCommand;
pub use new::NewCommand;
pub use test::TestCommand;
pub use webhook::WebhookCommand;

use anyhow::Result;
use clap::{Parser, Subcommand};

const ABOUT: &str = r#"FORGE - The Full-Stack Framework for the Impatient

Everything you need in one binary. No Redis, no Kafka, just PostgreSQL.

Quick Start:
  forge new my-app --template with-svelte/minimal
  cd my-app
  docker compose up --build      Start development

Learn more: https://tryforge.dev/docs"#;

const AFTER_HELP: &str = r#"Examples:
  forge new my-app --template with-svelte/minimal
  forge new my-app --template with-dioxus/realtime-todo-list
  docker compose up --build      Start development (requires Docker)
  docker compose down            Stop the development environment
  docker compose down -v         Stop and remove volumes
  forge test                     Run all tests (backend + frontend)
  forge test --skip-frontend     Run backend unit tests only
  forge test --skip-backend      Run frontend Playwright tests only
  forge test --skip-backend --ui Run frontend tests in Playwright UI mode
  forge check                    Validate project configuration
  forge generate                 Generate frontend bindings from backend code
  forge migrate status           Check migration status
"#;

/// FORGE - The Full-Stack Framework for the Impatient
#[derive(Parser)]
#[command(name = "forge")]
#[command(author, version)]
#[command(about = ABOUT, long_about = None, after_help = AFTER_HELP)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

/// CLI commands.
#[derive(Subcommand)]
pub enum Commands {
    /// Create a new FORGE project
    New(NewCommand),

    /// Validate project configuration and dependencies
    Check(CheckCommand),

    /// Generate frontend bindings from backend source
    Generate(GenerateCommand),

    /// Run project tests (backend + frontend)
    Test(TestCommand),

    /// Manage database migrations
    Migrate(MigrateCommand),

    /// Diagnose your dev environment
    Doctor(DoctorCommand),

    /// Print shell-init snippets (eval "$(forge env)")
    Env(EnvCommand),

    /// Manage webhooks (list events, replay failed deliveries)
    Webhook(WebhookCommand),
}

impl Cli {
    /// Execute the CLI command.
    pub async fn execute(self) -> Result<()> {
        match self.command {
            Commands::New(cmd) => cmd.execute().await,
            Commands::Check(cmd) => cmd.execute().await,
            Commands::Test(cmd) => cmd.execute().await,
            Commands::Generate(cmd) => cmd.execute().await,
            Commands::Migrate(cmd) => cmd.execute().await,
            Commands::Doctor(cmd) => cmd.execute().await,
            Commands::Env(cmd) => cmd.execute().await,
            Commands::Webhook(cmd) => cmd.execute().await.map_err(|e| anyhow::anyhow!("{e}")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_parse_new_template() {
        let cli = Cli::try_parse_from([
            "forge",
            "new",
            "my-app",
            "--template",
            "with-svelte/minimal",
        ]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_new_handler() {
        let cli = Cli::try_parse_from(["forge", "new", "query", "list_invoices"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_new_bare_arg() {
        // `forge new my-app` parses; runtime decides handler vs project mode
        // and surfaces a friendlier error than clap's required-arg complaint.
        let cli = Cli::try_parse_from(["forge", "new", "my-app"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_generate() {
        let cli = Cli::try_parse_from(["forge", "generate"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_test() {
        let cli = Cli::try_parse_from(["forge", "test"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_test_ui() {
        let cli = Cli::try_parse_from(["forge", "test", "--ui"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_test_with_args() {
        let cli = Cli::try_parse_from(["forge", "test", "tests/home.spec.ts", "--headed"]);
        assert!(cli.is_ok());
    }
}

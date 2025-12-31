mod add;
mod generate;
mod migrate;
mod new;
mod run;
mod runtime_generator;
mod template;

pub use add::AddCommand;
pub use generate::GenerateCommand;
pub use migrate::MigrateCommand;
pub use new::NewCommand;
pub use run::RunCommand;

use anyhow::Result;
use clap::{Parser, Subcommand};

/// FORGE - The Rust Full-Stack Framework
#[derive(Parser)]
#[command(name = "forge")]
#[command(author, version, about, long_about = None)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Commands,
}

/// CLI commands.
#[derive(Subcommand)]
pub enum Commands {
    /// Create a new FORGE project.
    New(NewCommand),

    /// Initialize FORGE in an existing directory.
    Init(InitCommand),

    /// Add a new component (model, query, mutation, etc.).
    Add(AddCommand),

    /// Generate TypeScript client code.
    Generate(GenerateCommand),

    /// Run the FORGE server.
    Run(RunCommand),

    /// Manage database migrations.
    Migrate(MigrateCommand),
}

/// Initialize in existing directory.
#[derive(Parser)]
pub struct InitCommand {
    /// Project name (defaults to directory name).
    #[arg(short, long)]
    pub name: Option<String>,

    /// Use minimal template (no frontend).
    #[arg(long)]
    pub minimal: bool,
}

impl Cli {
    /// Execute the CLI command.
    pub async fn execute(self) -> Result<()> {
        match self.command {
            Commands::New(cmd) => cmd.execute().await,
            Commands::Init(cmd) => init_project(cmd).await,
            Commands::Add(cmd) => cmd.execute().await,
            Commands::Generate(cmd) => cmd.execute().await,
            Commands::Run(cmd) => cmd.execute().await,
            Commands::Migrate(cmd) => cmd.execute().await,
        }
    }
}

/// Initialize a new project in the current directory.
async fn init_project(cmd: InitCommand) -> Result<()> {
    let current_dir = std::env::current_dir()?;
    let name = cmd.name.unwrap_or_else(|| {
        current_dir
            .file_name()
            .and_then(|s| s.to_str())
            .unwrap_or("forge-app")
            .to_string()
    });

    new::create_project(&current_dir, &name, cmd.minimal)?;
    println!("✅ Initialized FORGE project: {}", name);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cli_parse() {
        let cli = Cli::try_parse_from(["forge", "new", "my-app"]);
        assert!(cli.is_ok());
    }

    #[test]
    fn test_cli_parse_add() {
        let cli = Cli::try_parse_from(["forge", "add", "model", "User"]);
        assert!(cli.is_ok());
    }
}

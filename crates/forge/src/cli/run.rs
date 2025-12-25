use anyhow::Result;
use clap::Parser;
use console::style;
use tracing::info;

use crate::runtime::Forge;

/// Run the FORGE server.
#[derive(Parser)]
pub struct RunCommand {
    /// Configuration file path.
    #[arg(short, long, default_value = "forge.toml")]
    pub config: String,

    /// Port to listen on (overrides config).
    #[arg(short, long)]
    pub port: Option<u16>,

    /// Host to bind to (overrides config).
    #[arg(long)]
    pub host: Option<String>,

    /// Enable development mode (hot reload, verbose logging).
    #[arg(long)]
    pub dev: bool,
}

impl RunCommand {
    /// Execute the run command.
    pub async fn execute(self) -> Result<()> {
        // Initialize tracing
        let log_level = if self.dev { "debug" } else { "info" };
        tracing_subscriber::fmt()
            .with_env_filter(std::env::var("RUST_LOG").unwrap_or_else(|_| log_level.to_string()))
            .init();

        println!();
        println!(
            "  {}  {} v{}",
            style("⚒️").bold(),
            style("FORGE").bold().cyan(),
            env!("CARGO_PKG_VERSION")
        );
        println!();

        // Check for config file
        let config_path = std::path::Path::new(&self.config);
        if !config_path.exists() {
            anyhow::bail!(
                "Configuration file not found: {}\nRun `forge new` or `forge init` to create a project.",
                self.config
            );
        }

        info!("Loading configuration from {}", self.config);

        // Load configuration
        let mut config = forge_core::config::ForgeConfig::from_file(&self.config)?;

        // Apply command-line overrides
        if let Some(port) = self.port {
            config.gateway.port = port;
        }

        let host = self.host.clone().unwrap_or_else(|| "127.0.0.1".to_string());
        let port = config.gateway.port;

        println!(
            "  {} Listening on {}",
            style("🌐").bold(),
            style(format!("http://{}:{}", host, port)).cyan()
        );
        println!(
            "  {} Dashboard at {}",
            style("📊").bold(),
            style(format!("http://{}:{}/_dashboard", host, port)).cyan()
        );

        if self.dev {
            println!("  {} Development mode enabled", style("🔧").bold());
        }

        println!();

        // Build and run the FORGE runtime
        let forge = Forge::builder()
            .config(config)
            .build()
            .map_err(|e| anyhow::anyhow!("{}", e))?;

        // Run the server (blocks until shutdown)
        forge.run().await.map_err(|e| anyhow::anyhow!("{}", e))?;

        println!("\n  {} Goodbye!", style("👋").bold());

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_run_command_defaults() {
        let cmd = RunCommand {
            config: "forge.toml".to_string(),
            port: None,
            host: None,
            dev: false,
        };
        assert_eq!(cmd.config, "forge.toml");
        assert!(!cmd.dev);
    }

    #[test]
    fn test_run_command_with_overrides() {
        let cmd = RunCommand {
            config: "custom.toml".to_string(),
            port: Some(3000),
            host: Some("0.0.0.0".to_string()),
            dev: true,
        };
        assert_eq!(cmd.config, "custom.toml");
        assert_eq!(cmd.port, Some(3000));
        assert_eq!(cmd.host, Some("0.0.0.0".to_string()));
        assert!(cmd.dev);
    }
}

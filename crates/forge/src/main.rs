mod cli;
mod runtime;

pub use runtime::prelude;
pub use runtime::{Forge, ForgeBuilder};

use anyhow::Result;
use clap::Parser;

#[tokio::main]
async fn main() -> Result<()> {
    let cli = cli::Cli::parse();
    cli.execute().await
}

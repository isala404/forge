use forge::prelude::*;

mod functions;
mod schema;

#[tokio::main]
async fn main() -> Result<()> {
    let config_path = std::env::var("FORGE_CONFIG")
        .map_err(|_| ForgeError::config("FORGE_CONFIG is required for benchmark app"))?;
    let config = ForgeConfig::from_file(&config_path)?;

    Forge::builder()
        .auto_register()
        .config(config)
        .build()?
        .run()
        .await
}

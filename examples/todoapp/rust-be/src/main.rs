mod routes;
mod types;
mod util;

use std::time::Duration;

use forge::{Forge, ForgeConfig};
use rocket::{Build, Rocket};

use crate::routes::AppState;
use crate::util::env_or;

async fn maintenance(forge: Forge) {
    loop {
        if let Err(err) = forge.run_scheduler_once().await {
            tracing::warn!(error = %err, "scheduler tick failed");
        }
        if let Err(err) = forge.maintain().await {
            tracing::warn!(error = %err, "maintenance sweep failed");
        }
        tokio::time::sleep(Duration::from_secs(30)).await;
    }
}

fn app(forge: Forge, bind: String, port: u16) -> Rocket<Build> {
    let figment = rocket::Config::figment()
        .merge(("address", bind))
        .merge(("port", port));

    routes::mount_routes(rocket::custom(figment).manage(AppState { forge }))
}

#[rocket::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn".into()),
        )
        .init();

    let pg = env_or(
        "FORGE_POSTGRES_URL",
        "postgres://postgres:forge@127.0.0.1:5432/todoapp_rust",
    );
    let forge = Forge::init(ForgeConfig::new(pg).with_env_overrides()?).await?;
    tokio::spawn(maintenance(forge.clone()));

    let port = env_or("PORT", "9081").parse::<u16>()?;
    let bind = env_or("BIND", "127.0.0.1");
    tracing::info!("todoapp-rust-be listening on http://{bind}:{port}");
    app(forge, bind, port).launch().await?;
    Ok(())
}

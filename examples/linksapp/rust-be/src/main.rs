mod routes;
mod types;
mod util;
mod worker;

use forgelib::Forge;
use rocket::{Build, Rocket};

use crate::routes::AppState;
use crate::util::env_or;

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

    // Reads ./forge.toml (the connection string and any ${VAR} references live there).
    let forge = Forge::init().await?;

    tokio::spawn(worker::run_clicks_worker(forge.clone()));
    tokio::spawn(worker::run_expire_worker(forge.clone()));
    tokio::spawn(worker::run_scheduler_loop(forge.clone()));

    let port = env_or("PORT", "9091").parse::<u16>()?;
    let bind = env_or("BIND", "127.0.0.1");
    tracing::info!("linksapp-rust-be listening on http://{bind}:{port}");
    app(forge, bind, port).launch().await?;
    Ok(())
}

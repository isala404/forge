//! chatapp Rust backend: a pure GraphQL API over Forge. axum 0.8 + async-graphql 7
//! (code-first, with DataLoader) sharing Forge's Postgres pool.

mod context;
mod db;
mod gql;
mod http;
mod loaders;
mod worker;

use std::sync::Arc;

use anyhow::Result;
use forge::{Forge, ForgeConfig};

use context::{AppCtx, Ctx, scheduler_interval};
use http::AppState;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_string())
}

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}

#[tokio::main]
async fn main() -> Result<()> {
    // Emit the SDL without touching the database — useful for parity checks in CI.
    if std::env::args().any(|a| a == "--print-schema") {
        print!("{}", gql::sdl());
        return Ok(());
    }

    tracing_subscriber::fmt()
        .with_writer(std::io::stderr)
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,sqlx=warn".into()),
        )
        .init();

    let pg = env_or(
        "FORGE_POSTGRES_URL",
        "postgres://postgres:forge@127.0.0.1:5432/chatapp_rust",
    );
    let forge = Forge::init(
        ForgeConfig::new(&pg)
            .with_blob_signing_secret(env_or("FORGE_BLOB_SIGNING_SECRET", "dev-secret-change-me"))
            .with_blob_base_url("/_forge/blob"),
    )
    .await?;

    // Reuse Forge's pool for the domain tables rather than opening a second one.
    let pool = forge.pool().clone();
    db::migrate(&pool).await?;

    let ctx: Ctx = Arc::new(AppCtx {
        forge: forge.clone(),
        pool,
    });
    let schema = gql::schema(ctx.clone());

    tokio::spawn(worker::run_fanout(ctx.clone(), shutdown()));
    tokio::spawn(worker::run_reap(ctx.clone(), shutdown()));
    tokio::spawn(worker::run_fail(ctx.clone(), shutdown()));
    tokio::spawn(housekeeping(ctx.clone()));

    let app = http::router(AppState {
        schema,
        ctx: ctx.clone(),
    })
    .nest("/_forge/blob", forge.blob_router()?);

    let port = env_or("PORT", "8081");
    // In a container set BIND=0.0.0.0 so the published port reaches the process.
    let addr = format!("{}:{port}", env_or("BIND", "127.0.0.1"));
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    tracing::info!("chatapp-rust-be listening on http://{addr}");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}

/// Each tick fires due `schedule` jobs into the queue and runs Forge's maintenance
/// sweep. Both idempotent and safe on every replica.
async fn housekeeping(ctx: Ctx) {
    let interval = scheduler_interval();
    loop {
        if let Err(e) = ctx.forge.run_scheduler_once().await {
            tracing::warn!(error = %e, "scheduler tick failed");
        }
        // Heal any reap/fanout whose post-commit enqueue was lost (separate pools, no
        // shared tx). Bounded and idempotent, so a failure just retries next tick.
        if let Err(e) = worker::reconcile(&ctx).await {
            tracing::warn!(error = %e, "reconciliation sweep failed");
        }
        if let Err(e) = ctx.forge.maintain().await {
            tracing::warn!(error = %e, "forge maintain failed");
        }
        tokio::select! {
            _ = tokio::time::sleep(interval) => {},
            _ = shutdown() => break,
        }
    }
}

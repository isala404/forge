mod blob_router;
mod context;
mod db;
mod gql;
mod http;
mod loaders;
mod worker;

use std::sync::Arc;

use anyhow::Result;
use forgelib::Forge;

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
    // Emit the SDL without touching the database, useful for parity checks in CI.
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

    // Reads ./forge.toml; Forge owns its database and migrates the forge_* tables at
    // startup. The connection string and blob signing secret live in that file.
    let forge = Forge::init().await?;

    // Reuse Forge's pool for the domain tables rather than opening a second one.
    let pool = forge.pool().clone();
    db::migrate(&pool).await?;

    // Mint the login decoy hash once, via forge's own hasher so its argon2 params
    // always match real password hashes. `login` verifies against it on a username
    // miss to keep that path's timing indistinguishable from a real verify.
    let decoy_hash = forge
        .auth()
        .hash_password(&uuid::Uuid::new_v4().to_string())
        .await?;

    let ctx: Ctx = Arc::new(AppCtx {
        forge: forge.clone(),
        pool,
        decoy_hash,
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
    .nest("/api/files", blob_router::router(ctx.clone()));

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

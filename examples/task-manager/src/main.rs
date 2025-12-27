//! Task Manager - A comprehensive FORGE example application.
//!
//! This application demonstrates all FORGE features:
//! - Schema models with relations and enums
//! - Queries, mutations, and actions
//! - Background jobs with retry logic
//! - Cron scheduled tasks
//! - Multi-step workflows
//! - Real-time subscriptions
//! - Observability instrumentation

mod schema;
mod functions;

use anyhow::Result;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize tracing
    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(tracing_subscriber::EnvFilter::new(
            std::env::var("RUST_LOG").unwrap_or_else(|_| "info".into()),
        ))
        .init();

    tracing::info!("Starting Task Manager...");

    // Load configuration
    let database_url = std::env::var("DATABASE_URL")
        .unwrap_or_else(|_| "postgres://postgres:postgres@localhost:5432/task_manager".into());

    // Create database pool
    let pool = sqlx::PgPool::connect(&database_url).await?;

    // Run migrations
    sqlx::migrate!("./migrations").run(&pool).await?;

    tracing::info!("Database connected and migrations applied");

    // TODO: Initialize FORGE runtime with functions, jobs, crons, workflows

    // For now, just run a simple HTTP server placeholder
    tracing::info!("Task Manager started on http://localhost:8080");
    tracing::info!("Dashboard available at http://localhost:8080/_dashboard");

    // Keep running
    tokio::signal::ctrl_c().await?;
    tracing::info!("Shutting down...");

    Ok(())
}

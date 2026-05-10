//! Materialized view refresh for signals dashboards.
//!
//! Refreshes views concurrently (non-blocking reads) every 5 minutes.

use sqlx::PgPool;
use tracing::{debug, error};

/// Refresh all signals materialized views concurrently.
// Single SELECT against a runtime-owned function; offline cache may not include it.
#[allow(clippy::disallowed_methods)]
pub async fn refresh_views(pool: &PgPool) {
    let result = sqlx::query("SELECT forge_signals_refresh_views()")
        .execute(pool)
        .await;

    match result {
        Ok(_) => debug!("refreshed signal materialized views"),
        Err(e) => error!(error = %e, "failed to refresh signal materialized views"),
    }
}

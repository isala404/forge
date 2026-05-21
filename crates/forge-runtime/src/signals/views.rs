//! Incremental rollup refresh for signals dashboards.
//!
//! Rolls up hourly stats from `forge_signals_events`, then daily from hourly.

use sqlx::PgPool;
use tracing::{debug, error};

/// Roll up the current and previous hour, then the current day.
#[allow(clippy::disallowed_methods)]
pub async fn refresh_views(pool: &PgPool) {
    let now = chrono::Utc::now();
    let current_hour = now.format("%Y-%m-%d %H:00:00+00").to_string();
    let prev_hour = (now - chrono::Duration::hours(1))
        .format("%Y-%m-%d %H:00:00+00")
        .to_string();
    let today = now.format("%Y-%m-%d").to_string();

    for hour in [&prev_hour, &current_hour] {
        let result = sqlx::query("SELECT forge_signals_roll_up_hour($1::timestamptz)")
            .bind(hour)
            .execute(pool)
            .await;
        if let Err(e) = result {
            error!(error = %e, hour = %hour, "failed to roll up hourly signals stats");
            return;
        }
    }

    let result = sqlx::query("SELECT forge_signals_roll_up_day($1::date)")
        .bind(&today)
        .execute(pool)
        .await;

    match result {
        Ok(_) => debug!("rolled up signals stats"),
        Err(e) => error!(error = %e, day = %today, "failed to roll up daily signals stats"),
    }
}

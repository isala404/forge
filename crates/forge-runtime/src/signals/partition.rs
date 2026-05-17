//! Monthly partition management for the signals events table.
//!
//! Creates partitions for upcoming months and drops partitions
//! older than the configured retention period.

// Partition DDL constructs table names from runtime dates, so the query macros
// can't validate them at compile time.
#![allow(clippy::disallowed_methods)]

use sqlx::PgPool;
use tracing::{debug, error, info};

/// Ensure partitions exist for the current month and the next three months.
///
/// Pre-creating three months ahead prevents gaps when the maintenance loop
/// (which sleeps a fixed interval from startup, not aligned to midnight)
/// fires late on a month boundary.
pub async fn ensure_partitions(pool: &PgPool) {
    let results = tokio::join!(
        sqlx::query(
            "SELECT forge_signals_ensure_partition(date_trunc('month', CURRENT_DATE)::date)",
        )
        .execute(pool),
        sqlx::query(
            "SELECT forge_signals_ensure_partition((date_trunc('month', CURRENT_DATE) + interval '1 month')::date)",
        )
        .execute(pool),
        sqlx::query(
            "SELECT forge_signals_ensure_partition((date_trunc('month', CURRENT_DATE) + interval '2 months')::date)",
        )
        .execute(pool),
        sqlx::query(
            "SELECT forge_signals_ensure_partition((date_trunc('month', CURRENT_DATE) + interval '3 months')::date)",
        )
        .execute(pool),
    );

    let labels = ["current", "+1 month", "+2 months", "+3 months"];
    for (result, label) in [results.0, results.1, results.2, results.3]
        .into_iter()
        .zip(labels)
    {
        if let Err(e) = result {
            error!(error = %e, month = label, "failed to ensure signal partition");
        }
    }
    debug!("signal partitions verified (current + 3 months ahead)");
}

/// Drop partitions older than the retention period.
pub async fn drop_old_partitions(pool: &PgPool, retention_days: u32) {
    let result = sqlx::query_scalar::<_, i32>("SELECT forge_signals_drop_old_partitions($1)")
        .bind(retention_days as i32)
        .fetch_one(pool)
        .await;

    match result {
        Ok(dropped) if dropped > 0 => {
            info!(dropped, retention_days, "dropped old signal partitions");
        }
        Ok(_) => debug!(retention_days, "no old signal partitions to drop"),
        Err(e) => error!(error = %e, "failed to drop old signal partitions"),
    }
}

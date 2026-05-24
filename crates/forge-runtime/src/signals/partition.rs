//! Monthly partition management for the signals events table.
//!
//! Creates partitions for upcoming months and drops partitions
//! older than the configured retention period.
//!
//! ## Operational expectation
//!
//! Pre-creation runs every maintenance tick and covers the current month plus
//! the next three months. Anything farther out lands in the catch-all
//! `forge_signals_events_default` partition, which is **excluded from the
//! retention sweep** — rows there accumulate forever and won't be dropped.
//!
//! Two failure modes to watch:
//!
//! 1. A node hibernates / loses its scheduler past the +3-month horizon. When
//!    it wakes back up, any inserts whose `timestamp` falls outside the
//!    rolling window land in the default partition until the maintenance loop
//!    next runs.
//! 2. A client sends events with `timestamp` far in the future (clock skew,
//!    backfill jobs). Same outcome.
//!
//! `check_default_partition` logs an error whenever rows are present in the
//! default partition. Treat that log as actionable: investigate the coverage
//! gap, then move the misrouted rows into the correct month partition by
//! hand (or accept that they'll never be cleaned up by retention).

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

/// Warn if any rows have landed in the catch-all partition.
///
/// `forge_signals_events_default` catches rows whose timestamp doesn't match
/// any month partition. The retention sweep explicitly skips it, so anything
/// that lands here accumulates forever. A non-zero count means either a
/// `forge_signals_ensure_partition` failure left a gap, or clients are
/// inserting events with timestamps outside the rolling +3-month window.
pub async fn check_default_partition(pool: &PgPool) {
    let result = sqlx::query_scalar::<_, i64>("SELECT count(*) FROM forge_signals_events_default")
        .fetch_one(pool)
        .await;

    match result {
        Ok(0) => debug!("signals default partition empty"),
        Ok(count) => error!(
            misrouted_rows = count,
            "signals: rows landed in forge_signals_events_default — a partition is missing for some range. \
             These rows are excluded from retention drops; investigate the partition coverage."
        ),
        Err(e) => error!(error = %e, "failed to inspect signals default partition"),
    }
}

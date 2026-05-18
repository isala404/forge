//! Recovery helpers for `forge_change_log`.
//!
//! Doctrine: every consumer that needs at-least-once semantics over the
//! framework's table-change feed reads the durable change log through this
//! module. The table itself, the trigger that writes to it, and the trim
//! function all live in system migration `__forge_v002`.
//!
//! # Recovery model
//!
//! The change log is a monotonic, partitioned-by-time tail of every row
//! INSERT/UPDATE/DELETE in user tables that have been wired up to
//! `forge_notify_change()`. Each row gets a `BIGSERIAL seq` and a server-side
//! `created_at`. Consumers track the last `seq` they processed; on reconnect
//! after a NOTIFY drop, they call [`drain_change_log`] with that high-water
//! mark to replay anything they missed. If [`min_seq`] reports a value greater
//! than the consumer's high-water mark, retention has trimmed past it and the
//! consumer must fall back to a full resync of its derived state.
//!
//! Default retention is 1 hour with a minimum floor of 1 000 rows — trimming
//! is skipped entirely when the log is smaller than that floor, even if
//! entries are older than the window. Run [`trim_change_log`] periodically
//! from a background task; the helpers themselves never trim implicitly.

use chrono::{DateTime, Utc};
use futures_util::stream::{Stream, StreamExt};
use sqlx::PgPool;

use forge_core::cluster::LeaderRole;
use forge_core::error::{ForgeError, Result};

/// One row of `forge_change_log`.
///
/// Mirrors the columns produced by the v002 migration's
/// `forge_notify_change()` trigger. Kept deliberately close to the wire shape
/// so callers can decide how to map `row_id` (TEXT, may not be a UUID for
/// every table) and the comma-separated `changed_cols` string.
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ChangeRow {
    /// Monotonic sequence number; the consumer's high-water mark.
    pub seq: i64,
    /// Table that produced the change.
    pub table_name: String,
    /// SQL operation: `INSERT`, `UPDATE`, or `DELETE`.
    pub op: String,
    /// Text-encoded primary key. `None` only when the trigger could not
    /// resolve `OLD.id`/`NEW.id` (e.g. a table with a non-`id` PK).
    pub row_id: Option<String>,
    /// For `UPDATE`, comma-separated column names whose values changed. `None`
    /// for `INSERT`/`DELETE` and for `UPDATE`s where no columns differ.
    pub changed_cols: Option<String>,
    /// Server-side timestamp at which the trigger inserted the row.
    pub created_at: DateTime<Utc>,
}

/// Stream every change with `seq > since`, in ascending `seq` order.
///
/// Use to recover from a `LISTEN/NOTIFY` gap: track `last_seq`, and call this
/// on reconnect with that value to replay anything missed. The result is
/// already ordered so the consumer can advance its cursor as it processes
/// rows.
///
/// Errors from PG flow through the stream as `Err(ForgeError::Database)`. The
/// caller decides whether a transient error ends recovery or triggers a
/// retry.
pub fn drain_change_log(pool: &PgPool, since: i64) -> impl Stream<Item = Result<ChangeRow>> + '_ {
    sqlx::query_as!(
        ChangeRow,
        r#"SELECT seq, table_name, op, row_id, changed_cols, created_at
           FROM forge_change_log WHERE seq > $1 ORDER BY seq"#,
        since,
    )
    .fetch(pool)
    .map(|r| r.map_err(ForgeError::Database))
}

/// Minimum number of rows that must exist in `forge_change_log` before any
/// time-based trimming occurs. On quiet systems this prevents the entire log
/// from being wiped even though the rows are technically past the retention
/// window; consumers rely on the log for gap-recovery and benefit from having
/// at least this many recent entries available.
pub const CHANGE_LOG_MIN_ROWS: i64 = 1_000;

/// Delete every change-log row with `created_at < before`. Returns the number
/// of rows deleted.
///
/// Run periodically from a background task; the reactor's gap-recovery only
/// needs the recent tail. Default retention in the framework is 1 hour, set
/// by passing `Utc::now() - chrono::Duration::hours(1)`.
///
/// Trimming is skipped when the total row count is at or below
/// [`CHANGE_LOG_MIN_ROWS`] (default 1 000). This prevents over-aggressive
/// cleanup on quiet systems where entries age past the window but the log
/// itself is small.
///
/// Uses `pg_try_advisory_xact_lock` with the [`LeaderRole::LogCompactor`] lock
/// ID so that only one node in the cluster runs the DELETE at a time. If
/// another node already holds the lock the function returns `Ok(0)` immediately
/// rather than blocking or racing.
#[allow(clippy::disallowed_methods)]
pub async fn trim_change_log(pool: &PgPool, before: DateTime<Utc>) -> Result<u64> {
    let lock_id = LeaderRole::LogCompactor.lock_id();

    let mut tx = pool.begin().await.map_err(ForgeError::Database)?;

    // pg_try_advisory_xact_lock is a built-in function with no user-table
    // schema, so it has no cached .sqlx metadata and must use the runtime API.
    let acquired: bool = sqlx::query_scalar("SELECT pg_try_advisory_xact_lock($1)")
        .bind(lock_id)
        .fetch_one(&mut *tx)
        .await
        .map_err(ForgeError::Database)?;

    if !acquired {
        // Another node is already trimming; skip without blocking.
        return Ok(0);
    }

    // Enforce a minimum row count floor: don't trim at all when the log is
    // small, even if the entries are past the retention window. This prevents
    // total log erasure on quiet systems where consumers still need recent
    // history for gap-recovery.
    let total: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM forge_change_log")
        .fetch_one(&mut *tx)
        .await
        .map_err(ForgeError::Database)?;

    if total <= CHANGE_LOG_MIN_ROWS {
        // Log is small enough; leave it intact regardless of age.
        return Ok(0);
    }

    let result = sqlx::query!("DELETE FROM forge_change_log WHERE created_at < $1", before,)
        .execute(&mut *tx)
        .await
        .map_err(ForgeError::Database)?;

    tx.commit().await.map_err(ForgeError::Database)?;

    Ok(result.rows_affected())
}

/// Read the smallest `seq` currently in the log, or `None` if it is empty.
///
/// Pair with the consumer's high-water mark to detect unrecoverable gaps:
/// if `min_seq()` is `Some(n)` with `n > last_seq`, retention has trimmed
/// past the consumer's position and a full resync is required.
pub async fn min_seq(pool: &PgPool) -> Result<Option<i64>> {
    let row = sqlx::query!(r#"SELECT MIN(seq) AS min FROM forge_change_log"#)
        .fetch_one(pool)
        .await
        .map_err(ForgeError::Database)?;
    Ok(row.min)
}

/// Read the largest `seq` currently in the log, or `None` if it is empty.
///
/// Used at startup to initialize the listener's high-water mark so the first
/// reconnect does not replay the entire log.
#[allow(clippy::disallowed_methods)]
pub async fn max_seq(pool: &PgPool) -> Result<Option<i64>> {
    let row: (Option<i64>,) = sqlx::query_as("SELECT MAX(seq) FROM forge_change_log")
        .fetch_one(pool)
        .await
        .map_err(ForgeError::Database)?;
    Ok(row.0)
}

#[cfg(all(test, feature = "testcontainers"))]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::disallowed_methods
)]
mod integration_tests {
    use super::*;
    use forge_core::testing::{IsolatedTestDb, TestDatabase};
    use futures_util::stream::TryStreamExt;

    async fn setup_db(test_name: &str) -> IsolatedTestDb {
        let base = TestDatabase::from_env()
            .await
            .expect("Failed to create test database");
        base.isolated(test_name)
            .await
            .expect("Failed to create isolated db")
    }

    /// Create a tracked table that fires the change trigger on every write.
    async fn install_tracked_table(pool: &PgPool, table: &str) {
        sqlx::query(&format!(
            "CREATE TABLE {table} (id UUID PRIMARY KEY, name TEXT)"
        ))
        .execute(pool)
        .await
        .unwrap();
        sqlx::query(&format!(
            "CREATE TRIGGER {table}_notify
             AFTER INSERT OR UPDATE OR DELETE ON {table}
             FOR EACH ROW EXECUTE FUNCTION forge_notify_change()"
        ))
        .execute(pool)
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn drain_returns_rows_after_high_water_mark() {
        let db = setup_db("change_log_drain").await;
        install_tracked_table(db.pool(), "drain_items").await;

        // Seed three rows so seq advances.
        for _ in 0..3 {
            sqlx::query("INSERT INTO drain_items (id, name) VALUES (gen_random_uuid(), 'x')")
                .execute(db.pool())
                .await
                .unwrap();
        }
        // Capture the seq of the first insert; we'll drain after it.
        let first_seq: i64 = sqlx::query_scalar(
            "SELECT MIN(seq) FROM forge_change_log WHERE table_name='drain_items'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();

        let rows: Vec<ChangeRow> = drain_change_log(db.pool(), first_seq)
            .try_collect()
            .await
            .unwrap();
        assert_eq!(rows.len(), 2);
        assert!(rows.iter().all(|r| r.seq > first_seq));
        assert!(rows.iter().all(|r| r.table_name == "drain_items"));
        assert!(rows.iter().all(|r| r.op == "INSERT"));
        // seq monotonic
        assert!(rows[0].seq < rows[1].seq);
    }

    #[tokio::test]
    async fn trim_deletes_only_rows_older_than_cutoff() {
        let db = setup_db("change_log_trim").await;
        install_tracked_table(db.pool(), "trim_items").await;

        sqlx::query("INSERT INTO trim_items (id, name) VALUES (gen_random_uuid(), 'old')")
            .execute(db.pool())
            .await
            .unwrap();
        // Forge a future cutoff so the row qualifies as "old".
        let cutoff = Utc::now() + chrono::Duration::seconds(10);
        let deleted = trim_change_log(db.pool(), cutoff).await.unwrap();
        assert_eq!(deleted, 1);
        let remaining: i64 = sqlx::query_scalar(
            "SELECT COUNT(*) FROM forge_change_log WHERE table_name='trim_items'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(remaining, 0);
    }

    #[tokio::test]
    async fn min_seq_is_none_on_empty_log() {
        let db = setup_db("change_log_min_empty").await;
        // Trim everything the migrations may have produced (none expected, but
        // be explicit so this test is hermetic).
        sqlx::query("TRUNCATE forge_change_log")
            .execute(db.pool())
            .await
            .unwrap();
        assert_eq!(min_seq(db.pool()).await.unwrap(), None);
    }

    #[tokio::test]
    async fn min_seq_reports_oldest_after_writes() {
        let db = setup_db("change_log_min_after").await;
        install_tracked_table(db.pool(), "min_items").await;
        sqlx::query("TRUNCATE forge_change_log")
            .execute(db.pool())
            .await
            .unwrap();

        sqlx::query("INSERT INTO min_items (id, name) VALUES (gen_random_uuid(), 'a')")
            .execute(db.pool())
            .await
            .unwrap();
        sqlx::query("INSERT INTO min_items (id, name) VALUES (gen_random_uuid(), 'b')")
            .execute(db.pool())
            .await
            .unwrap();

        let min = min_seq(db.pool()).await.unwrap();
        let actual: i64 = sqlx::query_scalar("SELECT MIN(seq) FROM forge_change_log")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(min, Some(actual));
    }
}

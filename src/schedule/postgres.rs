//! Postgres `schedule` backend + ticker. Contract: docs/contracts/schedule.md.
//!
//! `cron`/`at` register rows in `forge_schedules`. The ticker ([`PgSchedule::process_due`],
//! driven by `forge.run_scheduler()`) claims due rows with `FOR UPDATE SKIP LOCKED` and,
//! in the SAME transaction, inserts a job into `forge_jobs` and advances/deletes the
//! schedule row — so a tick enqueues exactly once across all replicas (the row claim
//! is the synchronization point) and never loses a tick to a crash between the two.

use super::cron::Cron;
use super::{
    MAX_AT_HORIZON_DAYS, MAX_NAME_BYTES, Schedule, ScheduleInfo, ScheduleKind, ScheduleOpts,
};
use crate::error::{ForgeError, Result};
use crate::obs;
use crate::queue::{JobId, MAX_PAYLOAD_BYTES};
use crate::types::Cursor;
use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::time::SystemTime;
use tracing::field::Empty;
use uuid::Uuid;

/// A tick more than this far late (e.g. all replicas were down) fires once on
/// recovery only if within the window, else is skipped + logged (k8s
/// `startingDeadlineSeconds`).
const MISSED_TICK_GRACE_SECS: f64 = 60.0 * 60.0;
/// Most schedules a single tick fires at once.
const TICK_BATCH: i64 = 1000;

/// Postgres-backed [`Schedule`].
pub(crate) struct PgSchedule {
    pool: PgPool,
    /// App namespace: scopes the (name, app) schedule key and is mixed into the
    /// stored target-queue name so a scheduled enqueue lands in this app's queue.
    /// Empty = the unnamespaced app.
    app: String,
}

impl PgSchedule {
    pub(crate) fn new(pool: PgPool, app: String) -> Self {
        Self { pool, app }
    }

    /// The stored (namespaced) target-queue name, matching `PgQueue`'s prefixing.
    fn physical_queue(&self, queue: &str) -> String {
        if self.app.is_empty() {
            queue.to_string()
        } else {
            format!("{}:{}", self.app, queue)
        }
    }

    /// Strip the namespace prefix from a stored target-queue name.
    fn logical_queue(&self, stored: &str) -> String {
        if self.app.is_empty() {
            stored.to_string()
        } else {
            stored
                .strip_prefix(&self.app)
                .and_then(|s| s.strip_prefix(':'))
                .unwrap_or(stored)
                .to_string()
        }
    }

    /// Fire every due schedule once. Returns how many jobs were enqueued. Idempotent
    /// and safe to run concurrently on many replicas (per-row claim).
    // NOTE (P2-11): up to TICK_BATCH due schedules are processed in one transaction.
    // A single failing insert rolls back and retries the whole batch on the next
    // pass. Fine for v1; a per-row savepoint (or a smaller batch) would contain a
    // poison row so one bad schedule can't stall the others. Revisit at scale.
    async fn process_due_inner(&self) -> Result<u64> {
        let span = tracing::info_span!(
            "forge.schedule.tick",
            schedule.fired = Empty,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("schedule", "tick", span, async move {
            let mut tx = self.pool.begin().await?;
            let due = sqlx::query!(
                r#"SELECT name, app, kind, cron_expr, target_queue, payload, job_id,
                          max_attempts,
                          EXTRACT(EPOCH FROM (now() - next_run))::float8 AS "lateness!"
                   FROM forge_schedules
                   WHERE next_run <= now()
                   ORDER BY next_run
                   FOR UPDATE SKIP LOCKED
                   LIMIT $1"#,
                TICK_BATCH,
            )
            .fetch_all(&mut *tx)
            .await?;

            let now = Utc::now();
            let mut fired = 0u64;
            for row in due {
                // For a cron, the grace decision is measured from the MOST-RECENT missed
                // tick, not the oldest stored next_run. Otherwise a fast cron (e.g.
                // `* * * * *`) that fell behind during a long outage is wrongly skipped
                // wholesale, even though its latest tick is only seconds late and the
                // contract promises that one fires. One-shot/`at` rows keep next_run.
                let lateness = if row.kind == "cron" {
                    row.cron_expr
                        .as_deref()
                        .and_then(|e| Cron::parse(e).ok())
                        .and_then(|c| c.prev_or_at(now))
                        .map_or(row.lateness, |prev| (now - prev).num_seconds() as f64)
                } else {
                    row.lateness
                };
                if lateness <= MISSED_TICK_GRACE_SECS {
                    let job_id = row.job_id.unwrap_or_else(Uuid::new_v4);
                    // Unset opts inherit the queue's own enqueue defaults. Retry timing is the
                    // queue's default backoff policy, resolved at delivery time, not persisted.
                    let max_attempts = row.max_attempts.unwrap_or(5);
                    sqlx::query!(
                        "INSERT INTO forge_jobs \
                           (id, queue, payload, status, attempts, max_attempts, available_at) \
                         VALUES ($1, $2, $3, 'available', 0, $4, now())",
                        job_id,
                        row.target_queue,
                        row.payload.as_slice(),
                        max_attempts,
                    )
                    .execute(&mut *tx)
                    .await?;
                    fired += 1;
                } else {
                    tracing::warn!(
                        schedule.name = %row.name,
                        lateness_secs = lateness,
                        "skipping missed schedule tick (past the grace window)"
                    );
                }

                // A cron whose expression no longer parses or never fires again is
                // silently dropped here rather than erroring — same path as a one-shot.
                let next = if row.kind == "cron" {
                    row.cron_expr
                        .as_deref()
                        .and_then(|e| Cron::parse(e).ok())
                        .and_then(|c| c.next_after(Utc::now()))
                } else {
                    None
                };
                match next {
                    Some(n) => {
                        sqlx::query!(
                            "UPDATE forge_schedules SET last_run = now(), next_run = $2 \
                             WHERE name = $1 AND app = $3",
                            row.name,
                            n,
                            row.app,
                        )
                        .execute(&mut *tx)
                        .await?;
                    }
                    None => {
                        sqlx::query!(
                            "DELETE FROM forge_schedules WHERE name = $1 AND app = $2",
                            row.name,
                            row.app,
                        )
                        .execute(&mut *tx)
                        .await?;
                    }
                }
            }
            tx.commit().await?;
            tracing::Span::current().record("schedule.fired", fired);
            Ok(fired)
        })
        .await
    }
}

fn check_name(name: &str) -> Result<()> {
    if name.is_empty() {
        return Err(ForgeError::invalid("schedule name must not be empty"));
    }
    if name.len() > MAX_NAME_BYTES {
        return Err(ForgeError::limit(format!(
            "schedule name is {} bytes; max is {MAX_NAME_BYTES}",
            name.len()
        )));
    }
    Ok(())
}

/// Validate the target queue name (same rules as `queue`, checked here so a bad name
/// fails at registration rather than silently at tick time).
fn check_queue(queue: &str) -> Result<()> {
    if queue.is_empty() {
        return Err(ForgeError::invalid("target queue must not be empty"));
    }
    if !queue
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    {
        return Err(ForgeError::invalid(
            "target queue may only contain [A-Za-z0-9_.-]",
        ));
    }
    if queue.ends_with(".dlq") {
        return Err(ForgeError::invalid(
            "target queue must not end in '.dlq' (reserved)",
        ));
    }
    Ok(())
}

/// The nullable `max_attempts` storage column for [`ScheduleOpts`]. `None` means "inherit
/// the queue default" (resolved at tick time in `process_due`).
fn opt_columns(opts: &ScheduleOpts) -> Option<i32> {
    opts.max_attempts
        .map(|m| i32::try_from(m).unwrap_or(i32::MAX))
}

fn check_payload(payload: &[u8]) -> Result<()> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(ForgeError::limit(format!(
            "payload is {} bytes; max is {MAX_PAYLOAD_BYTES}",
            payload.len()
        )));
    }
    Ok(())
}

#[async_trait]
impl Schedule for PgSchedule {
    async fn cron(
        &self,
        name: &str,
        expr: &str,
        queue: &str,
        payload: Bytes,
        opts: ScheduleOpts,
    ) -> Result<()> {
        let span = tracing::info_span!(
            "forge.schedule.cron",
            schedule.name = %name,
            schedule.queue = %queue,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("schedule", "cron", span, async move {
            check_name(name)?;
            check_queue(queue)?;
            check_payload(&payload)?;
            let next = Cron::parse(expr)?
                .next_after(Utc::now())
                .ok_or_else(|| ForgeError::invalid("cron expression never fires"))?;
            let max_attempts = opt_columns(&opts);
            sqlx::query!(
                "INSERT INTO forge_schedules \
                   (name, kind, cron_expr, target_queue, payload, job_id, next_run, app, \
                    max_attempts) \
                 VALUES ($1, 'cron', $2, $3, $4, NULL, $5, $6, $7) \
                 ON CONFLICT (name, app) DO UPDATE SET \
                   kind = 'cron', cron_expr = EXCLUDED.cron_expr, \
                   target_queue = EXCLUDED.target_queue, payload = EXCLUDED.payload, \
                   job_id = NULL, next_run = EXCLUDED.next_run, last_run = NULL, \
                   max_attempts = EXCLUDED.max_attempts",
                name,
                expr,
                self.physical_queue(queue),
                payload.as_ref(),
                next,
                self.app,
                max_attempts,
            )
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }

    async fn at(
        &self,
        when: SystemTime,
        queue: &str,
        payload: Bytes,
        opts: ScheduleOpts,
    ) -> Result<JobId> {
        let job_id = Uuid::new_v4();
        let name = format!("at:{job_id}");
        let span = tracing::info_span!(
            "forge.schedule.at",
            schedule.name = %name,
            schedule.queue = %queue,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("schedule", "at", span, async move {
            check_queue(queue)?;
            check_payload(&payload)?;
            let when_dt: DateTime<Utc> = when.into();
            // A past/now `when` is allowed (it fires on the next tick within grace);
            // only an absurdly-far-future `when` is rejected, matching the contract's
            // ~100-year ceiling so every backend agrees on the horizon.
            if when_dt > Utc::now() + chrono::Duration::days(MAX_AT_HORIZON_DAYS) {
                return Err(ForgeError::limit("at `when` exceeds the ~100-year ceiling"));
            }
            let max_attempts = opt_columns(&opts);
            sqlx::query!(
                "INSERT INTO forge_schedules \
                   (name, kind, cron_expr, target_queue, payload, job_id, next_run, app, \
                    max_attempts) \
                 VALUES ($1, 'at', NULL, $2, $3, $4, $5, $6, $7)",
                name,
                self.physical_queue(queue),
                payload.as_ref(),
                job_id,
                when_dt,
                self.app,
                max_attempts,
            )
            .execute(&self.pool)
            .await?;
            Ok(JobId(job_id))
        })
        .await
    }

    async fn cancel(&self, name: &str) -> Result<bool> {
        let span = tracing::info_span!(
            "forge.schedule.cancel",
            schedule.name = %name,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("schedule", "cancel", span, async move {
            let removed = sqlx::query_scalar!(
                "DELETE FROM forge_schedules WHERE name = $1 AND app = $2 RETURNING name",
                name,
                self.app,
            )
            .fetch_optional(&self.pool)
            .await?
            .is_some();
            Ok(removed)
        })
        .await
    }

    async fn list(
        &self,
        cursor: Option<Cursor>,
        limit: u32,
    ) -> Result<(Vec<ScheduleInfo>, Option<Cursor>)> {
        let span = tracing::info_span!(
            "forge.schedule.list",
            schedule.count = Empty,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("schedule", "list", span, async move {
            let limit_i = i64::from(limit.clamp(1, 10_000));
            // `name` is the page key (it is unique within an app and the order column),
            // so the cursor token is simply the last name returned.
            let after = cursor.as_ref().map(|c| c.token());
            let rows = sqlx::query!(
                "SELECT name, kind, cron_expr, target_queue, next_run, last_run \
                 FROM forge_schedules \
                 WHERE app = $1 AND ($2::text IS NULL OR name > $2) \
                 ORDER BY name LIMIT $3",
                self.app,
                after,
                limit_i,
            )
            .fetch_all(&self.pool)
            .await?;
            let next = if (rows.len() as i64) < limit_i {
                None
            } else {
                rows.last().map(|r| Cursor::from_token(r.name.clone()))
            };
            let items: Vec<ScheduleInfo> = rows
                .into_iter()
                .map(|r| {
                    let kind = if r.kind == "cron" {
                        ScheduleKind::Cron(r.cron_expr.unwrap_or_default())
                    } else {
                        ScheduleKind::At
                    };
                    ScheduleInfo::new(
                        r.name,
                        kind,
                        self.logical_queue(&r.target_queue),
                        r.next_run.into(),
                        r.last_run.map(Into::into),
                    )
                })
                .collect();
            tracing::Span::current().record("schedule.count", items.len());
            Ok((items, next))
        })
        .await
    }

    async fn process_due(&self) -> Result<u64> {
        self.process_due_inner().await
    }
}

use super::cron::Cron;
use super::{
    MAX_AT_HORIZON_DAYS, MAX_NAME_BYTES, MisfirePolicy, Schedule, ScheduleInfo, ScheduleKind,
    ScheduleOpts, SchedulerDiagnostics, plan_cron_occurrences,
};
use crate::error::{ForgeError, Result};
use crate::obs;
use crate::queue::{EnqueueOpts, JobId, MAX_PAYLOAD_BYTES, Queue};
use crate::types::Cursor;
use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use sha2::{Digest, Sha256};
use sqlx::{PgPool, Row};
use std::sync::Arc;
use std::time::{Duration, SystemTime};
use tracing::field::Empty;
use uuid::Uuid;

/// Most schedules a single tick fires at once.
const TICK_BATCH: i64 = 1000;

/// Postgres-backed [`Schedule`].
pub(crate) struct PgSchedule {
    pool: PgPool,
    /// The Forge instance's resolved queue backend; a due tick enqueues through it.
    queue: Arc<dyn Queue>,
    /// App namespace: scopes the (name, app) schedule key and is mixed into the
    /// stored target-queue name so a scheduled enqueue lands in this app's queue.
    /// Empty = the unnamespaced app.
    app: String,
}

impl PgSchedule {
    pub(crate) fn new(pool: PgPool, app: String, queue: Arc<dyn Queue>) -> Self {
        Self { pool, queue, app }
    }

    /// The stored (namespaced) target-queue name, matching `PgQueue`'s prefixing.
    fn physical_queue(&self, queue: &str) -> String {
        if self.app.is_empty() {
            queue.to_string()
        } else {
            format!("{}:{}", self.app, queue)
        }
    }

    /// Stable id for one cron tick, so a retry after "queue enqueue succeeded but
    /// schedule transaction did not commit" is idempotent in built-in queue backends.
    fn tick_job_id(&self, name: &str, next_run: DateTime<Utc>) -> JobId {
        let mut h = Sha256::new();
        h.update(b"forge:schedule:tick:v1");
        h.update([0]);
        h.update(self.app.as_bytes());
        h.update([0]);
        h.update(name.as_bytes());
        h.update([0]);
        let ts = next_run
            .timestamp_nanos_opt()
            .map_or_else(|| next_run.to_rfc3339(), |n| n.to_string());
        h.update(ts.as_bytes());

        let digest = h.finalize();
        let mut bytes = [0u8; 16];
        for (dst, src) in bytes.iter_mut().zip(digest.iter()) {
            *dst = *src;
        }
        // Mark as a custom-version UUID with the RFC 4122 variant bits set.
        if let Some(b) = bytes.get_mut(6) {
            *b = (*b & 0x0f) | 0x80;
        }
        if let Some(b) = bytes.get_mut(8) {
            *b = (*b & 0x3f) | 0x80;
        }
        JobId(Uuid::from_bytes(bytes))
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

    // This best-effort write runs only after the scheduler transaction has been
    // rolled back, so it cannot use that transaction's statically checked query.
    #[allow(clippy::disallowed_methods)]
    async fn record_enqueue_failure(&self) {
        let _ = sqlx::query(
            "INSERT INTO forge_scheduler_state (app, enqueue_failures) VALUES ($1, 1) \
             ON CONFLICT (app) DO UPDATE SET enqueue_failures = \
             forge_scheduler_state.enqueue_failures + 1",
        )
        .bind(&self.app)
        .execute(&self.pool)
        .await;
    }

    /// Fire every due schedule once. Returns how many jobs were enqueued. Idempotent
    /// and safe to run concurrently on many replicas (per-row claim).
    // Up to TICK_BATCH due schedules are processed in one transaction. A single
    // failing enqueue rolls back schedule advancement and retries the whole batch
    // on the next pass; per-row savepoints would isolate a poison schedule.
    // Runtime SQL until offline sqlx metadata is regenerated for namespace-scoped ticks.
    #[allow(clippy::disallowed_methods)]
    async fn process_due_inner(&self) -> Result<u64> {
        let span = tracing::info_span!(
            "forge.schedule.tick",
            schedule.fired = Empty,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("schedule", "tick", span, async move {
            let mut tx = self.pool.begin().await?;
            let due = sqlx::query(
                r#"SELECT name, app, kind, cron_expr, target_queue, payload, job_id,
                          next_run, max_attempts, misfire_policy, max_catch_up
                   FROM forge_schedules
                   WHERE next_run <= now() AND app = $2 AND paused = FALSE
                   ORDER BY next_run
                   FOR UPDATE SKIP LOCKED
                   LIMIT $1"#,
            )
            .bind(TICK_BATCH)
            .bind(&self.app)
            .fetch_all(&mut *tx)
            .await?;

            let now = Utc::now();
            let mut fired = 0u64;
            for row in due {
                let name: String = row.try_get("name")?;
                let app: String = row.try_get("app")?;
                let kind: String = row.try_get("kind")?;
                let cron_expr: Option<String> = row.try_get("cron_expr")?;
                let target_queue: String = row.try_get("target_queue")?;
                let payload: Vec<u8> = row.try_get("payload")?;
                let job_id: Option<Uuid> = row.try_get("job_id")?;
                let next_run: DateTime<Utc> = row.try_get("next_run")?;
                let max_attempts: Option<i32> = row.try_get("max_attempts")?;
                let policy_name: String = row.try_get("misfire_policy")?;
                let max_catch_up: i32 = row.try_get("max_catch_up")?;
                let policy = policy_from_columns(&policy_name, max_catch_up)?;
                let (occurrences, next) = if kind == "cron" {
                    cron_expr.as_deref().and_then(|expr| Cron::parse(expr).ok()).map_or_else(
                        || (Vec::new(), None),
                        |cron| plan_cron_occurrences(&cron, next_run, now, policy),
                    )
                } else {
                    let occurrences = if policy == MisfirePolicy::Skip {
                        Vec::new()
                    } else {
                        vec![next_run]
                    };
                    (occurrences, None)
                };
                for occurrence in &occurrences {
                    let occurrence_job_id = job_id
                        .map(JobId)
                        .unwrap_or_else(|| self.tick_job_id(&name, *occurrence));
                    let mut opts = EnqueueOpts::new().with_job_id(occurrence_job_id);
                    if let Some(m) = max_attempts {
                        opts = opts.with_max_attempts(u32::try_from(m).unwrap_or(0));
                    }
                    if let Err(error) = self
                        .queue
                        .enqueue(
                            &self.logical_queue(&target_queue),
                            Bytes::from(payload.clone()),
                            opts,
                        )
                        .await
                    {
                        let _ = tx.rollback().await;
                        self.record_enqueue_failure().await;
                        return Err(error);
                    }
                    fired += 1;
                }

                match next {
                    Some(n) => {
                        sqlx::query(
                            "UPDATE forge_schedules SET last_run = COALESCE($4, last_run), \
                             next_run = $2 WHERE name = $1 AND app = $3",
                        )
                        .bind(&name)
                        .bind(n)
                        .bind(&app)
                        .bind(occurrences.last().copied())
                        .execute(&mut *tx)
                        .await?;
                    }
                    None => {
                        sqlx::query!(
                            "DELETE FROM forge_schedules WHERE name = $1 AND app = $2",
                            name,
                            app,
                        )
                        .execute(&mut *tx)
                        .await?;
                    }
                }
            }
            sqlx::query(
                "INSERT INTO forge_scheduler_state (app, last_successful_tick) VALUES ($1, $2) \
                 ON CONFLICT (app) DO UPDATE SET last_successful_tick = EXCLUDED.last_successful_tick",
            )
            .bind(&self.app)
            .bind(now)
            .execute(&mut *tx)
            .await?;
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

fn policy_columns(opts: &ScheduleOpts) -> Result<(&'static str, i32)> {
    let policy = opts.misfire_policy.validate()?;
    Ok((
        policy.name(),
        i32::try_from(policy.max_catch_up()).unwrap_or(i32::MAX),
    ))
}

fn policy_from_columns(name: &str, max_catch_up: i32) -> Result<MisfirePolicy> {
    match name {
        "skip" => Ok(MisfirePolicy::Skip),
        "run_once" => Ok(MisfirePolicy::RunOnce),
        "catch_up" => {
            MisfirePolicy::CatchUp(u32::try_from(max_catch_up).unwrap_or(u32::MAX)).validate()
        }
        _ => Err(ForgeError::backend(
            "invalid persisted scheduler misfire policy",
        )),
    }
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
            let (misfire_policy, max_catch_up) = policy_columns(&opts)?;
            sqlx::query!(
                "INSERT INTO forge_schedules \
                   (name, kind, cron_expr, target_queue, payload, job_id, next_run, app, \
                    max_attempts, misfire_policy, max_catch_up) \
                 VALUES ($1, 'cron', $2, $3, $4, NULL, $5, $6, $7, $8, $9) \
                 ON CONFLICT (name, app) DO UPDATE SET \
                   kind = 'cron', cron_expr = EXCLUDED.cron_expr, \
                   target_queue = EXCLUDED.target_queue, payload = EXCLUDED.payload, \
                   job_id = NULL, next_run = EXCLUDED.next_run, last_run = NULL, \
                   max_attempts = EXCLUDED.max_attempts, \
                   misfire_policy = EXCLUDED.misfire_policy, \
                   max_catch_up = EXCLUDED.max_catch_up",
                name,
                expr,
                self.physical_queue(queue),
                payload.as_ref(),
                next,
                self.app,
                max_attempts,
                misfire_policy,
                max_catch_up,
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
            // A past/now `when` is allowed and its misfire policy handles the next tick;
            // only an absurdly-far-future `when` is rejected, matching the contract's
            // ~100-year ceiling so every backend agrees on the horizon.
            if when_dt > Utc::now() + chrono::Duration::days(MAX_AT_HORIZON_DAYS) {
                return Err(ForgeError::limit("at `when` exceeds the ~100-year ceiling"));
            }
            let max_attempts = opt_columns(&opts);
            let (misfire_policy, max_catch_up) = policy_columns(&opts)?;
            sqlx::query!(
                "INSERT INTO forge_schedules \
                   (name, kind, cron_expr, target_queue, payload, job_id, next_run, app, \
                    max_attempts, misfire_policy, max_catch_up) \
                 VALUES ($1, 'at', NULL, $2, $3, $4, $5, $6, $7, $8, $9)",
                name,
                self.physical_queue(queue),
                payload.as_ref(),
                job_id,
                when_dt,
                self.app,
                max_attempts,
                misfire_policy,
                max_catch_up,
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

    async fn inspect(&self, name: &str) -> Result<Option<ScheduleInfo>> {
        let row = sqlx::query!(
            "SELECT name, kind, cron_expr, target_queue, next_run, last_run, paused, \
             misfire_policy, max_catch_up FROM forge_schedules WHERE name = $1 AND app = $2",
            name,
            self.app,
        )
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| {
            Ok(ScheduleInfo::new(
                row.name,
                if row.kind == "cron" {
                    ScheduleKind::Cron(row.cron_expr.unwrap_or_default())
                } else {
                    ScheduleKind::At
                },
                self.logical_queue(&row.target_queue),
                row.next_run.into(),
                row.last_run.map(Into::into),
                row.paused,
                policy_from_columns(&row.misfire_policy, row.max_catch_up)?,
            ))
        })
        .transpose()
    }

    async fn pause(&self, name: &str) -> Result<bool> {
        Ok(sqlx::query_scalar!(
            "UPDATE forge_schedules SET paused = TRUE WHERE name = $1 AND app = $2 \
             RETURNING name",
            name,
            self.app,
        )
        .fetch_optional(&self.pool)
        .await?
        .is_some())
    }

    async fn resume(&self, name: &str) -> Result<bool> {
        Ok(sqlx::query_scalar!(
            "UPDATE forge_schedules SET paused = FALSE WHERE name = $1 AND app = $2 \
             RETURNING name",
            name,
            self.app,
        )
        .fetch_optional(&self.pool)
        .await?
        .is_some())
    }

    #[allow(clippy::disallowed_methods)]
    async fn diagnostics(&self) -> Result<SchedulerDiagnostics> {
        let due = sqlx::query(
            "SELECT COUNT(*)::bigint AS due_count, \
             EXTRACT(EPOCH FROM (now() - MIN(next_run)))::float8 AS lag_seconds \
             FROM forge_schedules WHERE app = $1 AND paused = FALSE AND next_run <= now()",
        )
        .bind(&self.app)
        .fetch_one(&self.pool)
        .await?;
        let due_count: i64 = due.try_get("due_count")?;
        let lag_seconds: Option<f64> = due.try_get("lag_seconds")?;
        let state = sqlx::query(
            "SELECT last_successful_tick, enqueue_failures FROM forge_scheduler_state \
             WHERE app = $1",
        )
        .bind(&self.app)
        .fetch_optional(&self.pool)
        .await?;
        let last_successful_tick = state
            .as_ref()
            .map(|row| row.try_get::<Option<DateTime<Utc>>, _>("last_successful_tick"))
            .transpose()?
            .flatten();
        let enqueue_failures = state
            .as_ref()
            .map_or(Ok(0_i64), |row| row.try_get("enqueue_failures"))?;
        Ok(SchedulerDiagnostics {
            lag: lag_seconds.map(|seconds| Duration::from_secs_f64(seconds.max(0.0))),
            last_successful_tick: last_successful_tick.map(Into::into),
            due_count: u64::try_from(due_count).unwrap_or(0),
            enqueue_failures: u64::try_from(enqueue_failures).unwrap_or(0),
        })
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
                "SELECT name, kind, cron_expr, target_queue, next_run, last_run, paused, \
                 misfire_policy, max_catch_up \
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
            let items: Result<Vec<ScheduleInfo>> = rows
                .into_iter()
                .map(|r| {
                    let kind = if r.kind == "cron" {
                        ScheduleKind::Cron(r.cron_expr.unwrap_or_default())
                    } else {
                        ScheduleKind::At
                    };
                    Ok(ScheduleInfo::new(
                        r.name,
                        kind,
                        self.logical_queue(&r.target_queue),
                        r.next_run.into(),
                        r.last_run.map(Into::into),
                        r.paused,
                        policy_from_columns(&r.misfire_policy, r.max_catch_up)?,
                    ))
                })
                .collect();
            let items = items?;
            tracing::Span::current().record("schedule.count", items.len());
            Ok((items, next))
        })
        .await
    }

    async fn process_due(&self) -> Result<u64> {
        self.process_due_inner().await
    }
}

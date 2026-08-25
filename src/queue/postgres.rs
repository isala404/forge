use super::{
    Backoff, DeadLetterInfo, DeadLetterPage, DequeueOpts, EnqueueOpts, Job, JobId, JobState,
    JobStatus, JobStatusFilter, JobStatusPage, MAX_CONCURRENCY_KEY_BYTES, MAX_OPERATOR_BATCH,
    MAX_PAYLOAD_BYTES, MAX_VISIBILITY_TIMEOUT, MAX_WAIT, NackOpts, Priority, Queue, QueueDepth,
    QueueStats, RedriveBatchResult, RedriveDedupPolicy, RedriveOpts, TerminalRetention,
    safe_failure_summary,
};
use crate::error::{ForgeError, Result};
use crate::obs;
use async_trait::async_trait;
use bytes::Bytes;
use sqlx::{PgPool, Row};
use std::time::{Duration, SystemTime};
use tracing::field::Empty;
use uuid::Uuid;

/// Longest a `dedup_id` may be (SQS limit). Over => `Limit`.
const MAX_DEDUP_ID_LEN: usize = 128;
/// Queue name length cap (matches the schedule name cap). Without it a 1 MB name
/// is accepted and grows by 4 bytes per `.dlq` hop.
const MAX_QUEUE_NAME_BYTES: usize = 256;
/// SQS `DelaySeconds` ceiling (15 min). Out of range => `Invalid`.
const MAX_DELAY: Duration = Duration::from_secs(15 * 60);
/// SQS `maxReceiveCount` ceiling.
const MAX_MAX_ATTEMPTS: u32 = 1000;
/// Poll cadence while long-polling a `dequeue` (LISTEN/NOTIFY is a later optimization).
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Postgres-backed [`Queue`].
pub(crate) struct PgQueue {
    pool: PgPool,
    dedup_window: Duration,
    payload_retention: Duration,
    terminal_retention: TerminalRetention,
    /// Namespace prefix on queue names (`<ns>:<queue>`), so apps sharing a database
    /// don't cross-consume each other's queues. Empty = no prefix.
    namespace: String,
}

impl PgQueue {
    pub(crate) fn new(
        pool: PgPool,
        dedup_window: Duration,
        payload_retention: Duration,
        terminal_retention: TerminalRetention,
        namespace: String,
    ) -> Self {
        Self {
            pool,
            dedup_window,
            payload_retention,
            terminal_retention,
            namespace,
        }
    }

    /// Stored queue name for a caller name, applying the namespace prefix.
    fn physical(&self, queue: &str) -> String {
        crate::util::namespaced(&self.namespace, queue)
    }

    /// Strip the namespace prefix from a stored queue name (for the returned `Job`).
    fn logical(&self, stored: &str) -> String {
        if self.namespace.is_empty() {
            stored.to_string()
        } else {
            stored
                .strip_prefix(&self.namespace)
                .and_then(|s| s.strip_prefix(':'))
                .unwrap_or(stored)
                .to_string()
        }
    }

    /// Reclaim expired leases in `queue`: exhausted jobs re-home to the DLQ, the
    /// rest return to `available` immediately with attempts bumped. Idempotent.
    // Runtime SQL until offline sqlx metadata is regenerated for the DLQ reclaim updates.
    #[allow(clippy::disallowed_methods)]
    async fn reclaim(&self, queue: &str) -> Result<()> {
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE forge_jobs SET status = 'cancelled', completed_at = now(), \
             lease_token = NULL, leased_until = NULL, lease_secs = NULL \
             WHERE queue = $1 AND status = 'leased' AND cancel_requested_at IS NOT NULL \
               AND leased_until <= now()",
        )
        .bind(queue)
        .execute(&mut *tx)
        .await?;
        // Exhausted jobs already in a DLQ are terminal; never chain into `.dlq.dlq`.
        let terminal: Vec<Uuid> = sqlx::query_scalar(
            "UPDATE forge_jobs \
             SET status = 'dead', attempts = attempts + 1, \
                 dead_attempts = dead_attempts + attempts + 1, \
                 failure_summary = 'visibility timeout expired', \
                 dead_lettered_at = COALESCE(dead_lettered_at, now()), completed_at = now(), \
                 lease_token = NULL, leased_until = NULL, lease_secs = NULL \
             WHERE queue = $1 AND queue LIKE '%.dlq' AND status = 'leased' \
               AND cancel_requested_at IS NULL \
               AND leased_until <= now() AND attempts + 1 >= max_attempts \
             RETURNING id",
        )
        .bind(queue)
        .fetch_all(&mut *tx)
        .await?;

        // Exhausted non-DLQ jobs re-home first: changing `queue` keeps the normal
        // retry statement below from touching them in the same sweep.
        let dead_lettered: Vec<Uuid> = sqlx::query_scalar(
            "UPDATE forge_jobs \
             SET queue = queue || '.dlq', status = 'available', attempts = 0, \
                 dead_attempts = attempts + 1, failure_summary = 'visibility timeout expired', \
                 dead_lettered_at = now(), \
                 available_at = now(), lease_token = NULL, leased_until = NULL, lease_secs = NULL \
             WHERE queue = $1 AND status = 'leased' AND cancel_requested_at IS NULL AND leased_until <= now() \
               AND attempts + 1 >= max_attempts AND queue NOT LIKE '%.dlq' \
             RETURNING id",
        )
        .bind(queue)
        .fetch_all(&mut *tx)
        .await?;

        let mut released = terminal;
        released.extend(dead_lettered);
        if !released.is_empty() {
            sqlx::query("DELETE FROM forge_job_dedup WHERE job_id = ANY($1)")
                .bind(&released)
                .execute(&mut *tx)
                .await?;
        }

        sqlx::query(
            "UPDATE forge_jobs \
             SET status = 'available', attempts = attempts + 1, \
                 available_at = now(), lease_token = NULL, leased_until = NULL, lease_secs = NULL \
             WHERE queue = $1 AND status = 'leased' AND cancel_requested_at IS NULL AND leased_until <= now()"
        )
        .bind(queue)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Try to claim and lease exactly one due job. `None` if none are ready.
    // The propagation columns are decoded into the validated TraceContext type below;
    // the dynamic row keeps absent nullable headers distinct without SQLx overrides.
    #[allow(clippy::disallowed_methods)]
    async fn try_claim(
        &self,
        queue: &str,
        vis_secs: f64,
        concurrency_limit: Option<u32>,
    ) -> Result<Option<Job>> {
        let row = sqlx::query(
            r#"WITH claimed AS (
                   SELECT id FROM forge_jobs
                   WHERE queue = $1 AND status = 'available' AND available_at <= now()
                     AND NOT EXISTS (
                         SELECT 1 FROM forge_queue_controls controls
                         WHERE controls.queue = $1 AND controls.paused
                     )
                     AND ($3::bigint IS NULL OR concurrency_key IS NULL OR
                          (SELECT count(*) FROM forge_jobs leased
                           WHERE leased.queue = forge_jobs.queue
                             AND leased.status = 'leased' AND leased.leased_until > now()
                             AND leased.cancel_requested_at IS NULL
                             AND leased.concurrency_key = forge_jobs.concurrency_key) < $3)
                   ORDER BY priority DESC, available_at, enqueued_at, id
                   FOR UPDATE SKIP LOCKED
                   LIMIT 1
               )
               UPDATE forge_jobs j
               SET status = 'leased',
                   leased_until = now() + make_interval(secs => $2),
                   lease_token = gen_random_uuid(),
                   lease_secs = $2
               FROM claimed
               WHERE j.id = claimed.id
               RETURNING j.id, j.queue, j.payload, j.attempts, j.max_attempts,
                         j.leased_until, j.lease_token, j.traceparent, j.tracestate, j.baggage,
                         j.priority, j.concurrency_key"#,
        )
        .bind(queue)
        .bind(vis_secs)
        .bind(concurrency_limit.map(i64::from))
        .fetch_optional(&self.pool)
        .await?;

        row.map(|r| -> Result<Job> {
            let traceparent: Option<String> = r.try_get("traceparent")?;
            let baggage: Option<String> = r.try_get("baggage")?;
            let baggage_keys = stored_baggage_keys(baggage.as_deref());
            let trace_context = traceparent
                .map(|traceparent| {
                    crate::TraceContext::from_headers(
                        traceparent,
                        r.try_get("tracestate")?,
                        baggage,
                        &baggage_keys,
                    )
                })
                .transpose()?;
            Ok(Job::new(
                JobId(r.try_get("id")?),
                r.try_get("queue")?,
                Bytes::from(r.try_get::<Vec<u8>, _>("payload")?),
                // attempts counts FAILED deliveries; this delivery is attempts + 1.
                u32::try_from(r.try_get::<i32, _>("attempts")?)
                    .unwrap_or(0)
                    .saturating_add(1),
                u32::try_from(r.try_get::<i32, _>("max_attempts")?).unwrap_or(0),
                to_system_time(r.try_get("leased_until")?),
                r.try_get("lease_token")?,
            )
            .with_trace_context(trace_context)
            .with_scheduling(
                Priority::from_rank(r.try_get("priority")?),
                r.try_get("concurrency_key")?,
            ))
        })
        .transpose()
    }

    /// Maintenance sweep: purge old `done` jobs, reclaim expired leases across
    /// all queues, drop stale dedup entries. Idempotent.
    // Runtime SQL until offline sqlx metadata is regenerated for the DLQ reclaim updates.
    #[allow(clippy::disallowed_methods)]
    pub(crate) async fn maintenance(&self) -> Result<()> {
        let payload_retention_secs = self.payload_retention.as_secs_f64();
        let succeeded_retention_secs = self.terminal_retention.succeeded.as_secs_f64();
        let dead_retention_secs = self.terminal_retention.dead.as_secs_f64();
        let cancelled_retention_secs = self.terminal_retention.cancelled.as_secs_f64();
        let prefix = (!self.namespace.is_empty()).then(|| format!("{}:", self.namespace));
        let mut tx = self.pool.begin().await?;
        sqlx::query(
            "UPDATE forge_jobs SET payload = ''::bytea, payload_retained = false \
             WHERE status IN ('done', 'dead', 'cancelled') \
             AND completed_at <= now() - make_interval(secs => $1) \
             AND payload_retained \
             AND ($2::text IS NULL OR left(queue, length($2)) = $2)",
        )
        .bind(payload_retention_secs)
        .bind(&prefix)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "DELETE FROM forge_jobs WHERE \
             ((status = 'done' AND completed_at <= now() - make_interval(secs => $1)) OR \
              (status = 'dead' AND completed_at <= now() - make_interval(secs => $2)) OR \
              (status = 'cancelled' AND completed_at <= now() - make_interval(secs => $3))) \
             AND ($4::text IS NULL OR left(queue, length($4)) = $4)",
        )
        .bind(succeeded_retention_secs)
        .bind(dead_retention_secs)
        .bind(cancelled_retention_secs)
        .bind(&prefix)
        .execute(&mut *tx)
        .await?;
        sqlx::query(
            "UPDATE forge_jobs SET status = 'cancelled', completed_at = now(), \
             lease_token = NULL, leased_until = NULL, lease_secs = NULL \
             WHERE status = 'leased' AND cancel_requested_at IS NOT NULL \
               AND leased_until <= now() \
               AND ($1::text IS NULL OR left(queue, length($1)) = $1)",
        )
        .bind(&prefix)
        .execute(&mut *tx)
        .await?;

        let terminal: Vec<Uuid> = sqlx::query_scalar(
            "UPDATE forge_jobs \
             SET status = 'dead', attempts = attempts + 1, \
                 dead_attempts = dead_attempts + attempts + 1, \
                 failure_summary = 'visibility timeout expired', \
                 dead_lettered_at = COALESCE(dead_lettered_at, now()), completed_at = now(), \
                 lease_token = NULL, leased_until = NULL, lease_secs = NULL \
             WHERE status = 'leased' AND cancel_requested_at IS NULL AND leased_until <= now() \
               AND attempts + 1 >= max_attempts AND queue LIKE '%.dlq' \
               AND ($1::text IS NULL OR left(queue, length($1)) = $1) \
             RETURNING id",
        )
        .bind(&prefix)
        .fetch_all(&mut *tx)
        .await?;
        let dead_lettered: Vec<Uuid> = sqlx::query_scalar(
            "UPDATE forge_jobs \
             SET queue = queue || '.dlq', status = 'available', attempts = 0, \
                 dead_attempts = attempts + 1, failure_summary = 'visibility timeout expired', \
                 dead_lettered_at = now(), \
                 available_at = now(), lease_token = NULL, leased_until = NULL, lease_secs = NULL \
             WHERE status = 'leased' AND cancel_requested_at IS NULL AND leased_until <= now() \
               AND attempts + 1 >= max_attempts AND queue NOT LIKE '%.dlq' \
               AND ($1::text IS NULL OR left(queue, length($1)) = $1) \
             RETURNING id",
        )
        .bind(&prefix)
        .fetch_all(&mut *tx)
        .await?;
        let mut released = terminal;
        released.extend(dead_lettered);
        if !released.is_empty() {
            sqlx::query("DELETE FROM forge_job_dedup WHERE job_id = ANY($1)")
                .bind(&released)
                .execute(&mut *tx)
                .await?;
        }
        sqlx::query(
            "UPDATE forge_jobs \
             SET status = 'available', attempts = attempts + 1, \
                 available_at = now(), lease_token = NULL, leased_until = NULL, lease_secs = NULL \
             WHERE status = 'leased' AND cancel_requested_at IS NULL AND leased_until <= now() \
             AND ($1::text IS NULL OR left(queue, length($1)) = $1)",
        )
        .bind(&prefix)
        .execute(&mut *tx)
        .await?;

        sqlx::query!(
            "DELETE FROM forge_job_dedup WHERE expires_at <= now() \
             AND ($1::text IS NULL OR left(queue, length($1)) = $1)",
            prefix,
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    /// Shared `nack`/`heartbeat` failure path when the fenced update matched no
    /// row: NotFound if the id is unknown, else Precondition (lease lost).
    async fn lease_lost_error(&self, id: Uuid, queue: &str) -> ForgeError {
        match sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT 1 FROM forge_jobs WHERE id = $1 AND queue = $2) AS "exists!""#,
            id,
            queue,
        )
        .fetch_one(&self.pool)
        .await
        {
            Ok(true) => ForgeError::precondition("lease lost: another worker owns this job"),
            Ok(false) => ForgeError::NotFound,
            Err(e) => e.into(),
        }
    }

    fn status_from_row(&self, row: &sqlx::postgres::PgRow) -> Result<JobStatus> {
        let stored_status: String = row.try_get("status")?;
        let attempts = u32::try_from(row.try_get::<i32, _>("attempts")?).unwrap_or(0);
        let available_at: chrono::DateTime<chrono::Utc> = row.try_get("available_at")?;
        let cancel_requested: Option<chrono::DateTime<chrono::Utc>> =
            row.try_get("cancel_requested_at")?;
        let state = match stored_status.as_str() {
            "available" if attempts > 0 => JobState::Retrying,
            "available" if available_at > chrono::Utc::now() => JobState::Delayed,
            "available" => JobState::Queued,
            "leased" if cancel_requested.is_some() => JobState::CancelRequested,
            "leased" => JobState::Leased,
            "done" => JobState::Succeeded,
            "dead" => JobState::Dead,
            "cancelled" => JobState::Cancelled,
            _ => {
                return Err(ForgeError::backend(
                    "database returned an unknown job status",
                ));
            }
        };
        Ok(JobStatus {
            id: JobId(row.try_get("id")?),
            queue: self.logical(row.try_get::<&str, _>("queue")?),
            state,
            attempt_count: attempts,
            max_attempts: u32::try_from(row.try_get::<i32, _>("max_attempts")?).unwrap_or(0),
            priority: Priority::from_rank(row.try_get("priority")?),
            concurrency_key: row.try_get("concurrency_key")?,
            enqueued_at: to_system_time(row.try_get("enqueued_at")?),
            available_at: to_system_time(available_at),
            completed_at: row
                .try_get::<Option<chrono::DateTime<chrono::Utc>>, _>("completed_at")?
                .map(to_system_time),
        })
    }
}

fn state_label(state: JobState) -> &'static str {
    match state {
        JobState::Queued => "queued",
        JobState::Delayed => "delayed",
        JobState::Leased => "leased",
        JobState::Retrying => "retrying",
        JobState::Succeeded => "succeeded",
        JobState::Dead => "dead",
        JobState::CancelRequested => "cancel_requested",
        JobState::Cancelled => "cancelled",
    }
}

fn stored_baggage_keys(baggage: Option<&str>) -> Vec<String> {
    baggage
        .into_iter()
        .flat_map(|value| value.split(','))
        .filter_map(|item| {
            item.trim()
                .split_once('=')
                .map(|(key, _)| key.trim().to_string())
        })
        .collect()
}

#[async_trait]
impl Queue for PgQueue {
    // Runtime SQL until offline sqlx metadata is regenerated for caller-selected job ids.
    #[allow(clippy::disallowed_methods)]
    async fn enqueue(&self, queue: &str, payload: Bytes, opts: EnqueueOpts) -> Result<JobId> {
        let delay_secs = opts.delay.as_secs_f64();
        let max_attempts = i32::try_from(opts.max_attempts).unwrap_or(i32::MAX);
        let payload_vec = payload.as_ref().to_vec();
        let dedup_window_secs = self.dedup_window.as_secs_f64();
        let requested_id = opts.job_id.map(|id| id.0);
        let traceparent = opts
            .trace_context
            .as_ref()
            .map(|context| context.traceparent().to_string());
        let tracestate = opts
            .trace_context
            .as_ref()
            .and_then(|context| context.tracestate().map(str::to_string));
        let baggage = opts
            .trace_context
            .as_ref()
            .and_then(|context| context.baggage().map(str::to_string));
        let priority = opts.priority.rank();
        let concurrency_key = opts.concurrency_key.clone();
        let span = tracing::info_span!(
            "forge.queue.enqueue",
            queue = %queue,
            payload_bytes = payload.len(),
            dedup_hit = Empty,
            outcome = Empty,
            error.variant = Empty,
        );

        obs::instrument("queue", "enqueue", span, async move {
            check_queue_name(queue, false)?;
            check_payload(&payload_vec)?;
            check_enqueue_opts(&opts)?;
            let queue = self.physical(queue); // namespaced name; bound by the SQL below
            let Some(dedup_id) = opts.dedup_id.clone() else {
                let id = requested_id.unwrap_or_else(Uuid::new_v4);
                let mut tx = self.pool.begin().await?;
                let inserted = sqlx::query_scalar::<_, Uuid>(
                    "INSERT INTO forge_jobs \
                       (id, queue, payload, status, attempts, max_attempts, available_at, \
                        traceparent, tracestate, baggage, priority, concurrency_key) \
                     VALUES ($1, $2, $3, 'available', 0, $4, now() + make_interval(secs => $5), \
                             $6, $7, $8, $9, $10) \
                     ON CONFLICT (id) DO NOTHING \
                     RETURNING id",
                )
                .bind(id)
                .bind(&queue)
                .bind(&payload_vec)
                .bind(max_attempts)
                .bind(delay_secs)
                .bind(&traceparent)
                .bind(&tracestate)
                .bind(&baggage)
                .bind(priority)
                .bind(&concurrency_key)
                .fetch_optional(&mut *tx)
                .await?;
                tracing::Span::current().record("dedup_hit", false);
                if let Some(id) = inserted {
                    sqlx::query(
                        "INSERT INTO forge_queue_counters (queue, enqueued_total) VALUES ($1, 1) \
                         ON CONFLICT (queue) DO UPDATE SET enqueued_total = forge_queue_counters.enqueued_total + 1",
                    )
                    .bind(&queue)
                    .execute(&mut *tx)
                    .await?;
                    tx.commit().await?;
                    return Ok(JobId(id));
                }
                if requested_id.is_some() {
                    let existing_queue = sqlx::query_scalar::<_, String>(
                        "SELECT queue FROM forge_jobs WHERE id = $1",
                    )
                    .bind(id)
                    .fetch_optional(&mut *tx)
                    .await?;
                    if existing_queue.as_deref() == Some(queue.as_str()) {
                        tx.commit().await?;
                        return Ok(JobId(id));
                    }
                    return Err(ForgeError::precondition(
                        "requested job id already exists for another queue",
                    ));
                }
                return Err(ForgeError::backend("generated job id collided"));
            };

            // The upsert always returns the surviving dedup row in one round-trip. When
            // the slot was free or expired, the CASE rewrites job_id to our new_id, so
            // `claimed` is true and we insert the job; a still-live slot keeps its
            // existing job_id, so `claimed` is false and we return it without a second
            // lookup. (new_id is freshly random, so it can't equal an existing live id.)
            let new_id = requested_id.unwrap_or_else(Uuid::new_v4);
            let mut tx = self.pool.begin().await?;
            let row = sqlx::query!(
                r#"INSERT INTO forge_job_dedup (queue, dedup_id, job_id, expires_at)
                   VALUES ($1, $2, $3, now() + make_interval(secs => $4))
                   ON CONFLICT (queue, dedup_id) DO UPDATE
                     SET job_id = CASE WHEN forge_job_dedup.expires_at <= now()
                                       THEN EXCLUDED.job_id ELSE forge_job_dedup.job_id END,
                         expires_at = CASE WHEN forge_job_dedup.expires_at <= now()
                                           THEN EXCLUDED.expires_at ELSE forge_job_dedup.expires_at END
                   RETURNING job_id, (job_id = $3) AS "claimed!""#,
                queue,
                dedup_id,
                new_id,
                dedup_window_secs,
            )
            .fetch_one(&mut *tx)
            .await?;

            if row.claimed {
                let inserted = sqlx::query_scalar::<_, Uuid>(
                    r#"INSERT INTO forge_jobs
                         (id, queue, payload, status, attempts, max_attempts, available_at,
                          traceparent, tracestate, baggage, priority, concurrency_key)
                       VALUES ($1, $2, $3, 'available', 0, $4, now() + make_interval(secs => $5),
                               $6, $7, $8, $9, $10)
                       ON CONFLICT (id) DO NOTHING
                       RETURNING id"#,
                )
                .bind(new_id)
                .bind(&queue)
                .bind(&payload_vec)
                .bind(max_attempts)
                .bind(delay_secs)
                .bind(&traceparent)
                .bind(&tracestate)
                .bind(&baggage)
                .bind(priority)
                .bind(&concurrency_key)
                .fetch_optional(&mut *tx)
                .await?;
                tracing::Span::current().record("dedup_hit", false);
                if inserted.is_some() {
                    sqlx::query(
                        "INSERT INTO forge_queue_counters (queue, enqueued_total) VALUES ($1, 1) \
                         ON CONFLICT (queue) DO UPDATE SET enqueued_total = forge_queue_counters.enqueued_total + 1",
                    )
                    .bind(&queue)
                    .execute(&mut *tx)
                    .await?;
                    tx.commit().await?;
                    return Ok(JobId(new_id));
                }

                if requested_id.is_some() {
                    let existing_queue = sqlx::query_scalar::<_, String>(
                        "SELECT queue FROM forge_jobs WHERE id = $1",
                    )
                    .bind(new_id)
                    .fetch_optional(&mut *tx)
                    .await?;
                    if existing_queue.as_deref() == Some(queue.as_str()) {
                        tx.commit().await?;
                        return Ok(JobId(new_id));
                    }
                    return Err(ForgeError::precondition(
                        "requested job id already exists for another queue",
                    ));
                }
                Err(ForgeError::backend("generated job id collided"))
            } else {
                // Live dedup entry: return the existing job id.
                if requested_id.is_some_and(|requested| requested != row.job_id) {
                    return Err(ForgeError::precondition(
                        "deduplication id is reserved by a different job id",
                    ));
                }
                tx.commit().await?;
                tracing::Span::current().record("dedup_hit", true);
                Ok(JobId(row.job_id))
            }
        })
        .await
    }

    async fn dequeue(&self, queue: &str, opts: DequeueOpts) -> Result<Option<Job>> {
        let vis_secs = opts.visibility_timeout.as_secs_f64();
        let wait = opts.wait.min(MAX_WAIT);
        let span = tracing::info_span!(
            "forge.queue.dequeue",
            queue = %queue,
            wait_ms = wait.as_millis() as i64,
            visibility_timeout_ms = opts.visibility_timeout.as_millis() as i64,
            job.id = Empty,
            job.attempt = Empty,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("queue", "dequeue", span, async move {
            check_queue_name(queue, true)?;
            check_visibility(opts.visibility_timeout)?;
            if opts.concurrency_limit_per_key == Some(0) {
                return Err(ForgeError::invalid(
                    "concurrency_limit_per_key must be positive",
                ));
            }
            let pq = self.physical(queue);
            // Reclaim expired leases once up front so crashed work redelivers.
            self.reclaim(&pq).await?;

            let deadline = tokio::time::Instant::now() + wait;
            loop {
                if let Some(mut job) = self
                    .try_claim(&pq, vis_secs, opts.concurrency_limit_per_key)
                    .await?
                {
                    // Return the caller's (logical) queue name, not the prefixed one.
                    job.queue = self.logical(&job.queue);
                    let s = tracing::Span::current();
                    s.record("job.id", tracing::field::display(job.id));
                    s.record("job.attempt", job.attempt);
                    return Ok(Some(job));
                }
                let now = tokio::time::Instant::now();
                if now >= deadline {
                    return Ok(None);
                }
                let sleep = POLL_INTERVAL.min(deadline - now);
                tokio::time::sleep(sleep).await;
            }
        })
        .await
    }

    #[allow(clippy::disallowed_methods)]
    async fn ack(&self, job: &Job) -> Result<()> {
        let id = job.id.0;
        let token = job.lease_token();
        let queue = self.physical(&job.queue);
        let span = tracing::info_span!(
            "forge.queue.ack",
            queue = %job.queue,
            job.id = %job.id,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("queue", "ack", span, async move {
            let mut tx = self.pool.begin().await?;
            let settled = sqlx::query_scalar::<_, Uuid>(
                "UPDATE forge_jobs SET status = 'done', completed_at = now(), \
                 lease_token = NULL, leased_until = NULL, lease_secs = NULL \
                 WHERE id = $1 AND lease_token = $2 AND queue = $3 AND status = 'leased' \
                   AND cancel_requested_at IS NULL \
                 RETURNING id",
            )
            .bind(id)
            .bind(token)
            .bind(&queue)
            .fetch_optional(&mut *tx)
            .await?;
            if settled.is_none() {
                tx.rollback().await.ok();
                return Err(self.lease_lost_error(id, &queue).await);
            }
            sqlx::query(
                "INSERT INTO forge_queue_counters (queue, settled_total) VALUES ($1, 1) \
                 ON CONFLICT (queue) DO UPDATE SET settled_total = forge_queue_counters.settled_total + 1",
            )
            .bind(&queue)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            Ok(())
        })
        .await
    }

    #[allow(clippy::disallowed_methods)]
    async fn nack(&self, job: &Job, opts: NackOpts) -> Result<()> {
        let id = job.id.0;
        let token = job.lease_token();
        let queue = self.physical(&job.queue);
        let seed = seed_from_id(id);
        // A job already in a *.dlq queue must not re-home into an unwatched .dlq.dlq;
        // exhaustion there is terminal.
        let in_dlq = job.queue.ends_with(".dlq");
        let span = tracing::info_span!(
            "forge.queue.nack",
            queue = %job.queue,
            job.id = %job.id,
            retry_in_ms = Empty,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("queue", "nack", span, async move {
            if opts.retry_in.is_some_and(|d| d > MAX_VISIBILITY_TIMEOUT) {
                return Err(ForgeError::invalid(format!(
                    "retry_in exceeds the maximum of {}s",
                    MAX_VISIBILITY_TIMEOUT.as_secs()
                )));
            }
            let mut tx = self.pool.begin().await?;
            let row = sqlx::query(
                r#"SELECT attempts, max_attempts
                   FROM forge_jobs
                   WHERE id = $1 AND lease_token = $2 AND queue = $3 AND status = 'leased'
                     AND cancel_requested_at IS NULL
                   FOR UPDATE"#,
            )
            .bind(id)
            .bind(token)
            .bind(&queue)
            .fetch_optional(&mut *tx)
            .await?;

            let Some(row) = row else {
                tx.rollback().await.ok();
                return Err(self.lease_lost_error(id, &queue).await);
            };

            let row_attempts: i32 = row.try_get("attempts")?;
            let row_max_attempts: i32 = row.try_get("max_attempts")?;
            let new_attempts = u32::try_from(row_attempts).unwrap_or(0).saturating_add(1);
            let failure_summary = opts
                .failure_summary
                .map(safe_failure_summary)
                .unwrap_or_else(|| "handler failed".to_string());
            if new_attempts >= u32::try_from(row_max_attempts).unwrap_or(u32::MAX) {
                if in_dlq {
                    // Terminal: park as 'dead' with attempts pinned. Nothing re-homes a
                    // dead-letter job into .dlq.dlq; a dead row is observable + queryable.
                    sqlx::query(
                        "UPDATE forge_jobs \
                         SET status = 'dead', attempts = $2, \
                             dead_attempts = dead_attempts + $2, failure_summary = $3, \
                             dead_lettered_at = COALESCE(dead_lettered_at, now()), completed_at = now(), \
                             lease_token = NULL, leased_until = NULL, lease_secs = NULL \
                         WHERE id = $1",
                    )
                    .bind(id)
                    .bind(i32::try_from(new_attempts).unwrap_or(i32::MAX))
                    .bind(&failure_summary)
                    .execute(&mut *tx)
                    .await?;
                    sqlx::query(
                        "INSERT INTO forge_queue_counters (queue, settled_total, dead_total) VALUES ($1, 1, 1) \
                         ON CONFLICT (queue) DO UPDATE SET settled_total = forge_queue_counters.settled_total + 1, dead_total = forge_queue_counters.dead_total + 1",
                    )
                    .bind(&queue)
                    .execute(&mut *tx)
                    .await?;
                    tx.commit().await?;
                    tracing::Span::current().record("outcome", "dead");
                    return Ok(());
                }
                // Exhausted: re-home to DLQ with attempts reset so DLQ consumers see a clean slate.
                sqlx::query(
                    "UPDATE forge_jobs \
                     SET queue = queue || '.dlq', status = 'available', attempts = 0, \
                         dead_attempts = $2, failure_summary = $3, dead_lettered_at = now(), \
                         available_at = now(), lease_token = NULL, leased_until = NULL, lease_secs = NULL \
                     WHERE id = $1",
                )
                .bind(id)
                .bind(i32::try_from(new_attempts).unwrap_or(i32::MAX))
                .bind(&failure_summary)
                .execute(&mut *tx)
                .await?;
                sqlx::query(
                    "INSERT INTO forge_queue_counters (queue, settled_total, dead_total) VALUES ($1, 1, 1) \
                     ON CONFLICT (queue) DO UPDATE SET settled_total = forge_queue_counters.settled_total + 1, dead_total = forge_queue_counters.dead_total + 1",
                )
                .bind(&queue)
                .execute(&mut *tx)
                .await?;
                sqlx::query("DELETE FROM forge_job_dedup WHERE job_id = $1")
                    .bind(id)
                    .execute(&mut *tx)
                    .await?;
                tx.commit().await?;
                tracing::Span::current().record("outcome", "dead_letter");
                return Ok(());
            }

            let delay = opts
                .retry_in
                .unwrap_or_else(|| Backoff::default().delay_for_attempt(new_attempts, seed));
            let delay_secs = delay.as_secs_f64();
            sqlx::query(
                "UPDATE forge_jobs \
                 SET status = 'available', attempts = $2, \
                     failure_summary = $4, \
                     available_at = now() + make_interval(secs => $3), \
                     lease_token = NULL, leased_until = NULL, lease_secs = NULL \
                 WHERE id = $1",
            )
            .bind(id)
            .bind(i32::try_from(new_attempts).unwrap_or(i32::MAX))
            .bind(delay_secs)
            .bind(&failure_summary)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            tracing::Span::current().record("retry_in_ms", delay.as_millis() as i64);
            Ok(())
        })
        .await
    }

    #[allow(clippy::disallowed_methods)]
    async fn heartbeat(&self, job: &Job) -> Result<()> {
        let id = job.id.0;
        let token = job.lease_token();
        let queue = self.physical(&job.queue);
        let span = tracing::info_span!(
            "forge.queue.heartbeat",
            queue = %job.queue,
            job.id = %job.id,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("queue", "heartbeat", span, async move {
            let extended = sqlx::query_scalar::<_, Uuid>(
                "UPDATE forge_jobs SET leased_until = now() + make_interval(secs => lease_secs) \
                 WHERE id = $1 AND lease_token = $2 AND queue = $3 AND status = 'leased' \
                   AND cancel_requested_at IS NULL \
                 RETURNING id",
            )
            .bind(id)
            .bind(token)
            .bind(&queue)
            .fetch_optional(&self.pool)
            .await?;
            if extended.is_none() {
                return Err(self.lease_lost_error(id, &queue).await);
            }
            Ok(())
        })
        .await
    }

    #[allow(clippy::disallowed_methods)]
    async fn cancellation_requested(&self, job: &Job) -> Result<bool> {
        let row = sqlx::query(
            "SELECT status, cancel_requested_at IS NOT NULL AS requested FROM forge_jobs \
             WHERE id = $1 AND queue = $2",
        )
        .bind(job.id.0)
        .bind(self.physical(&job.queue))
        .fetch_optional(&self.pool)
        .await?;
        let Some(row) = row else {
            return Err(ForgeError::NotFound);
        };
        let requested: bool = row.try_get("requested")?;
        let status: String = row.try_get("status")?;
        let requested = requested || status == "cancelled";
        if requested {
            job.cancellation.signal();
        }
        Ok(requested)
    }

    #[allow(clippy::disallowed_methods)]
    async fn cancel(&self, id: JobId) -> Result<Option<JobStatus>> {
        let prefix = (!self.namespace.is_empty()).then(|| format!("{}:", self.namespace));
        let mut tx = self.pool.begin().await?;
        let prior = sqlx::query(
            "SELECT status, queue FROM forge_jobs WHERE id = $1 \
             AND ($2::text IS NULL OR left(queue, length($2)) = $2) FOR UPDATE",
        )
        .bind(id.0)
        .bind(&prefix)
        .fetch_optional(&mut *tx)
        .await?;
        let Some(prior) = prior else {
            tx.commit().await?;
            return Ok(None);
        };
        let prior_status: String = prior.try_get("status")?;
        let prior_queue: String = prior.try_get("queue")?;
        let row = sqlx::query(
            "UPDATE forge_jobs SET \
               status = CASE WHEN status = 'available' THEN 'cancelled' ELSE status END, \
               cancel_requested_at = CASE WHEN status = 'leased' THEN COALESCE(cancel_requested_at, now()) ELSE cancel_requested_at END, \
               completed_at = CASE WHEN status = 'available' THEN now() ELSE completed_at END, \
               lease_token = CASE WHEN status = 'available' THEN NULL ELSE lease_token END, \
               leased_until = CASE WHEN status = 'available' THEN NULL ELSE leased_until END, \
               lease_secs = CASE WHEN status = 'available' THEN NULL ELSE lease_secs END \
             WHERE id = $1 AND ($2::text IS NULL OR left(queue, length($2)) = $2) \
             RETURNING id, queue, status, attempts, max_attempts, priority, concurrency_key, \
                       enqueued_at, available_at, completed_at, cancel_requested_at",
        ).bind(id.0).bind(&prefix).fetch_optional(&mut *tx).await?;
        let status = row.map(|row| self.status_from_row(&row)).transpose()?;
        if prior_status == "available" {
            sqlx::query(
                "INSERT INTO forge_queue_counters (queue, settled_total, cancelled_total) VALUES ($1, 1, 1) \
                 ON CONFLICT (queue) DO UPDATE SET settled_total = forge_queue_counters.settled_total + 1, cancelled_total = forge_queue_counters.cancelled_total + 1",
            )
            .bind(&prior_queue)
            .execute(&mut *tx)
            .await?;
        }
        if status.is_some() {
            sqlx::query("DELETE FROM forge_job_dedup WHERE job_id = $1")
                .bind(id.0)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(status)
    }

    #[allow(clippy::disallowed_methods)]
    async fn finish_cancellation(&self, job: &Job) -> Result<()> {
        let queue = self.physical(&job.queue);
        let mut tx = self.pool.begin().await?;
        let updated = sqlx::query_scalar::<_, Uuid>(
            "UPDATE forge_jobs SET status = 'cancelled', completed_at = now(), \
             lease_token = NULL, leased_until = NULL, lease_secs = NULL \
             WHERE id = $1 AND queue = $2 AND lease_token = $3 AND status = 'leased' \
               AND cancel_requested_at IS NOT NULL RETURNING id",
        )
        .bind(job.id.0)
        .bind(&queue)
        .bind(job.lease_token())
        .fetch_optional(&mut *tx)
        .await?;
        if updated.is_none() {
            tx.rollback().await.ok();
            return Err(self.lease_lost_error(job.id.0, &queue).await);
        }
        sqlx::query(
            "INSERT INTO forge_queue_counters (queue, settled_total, cancelled_total) VALUES ($1, 1, 1) \
             ON CONFLICT (queue) DO UPDATE SET settled_total = forge_queue_counters.settled_total + 1, cancelled_total = forge_queue_counters.cancelled_total + 1",
        )
        .bind(&queue)
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(())
    }

    #[allow(clippy::disallowed_methods)]
    async fn status(&self, id: JobId) -> Result<Option<JobStatus>> {
        let prefix = (!self.namespace.is_empty()).then(|| format!("{}:", self.namespace));
        let row = sqlx::query(
            "SELECT id, queue, status, attempts, max_attempts, priority, concurrency_key, \
                    enqueued_at, available_at, completed_at, cancel_requested_at \
             FROM forge_jobs WHERE id = $1 AND ($2::text IS NULL OR left(queue, length($2)) = $2)",
        )
        .bind(id.0)
        .bind(prefix)
        .fetch_optional(&self.pool)
        .await?;
        row.map(|row| self.status_from_row(&row)).transpose()
    }

    #[allow(clippy::disallowed_methods)]
    async fn list_status(&self, filter: JobStatusFilter) -> Result<JobStatusPage> {
        check_operator_limit(filter.limit)?;
        if let Some(queue) = &filter.queue {
            check_queue_name(queue, true)?;
        }
        let after = filter
            .cursor
            .map(|value| {
                Uuid::parse_str(value.token())
                    .map_err(|_| ForgeError::invalid("invalid job-status cursor"))
            })
            .transpose()?;
        let states = filter
            .states
            .iter()
            .map(|state| state_label(*state))
            .collect::<Vec<_>>();
        let queue = filter.queue.map(|queue| self.physical(&queue));
        let prefix = (!self.namespace.is_empty()).then(|| format!("{}:", self.namespace));
        let rows = sqlx::query(
            "SELECT id, queue, status, attempts, max_attempts, priority, concurrency_key, \
                    enqueued_at, available_at, completed_at, cancel_requested_at \
             FROM forge_jobs j WHERE ($1::text IS NULL OR queue = $1) \
               AND ($2::text IS NULL OR left(queue, length($2)) = $2) \
               AND ($3::uuid IS NULL OR (enqueued_at, id) > \
                    (SELECT enqueued_at, id FROM forge_jobs WHERE id = $3)) \
               AND (cardinality($4::text[]) = 0 OR \
                    CASE WHEN status = 'available' AND attempts > 0 THEN 'retrying' \
                         WHEN status = 'available' AND available_at > now() THEN 'delayed' \
                         WHEN status = 'available' THEN 'queued' \
                         WHEN status = 'leased' AND cancel_requested_at IS NOT NULL THEN 'cancel_requested' \
                         WHEN status = 'leased' THEN 'leased' WHEN status = 'done' THEN 'succeeded' \
                         ELSE status END = ANY($4)) \
             ORDER BY enqueued_at, id LIMIT $5",
        ).bind(queue).bind(prefix).bind(after).bind(states).bind(i64::from(filter.limit) + 1).fetch_all(&self.pool).await?;
        let more = rows.len() > filter.limit as usize;
        let mut items = rows
            .iter()
            .take(filter.limit as usize)
            .map(|row| self.status_from_row(row))
            .collect::<Result<Vec<_>>>()?;
        let next_cursor = more.then(|| {
            crate::Cursor::from_token(
                items
                    .last()
                    .map(|item| item.id.to_string())
                    .unwrap_or_default(),
            )
        });
        Ok(JobStatusPage {
            items: std::mem::take(&mut items),
            next_cursor,
        })
    }

    // Dynamic decoding keeps the optional aggregate portable across PostgreSQL versions.
    #[allow(clippy::disallowed_methods)]
    async fn depth(&self, queue: &str) -> Result<QueueDepth> {
        let span = tracing::info_span!(
            "forge.queue.depth",
            queue = %queue,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("queue", "depth", span, async move {
            // Allow `.dlq` names: gauging a dead-letter backlog is the headline use.
            check_queue_name(queue, true)?;
            let row = sqlx::query(
                r#"SELECT
                     count(*) FILTER (WHERE status = 'available' AND available_at <= now()
                                         OR status = 'leased'    AND leased_until <= now()) AS visible,
                     count(*) FILTER (WHERE status = 'leased'    AND leased_until > now())  AS in_flight,
                     count(*) FILTER (WHERE status = 'available' AND available_at > now())   AS delayed,
                     (EXTRACT(EPOCH FROM (now() - min(enqueued_at) FILTER (
                         WHERE status = 'available' AND available_at <= now()
                            OR status = 'leased' AND leased_until <= now()
                     ))) * 1000)::double precision AS oldest_age_ms
                   FROM forge_jobs
                   WHERE queue = $1"#,
            )
            .bind(self.physical(queue))
            .fetch_one(&self.pool)
            .await?;
            use sqlx::Row as _;
            let visible = row.try_get::<i64, _>("visible")?;
            let in_flight = row.try_get::<i64, _>("in_flight")?;
            let delayed = row.try_get::<i64, _>("delayed")?;
            let oldest = row.try_get::<Option<f64>, _>("oldest_age_ms")?
                .map(|value| value.max(0.0) as u64);
            Ok(QueueDepth::new(
                u64::try_from(visible).unwrap_or(0),
                u64::try_from(in_flight).unwrap_or(0),
                u64::try_from(delayed).unwrap_or(0),
            ).with_oldest_visible_age_ms(oldest))
        })
        .await
    }

    #[allow(clippy::disallowed_methods)]
    async fn pause(&self, queue: &str) -> Result<()> {
        check_queue_name(queue, true)?;
        sqlx::query(
            "INSERT INTO forge_queue_controls (queue, paused) VALUES ($1, true) \
             ON CONFLICT (queue) DO UPDATE SET paused = true, updated_at = now()",
        )
        .bind(self.physical(queue))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    #[allow(clippy::disallowed_methods)]
    async fn resume(&self, queue: &str) -> Result<()> {
        check_queue_name(queue, true)?;
        sqlx::query(
            "INSERT INTO forge_queue_controls (queue, paused) VALUES ($1, false) \
             ON CONFLICT (queue) DO UPDATE SET paused = false, updated_at = now()",
        )
        .bind(self.physical(queue))
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    #[allow(clippy::disallowed_methods)]
    async fn is_paused(&self, queue: &str) -> Result<bool> {
        check_queue_name(queue, true)?;
        Ok(sqlx::query_scalar::<_, bool>(
            "SELECT COALESCE((SELECT paused FROM forge_queue_controls WHERE queue = $1), false)",
        )
        .bind(self.physical(queue))
        .fetch_one(&self.pool)
        .await?)
    }

    #[allow(clippy::disallowed_methods)]
    async fn stats(&self, queue: &str) -> Result<QueueStats> {
        check_queue_name(queue, true)?;
        let queue = self.physical(queue);
        let row = sqlx::query(
            "SELECT c.enqueued_total, c.settled_total, c.dead_total, c.cancelled_total, \
                    EXTRACT(EPOCH FROM (now() - c.started_at))::double precision AS elapsed_secs, \
                    COALESCE(controls.paused, false) AS paused, \
                    (SELECT (EXTRACT(EPOCH FROM (now() - jobs.enqueued_at)) * 1000)::double precision \
                     FROM forge_jobs jobs WHERE jobs.queue = $1 AND jobs.status = 'available' \
                       AND jobs.available_at <= now() ORDER BY jobs.enqueued_at LIMIT 1) AS oldest_age_ms \
             FROM (SELECT $1::text AS queue) requested \
             LEFT JOIN forge_queue_counters c ON c.queue = requested.queue \
             LEFT JOIN forge_queue_controls controls ON controls.queue = requested.queue",
        )
        .bind(&queue)
        .fetch_one(&self.pool)
        .await?;
        let enqueued = u64::try_from(
            row.try_get::<Option<i64>, _>("enqueued_total")?
                .unwrap_or(0),
        )
        .unwrap_or(0);
        let settled = u64::try_from(row.try_get::<Option<i64>, _>("settled_total")?.unwrap_or(0))
            .unwrap_or(0);
        let minutes = row
            .try_get::<Option<f64>, _>("elapsed_secs")?
            .unwrap_or(60.0)
            .max(1.0)
            / 60.0;
        Ok(QueueStats {
            enqueued_total: enqueued,
            settled_total: settled,
            dead_total: u64::try_from(row.try_get::<Option<i64>, _>("dead_total")?.unwrap_or(0))
                .unwrap_or(0),
            cancelled_total: u64::try_from(
                row.try_get::<Option<i64>, _>("cancelled_total")?
                    .unwrap_or(0),
            )
            .unwrap_or(0),
            enqueue_rate_per_minute: enqueued as f64 / minutes,
            settle_rate_per_minute: settled as f64 / minutes,
            oldest_visible_age_ms: row
                .try_get::<Option<f64>, _>("oldest_age_ms")?
                .map(|value| value.max(0.0) as u64),
            paused: row.try_get("paused")?,
        })
    }

    #[allow(clippy::disallowed_methods)]
    async fn dead_letters(
        &self,
        queue: &str,
        cursor: Option<crate::Cursor>,
        limit: u32,
    ) -> Result<DeadLetterPage> {
        check_queue_name(queue, false)?;
        check_operator_limit(limit)?;
        let after = cursor
            .map(|value| {
                Uuid::parse_str(value.token())
                    .map_err(|_| ForgeError::invalid("invalid dead-letter cursor"))
            })
            .transpose()?;
        let physical = self.physical(&format!("{queue}.dlq"));
        let rows = sqlx::query(
            "SELECT id, dead_attempts, enqueued_at, COALESCE(dead_lettered_at, enqueued_at) AS dead_lettered_at, failure_summary \
             FROM forge_jobs WHERE queue = $1 AND status IN ('available', 'dead') \
               AND ($2::uuid IS NULL OR id > $2) ORDER BY id LIMIT $3",
        )
        .bind(physical)
        .bind(after)
        .bind(i64::from(limit) + 1)
        .fetch_all(&self.pool)
        .await?;
        use sqlx::Row as _;
        let more = rows.len() > limit as usize;
        let mut items = Vec::with_capacity(rows.len().min(limit as usize));
        for row in rows.iter().take(limit as usize) {
            items.push(DeadLetterInfo {
                job_id: JobId(row.try_get("id")?),
                queue: queue.to_string(),
                attempt_count: u32::try_from(row.try_get::<i32, _>("dead_attempts")?).unwrap_or(0),
                enqueued_at: to_system_time(row.try_get("enqueued_at")?),
                dead_lettered_at: to_system_time(row.try_get("dead_lettered_at")?),
                failure_summary: row.try_get("failure_summary")?,
            });
        }
        let next_cursor = more.then(|| {
            crate::Cursor::from_token(
                items
                    .last()
                    .map(|item| item.job_id.to_string())
                    .unwrap_or_default(),
            )
        });
        Ok(DeadLetterPage { items, next_cursor })
    }

    #[allow(clippy::disallowed_methods)]
    async fn redrive(&self, job_id: JobId, opts: RedriveOpts) -> Result<bool> {
        check_queue_name(&opts.destination, false)?;
        let prefix = (!self.namespace.is_empty()).then(|| format!("{}:", self.namespace));
        let mut tx = self.pool.begin().await?;
        let retained = sqlx::query_scalar::<_, bool>(
            "SELECT payload_retained FROM forge_jobs \
             WHERE id = $1 AND queue LIKE '%.dlq' AND status IN ('available', 'dead') \
               AND ($2::text IS NULL OR left(queue, length($2)) = $2) FOR UPDATE",
        )
        .bind(job_id.0)
        .bind(&prefix)
        .fetch_optional(&mut *tx)
        .await?;
        match retained {
            None => {
                tx.commit().await?;
                return Ok(false);
            }
            Some(false) => {
                return Err(ForgeError::precondition(
                    "dead-letter payload retention elapsed; the job cannot be redriven",
                ));
            }
            Some(true) => {}
        }
        let updated = sqlx::query_scalar::<_, Uuid>(
            "UPDATE forge_jobs SET queue = $2, status = 'available', attempts = 0, \
                available_at = now(), completed_at = NULL, dead_attempts = 0, \
                dead_lettered_at = NULL, failure_summary = NULL, lease_token = NULL, \
                leased_until = NULL, lease_secs = NULL, payload_retained = true \
             WHERE id = $1 AND queue LIKE '%.dlq' AND status IN ('available', 'dead') \
               AND ($3::text IS NULL OR left(queue, length($3)) = $3) RETURNING id",
        )
        .bind(job_id.0)
        .bind(self.physical(&opts.destination))
        .bind(prefix)
        .fetch_optional(&mut *tx)
        .await?;
        if updated.is_some() && opts.dedup_policy == RedriveDedupPolicy::Clear {
            sqlx::query("DELETE FROM forge_job_dedup WHERE job_id = $1")
                .bind(job_id.0)
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(updated.is_some())
    }

    async fn redrive_batch(
        &self,
        queue: &str,
        cursor: Option<crate::Cursor>,
        limit: u32,
        opts: RedriveOpts,
    ) -> Result<RedriveBatchResult> {
        let page = self.dead_letters(queue, cursor, limit).await?;
        let mut redriven = 0;
        for item in &page.items {
            redriven += u32::from(self.redrive(item.job_id, opts.clone()).await?);
        }
        Ok(RedriveBatchResult {
            redriven,
            next_cursor: page.next_cursor,
        })
    }

    #[allow(clippy::disallowed_methods)]
    async fn purge_dead_letters_dry_run(&self, queue: &str) -> Result<u64> {
        check_queue_name(queue, false)?;
        let count: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM forge_jobs WHERE queue = $1 AND status IN ('available', 'dead')",
        )
        .bind(self.physical(&format!("{queue}.dlq")))
        .fetch_one(&self.pool)
        .await?;
        Ok(u64::try_from(count).unwrap_or(0))
    }

    #[allow(clippy::disallowed_methods)]
    async fn purge_dead_letters(&self, queue: &str, confirmation: &str) -> Result<u64> {
        check_queue_name(queue, false)?;
        if confirmation != queue {
            return Err(ForgeError::precondition(
                "purge confirmation must exactly match the source queue",
            ));
        }
        let result = sqlx::query(
            "DELETE FROM forge_jobs WHERE queue = $1 AND status IN ('available', 'dead')",
        )
        .bind(self.physical(&format!("{queue}.dlq")))
        .execute(&self.pool)
        .await?;
        Ok(result.rows_affected())
    }
}

/// Derive a stable per-job jitter seed from the job id's low 8 bytes.
fn seed_from_id(id: Uuid) -> u64 {
    u128::from_le_bytes(*id.as_bytes()) as u64
}

/// Postgres `TIMESTAMPTZ` (chrono) to `SystemTime`. Lease deadlines are always in the
/// future; a pre-epoch timestamp clamps to the epoch rather than underflowing.
fn to_system_time(dt: chrono::DateTime<chrono::Utc>) -> SystemTime {
    let secs = dt.timestamp().max(0) as u64;
    SystemTime::UNIX_EPOCH + Duration::new(secs, dt.timestamp_subsec_nanos())
}

fn check_queue_name(name: &str, allow_dlq: bool) -> Result<()> {
    if name.is_empty() {
        return Err(ForgeError::invalid("queue name must not be empty"));
    }
    if name.len() > MAX_QUEUE_NAME_BYTES {
        return Err(ForgeError::invalid(format!(
            "queue name is {} bytes; max is {MAX_QUEUE_NAME_BYTES}",
            name.len()
        )));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    {
        return Err(ForgeError::invalid(
            "queue name may only contain [A-Za-z0-9_.-]",
        ));
    }
    if !allow_dlq && name.ends_with(".dlq") {
        return Err(ForgeError::invalid(
            "queue name must not end in '.dlq' (reserved for dead-letter queues)",
        ));
    }
    Ok(())
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

fn check_enqueue_opts(opts: &EnqueueOpts) -> Result<()> {
    if opts.delay > MAX_DELAY {
        return Err(ForgeError::invalid(format!(
            "delay exceeds the maximum of {}s",
            MAX_DELAY.as_secs()
        )));
    }
    if opts.max_attempts == 0 || opts.max_attempts > MAX_MAX_ATTEMPTS {
        return Err(ForgeError::invalid(format!(
            "max_attempts must be in 1..={MAX_MAX_ATTEMPTS}"
        )));
    }
    if let Some(d) = &opts.dedup_id
        && d.len() > MAX_DEDUP_ID_LEN
    {
        return Err(ForgeError::limit(format!(
            "dedup_id is {} chars; max is {MAX_DEDUP_ID_LEN}",
            d.len()
        )));
    }
    if let Some(key) = &opts.concurrency_key {
        if key.is_empty() {
            return Err(ForgeError::invalid("concurrency_key must not be empty"));
        }
        if key.len() > MAX_CONCURRENCY_KEY_BYTES {
            return Err(ForgeError::limit(format!(
                "concurrency_key is {} bytes; max is {MAX_CONCURRENCY_KEY_BYTES}",
                key.len()
            )));
        }
    }
    Ok(())
}

fn check_visibility(vt: Duration) -> Result<()> {
    if vt.is_zero() || vt > MAX_VISIBILITY_TIMEOUT {
        return Err(ForgeError::invalid(format!(
            "visibility_timeout must be in (0, {}s]",
            MAX_VISIBILITY_TIMEOUT.as_secs()
        )));
    }
    Ok(())
}

fn check_operator_limit(limit: u32) -> Result<()> {
    if limit == 0 || limit > MAX_OPERATOR_BATCH {
        return Err(ForgeError::invalid(format!(
            "limit must be in 1..={MAX_OPERATOR_BATCH}"
        )));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn queue_name_validation() {
        assert!(check_queue_name("emails", false).is_ok());
        assert!(check_queue_name("a.b_c-1", false).is_ok());
        assert!(matches!(
            check_queue_name("", false),
            Err(ForgeError::Invalid(_))
        ));
        assert!(matches!(
            check_queue_name("bad space", false),
            Err(ForgeError::Invalid(_))
        ));
        // ".dlq" reserved for enqueue, allowed for dequeue.
        assert!(matches!(
            check_queue_name("emails.dlq", false),
            Err(ForgeError::Invalid(_))
        ));
        assert!(check_queue_name("emails.dlq", true).is_ok());
    }

    #[test]
    fn visibility_and_delay_bounds() {
        assert!(matches!(
            check_visibility(Duration::ZERO),
            Err(ForgeError::Invalid(_))
        ));
        assert!(check_visibility(Duration::from_secs(30)).is_ok());
        assert!(matches!(
            check_visibility(MAX_VISIBILITY_TIMEOUT + Duration::from_secs(1)),
            Err(ForgeError::Invalid(_))
        ));
    }

    #[test]
    fn seed_is_stable_per_id() {
        let id = Uuid::from_u128(0x1234_5678_9abc_def0_1122_3344_5566_7788);
        assert_eq!(seed_from_id(id), seed_from_id(id));
    }
}

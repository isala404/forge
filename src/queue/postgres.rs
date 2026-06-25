//! Postgres `queue` backend. Contract: docs/contracts/queue.md.
//!
//! One table `forge_jobs` with a `available -> leased -> done` state machine,
//! claimed via `FOR UPDATE SKIP LOCKED`. At-least-once: a lease that expires
//! (crash) or is nacked returns the job to `available` with `attempts` bumped;
//! on exhaustion the row is re-homed to the `"<queue>.dlq"` queue. A per-claim
//! `lease_token` fences stale `ack`/`nack`/`heartbeat` calls.
//!
//! Redelivery timing: an explicit `nack` applies the configured backoff (with
//! jitter, computed in Rust); a lease-expiry reclaim makes the job available
//! immediately, because a lease expiry means a worker likely crashed and prompt
//! retry by a healthy worker beats delay.

use super::{
    Backoff, DequeueOpts, EnqueueOpts, Job, JobId, MAX_PAYLOAD_BYTES, MAX_VISIBILITY_TIMEOUT,
    MAX_WAIT, NackOpts, Queue, QueueDepth,
};
use crate::error::{ForgeError, Result};
use crate::obs;
use async_trait::async_trait;
use bytes::Bytes;
use sqlx::PgPool;
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
    retention: Duration,
    /// Namespace prefix on queue names (`<ns>:<queue>`), so apps sharing a database
    /// don't cross-consume each other's queues. Empty = no prefix.
    namespace: String,
}

impl PgQueue {
    pub(crate) fn new(
        pool: PgPool,
        dedup_window: Duration,
        retention: Duration,
        namespace: String,
    ) -> Self {
        Self {
            pool,
            dedup_window,
            retention,
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
    async fn reclaim(&self, queue: &str) -> Result<()> {
        // Exhausted first — re-homing changes `queue`, so the second statement
        // (still scoped to `queue`) won't touch them again.
        sqlx::query!(
            "UPDATE forge_jobs \
             SET queue = queue || '.dlq', status = 'available', attempts = 0, \
                 available_at = now(), lease_token = NULL, leased_until = NULL, lease_secs = NULL \
             WHERE queue = $1 AND status = 'leased' AND leased_until <= now() \
               AND attempts + 1 >= max_attempts",
            queue
        )
        .execute(&self.pool)
        .await?;

        sqlx::query!(
            "UPDATE forge_jobs \
             SET status = 'available', attempts = attempts + 1, \
                 available_at = now(), lease_token = NULL, leased_until = NULL, lease_secs = NULL \
             WHERE queue = $1 AND status = 'leased' AND leased_until <= now()",
            queue
        )
        .execute(&self.pool)
        .await?;
        Ok(())
    }

    /// Try to claim and lease exactly one due job. `None` if none are ready.
    async fn try_claim(&self, queue: &str, vis_secs: f64) -> Result<Option<Job>> {
        let row = sqlx::query!(
            r#"WITH claimed AS (
                   SELECT id FROM forge_jobs
                   WHERE queue = $1 AND status = 'available' AND available_at <= now()
                   ORDER BY available_at
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
               RETURNING j.id AS "id!", j.queue AS "queue!", j.payload AS "payload!",
                         j.attempts AS "attempts!", j.max_attempts AS "max_attempts!",
                         j.leased_until AS "leased_until!", j.lease_token AS "lease_token!""#,
            queue,
            vis_secs,
        )
        .fetch_optional(&self.pool)
        .await?;

        Ok(row.map(|r| {
            Job::new(
                JobId(r.id),
                r.queue,
                Bytes::from(r.payload),
                // attempts counts FAILED deliveries; this delivery is attempts + 1.
                u32::try_from(r.attempts).unwrap_or(0).saturating_add(1),
                u32::try_from(r.max_attempts).unwrap_or(0),
                to_system_time(r.leased_until),
                r.lease_token,
            )
        }))
    }

    /// Maintenance sweep: purge old `done` jobs, reclaim expired leases across
    /// all queues, drop stale dedup entries. Idempotent.
    pub(crate) async fn maintenance(&self) -> Result<()> {
        let retention_secs = self.retention.as_secs_f64();
        sqlx::query!(
            "DELETE FROM forge_jobs \
             WHERE status = 'done' AND completed_at <= now() - make_interval(secs => $1)",
            retention_secs
        )
        .execute(&self.pool)
        .await?;

        sqlx::query!(
            "UPDATE forge_jobs \
             SET queue = queue || '.dlq', status = 'available', attempts = 0, \
                 available_at = now(), lease_token = NULL, leased_until = NULL, lease_secs = NULL \
             WHERE status = 'leased' AND leased_until <= now() AND attempts + 1 >= max_attempts"
        )
        .execute(&self.pool)
        .await?;
        sqlx::query!(
            "UPDATE forge_jobs \
             SET status = 'available', attempts = attempts + 1, \
                 available_at = now(), lease_token = NULL, leased_until = NULL, lease_secs = NULL \
             WHERE status = 'leased' AND leased_until <= now()"
        )
        .execute(&self.pool)
        .await?;

        sqlx::query!("DELETE FROM forge_job_dedup WHERE expires_at <= now()")
            .execute(&self.pool)
            .await?;
        Ok(())
    }

    /// Shared `nack`/`heartbeat` failure path when the fenced update matched no
    /// row: NotFound if the id is unknown, else Precondition (lease lost).
    async fn lease_lost_error(&self, id: Uuid) -> ForgeError {
        match sqlx::query_scalar!(
            r#"SELECT EXISTS(SELECT 1 FROM forge_jobs WHERE id = $1) AS "exists!""#,
            id
        )
        .fetch_one(&self.pool)
        .await
        {
            Ok(true) => ForgeError::precondition("lease lost — another worker owns this job"),
            Ok(false) => ForgeError::NotFound,
            Err(e) => e.into(),
        }
    }
}

#[async_trait]
impl Queue for PgQueue {
    async fn enqueue(&self, queue: &str, payload: Bytes, opts: EnqueueOpts) -> Result<JobId> {
        let delay_secs = opts.delay.as_secs_f64();
        let max_attempts = i32::try_from(opts.max_attempts).unwrap_or(i32::MAX);
        let payload_vec = payload.as_ref().to_vec();
        let dedup_window_secs = self.dedup_window.as_secs_f64();
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
            let queue = self.physical(queue); // apply namespace; SQL below binds it
            let Some(dedup_id) = opts.dedup_id.clone() else {
                let id = sqlx::query_scalar!(
                    r#"INSERT INTO forge_jobs (queue, payload, status, attempts, max_attempts, available_at)
                       VALUES ($1, $2, 'available', 0, $3, now() + make_interval(secs => $4))
                       RETURNING id"#,
                    queue,
                    payload_vec,
                    max_attempts,
                    delay_secs,
                )
                .fetch_one(&self.pool)
                .await?;
                tracing::Span::current().record("dedup_hit", false);
                return Ok(JobId(id));
            };

            // The upsert always returns the surviving dedup row in one round-trip. When
            // the slot was free or expired, the CASE rewrites job_id to our new_id, so
            // `claimed` is true and we insert the job; a still-live slot keeps its
            // existing job_id, so `claimed` is false and we return it without a second
            // lookup. (new_id is freshly random, so it can't equal an existing live id.)
            let new_id = Uuid::new_v4();
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
                sqlx::query!(
                    r#"INSERT INTO forge_jobs (id, queue, payload, status, attempts, max_attempts, available_at)
                       VALUES ($1, $2, $3, 'available', 0, $4, now() + make_interval(secs => $5))"#,
                    new_id,
                    queue,
                    payload_vec,
                    max_attempts,
                    delay_secs,
                )
                .execute(&mut *tx)
                .await?;
                tx.commit().await?;
                tracing::Span::current().record("dedup_hit", false);
                Ok(JobId(new_id))
            } else {
                // Live dedup entry — return the existing job id (success).
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
            let pq = self.physical(queue);
            // Reclaim expired leases once up front so crashed work redelivers.
            self.reclaim(&pq).await?;

            let deadline = tokio::time::Instant::now() + wait;
            loop {
                if let Some(mut job) = self.try_claim(&pq, vis_secs).await? {
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

    async fn ack(&self, job: &Job) -> Result<()> {
        let id = job.id.0;
        let token = job.lease_token();
        let span = tracing::info_span!(
            "forge.queue.ack",
            queue = %job.queue,
            job.id = %job.id,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("queue", "ack", span, async move {
            // Idempotent: a lost/reclaimed lease matches 0 rows and is still Ok.
            sqlx::query!(
                "UPDATE forge_jobs SET status = 'done', completed_at = now(), \
                 lease_token = NULL, leased_until = NULL, lease_secs = NULL \
                 WHERE id = $1 AND lease_token = $2 AND status = 'leased'",
                id,
                token,
            )
            .execute(&self.pool)
            .await?;
            Ok(())
        })
        .await
    }

    async fn nack(&self, job: &Job, opts: NackOpts) -> Result<()> {
        let id = job.id.0;
        let token = job.lease_token();
        let seed = seed_from_id(id);
        // A job already in a *.dlq queue must not re-home into an unwatched .dlq.dlq;
        // exhaustion there is terminal (P1-4).
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
            let row = sqlx::query!(
                r#"SELECT attempts AS "attempts!", max_attempts AS "max_attempts!"
                   FROM forge_jobs
                   WHERE id = $1 AND lease_token = $2 AND status = 'leased'
                   FOR UPDATE"#,
                id,
                token,
            )
            .fetch_optional(&mut *tx)
            .await?;

            let Some(row) = row else {
                tx.rollback().await.ok();
                return Err(self.lease_lost_error(id).await);
            };

            let new_attempts = u32::try_from(row.attempts).unwrap_or(0).saturating_add(1);
            if new_attempts >= u32::try_from(row.max_attempts).unwrap_or(u32::MAX) {
                if in_dlq {
                    // Terminal: park as 'dead' with attempts pinned. Nothing re-homes a
                    // dead-letter job into .dlq.dlq; a dead row is observable + queryable.
                    sqlx::query!(
                        "UPDATE forge_jobs \
                         SET status = 'dead', attempts = $2, \
                             lease_token = NULL, leased_until = NULL, lease_secs = NULL \
                         WHERE id = $1",
                        id,
                        i32::try_from(new_attempts).unwrap_or(i32::MAX),
                    )
                    .execute(&mut *tx)
                    .await?;
                    tx.commit().await?;
                    tracing::Span::current().record("outcome", "dead");
                    return Ok(());
                }
                // Exhausted — re-home to DLQ with attempts reset so DLQ consumers see a clean slate.
                sqlx::query!(
                    "UPDATE forge_jobs \
                     SET queue = queue || '.dlq', status = 'available', attempts = 0, \
                         available_at = now(), lease_token = NULL, leased_until = NULL, lease_secs = NULL \
                     WHERE id = $1",
                    id
                )
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
            sqlx::query!(
                "UPDATE forge_jobs \
                 SET status = 'available', attempts = $2, \
                     available_at = now() + make_interval(secs => $3), \
                     lease_token = NULL, leased_until = NULL, lease_secs = NULL \
                 WHERE id = $1",
                id,
                i32::try_from(new_attempts).unwrap_or(i32::MAX),
                delay_secs,
            )
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
            tracing::Span::current().record("retry_in_ms", delay.as_millis() as i64);
            Ok(())
        })
        .await
    }

    async fn heartbeat(&self, job: &Job) -> Result<()> {
        let id = job.id.0;
        let token = job.lease_token();
        let span = tracing::info_span!(
            "forge.queue.heartbeat",
            queue = %job.queue,
            job.id = %job.id,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("queue", "heartbeat", span, async move {
            let extended = sqlx::query_scalar!(
                "UPDATE forge_jobs SET leased_until = now() + make_interval(secs => lease_secs) \
                 WHERE id = $1 AND lease_token = $2 AND status = 'leased' \
                 RETURNING id",
                id,
                token,
            )
            .fetch_optional(&self.pool)
            .await?;
            if extended.is_none() {
                return Err(self.lease_lost_error(id).await);
            }
            Ok(())
        })
        .await
    }

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
            let row = sqlx::query!(
                r#"SELECT
                     count(*) FILTER (WHERE status = 'available' AND available_at <= now()
                                         OR status = 'leased'    AND leased_until <= now()) AS "visible!",
                     count(*) FILTER (WHERE status = 'leased'    AND leased_until > now())  AS "in_flight!",
                     count(*) FILTER (WHERE status = 'available' AND available_at > now())   AS "delayed!"
                   FROM forge_jobs
                   WHERE queue = $1"#,
                self.physical(queue),
            )
            .fetch_one(&self.pool)
            .await?;
            Ok(QueueDepth::new(
                u64::try_from(row.visible).unwrap_or(0),
                u64::try_from(row.in_flight).unwrap_or(0),
                u64::try_from(row.delayed).unwrap_or(0),
            ))
        })
        .await
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

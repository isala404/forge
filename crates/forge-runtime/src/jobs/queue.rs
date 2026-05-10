use chrono::{DateTime, Utc};
use forge_core::job::{JobPriority, JobStatus};
use uuid::Uuid;

/// A job record in the database.
#[derive(Debug, Clone)]
pub struct JobRecord {
    /// Unique job ID.
    pub id: Uuid,
    /// Job type/name.
    pub job_type: String,
    /// Job input as JSON.
    pub input: serde_json::Value,
    /// Job output as JSON (if completed).
    pub output: Option<serde_json::Value>,
    /// Persisted context data.
    pub job_context: serde_json::Value,
    /// Current status.
    pub status: JobStatus,
    /// Priority level.
    pub priority: i32,
    /// Number of attempts made.
    pub attempts: i32,
    /// Maximum attempts allowed.
    pub max_attempts: i32,
    /// Last error message.
    pub last_error: Option<String>,
    /// Required worker capability.
    pub worker_capability: Option<String>,
    /// Worker ID that claimed the job.
    pub worker_id: Option<Uuid>,
    /// Idempotency key for deduplication.
    pub idempotency_key: Option<String>,
    /// Principal that created the job (for access control).
    pub owner_subject: Option<String>,
    /// When the job is scheduled to run.
    pub scheduled_at: DateTime<Utc>,
    /// When the job was created.
    pub created_at: DateTime<Utc>,
    /// When the job was claimed.
    pub claimed_at: Option<DateTime<Utc>>,
    /// When the job started running.
    pub started_at: Option<DateTime<Utc>>,
    /// When the job completed.
    pub completed_at: Option<DateTime<Utc>>,
    /// When the job failed.
    pub failed_at: Option<DateTime<Utc>>,
    /// Last heartbeat time.
    pub last_heartbeat: Option<DateTime<Utc>>,
    /// Cancellation requested at.
    pub cancel_requested_at: Option<DateTime<Utc>>,
    /// Cancellation completed at.
    pub cancelled_at: Option<DateTime<Utc>>,
    /// Cancellation reason.
    pub cancel_reason: Option<String>,
}

impl JobRecord {
    /// Create a new job record.
    pub fn new(
        job_type: impl Into<String>,
        input: serde_json::Value,
        priority: JobPriority,
        max_attempts: i32,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            job_type: job_type.into(),
            input,
            output: None,
            job_context: serde_json::json!({}),
            status: JobStatus::Pending,
            priority: priority.as_i32(),
            attempts: 0,
            max_attempts,
            last_error: None,
            worker_capability: None,
            worker_id: None,
            idempotency_key: None,
            owner_subject: None,
            scheduled_at: Utc::now(),
            created_at: Utc::now(),
            claimed_at: None,
            started_at: None,
            completed_at: None,
            failed_at: None,
            last_heartbeat: None,
            cancel_requested_at: None,
            cancelled_at: None,
            cancel_reason: None,
        }
    }

    /// Set worker capability requirement.
    pub fn with_capability(mut self, capability: impl Into<String>) -> Self {
        self.worker_capability = Some(capability.into());
        self
    }

    /// Set scheduled time.
    pub fn with_scheduled_at(mut self, at: DateTime<Utc>) -> Self {
        self.scheduled_at = at;
        self
    }

    /// Set idempotency key.
    pub fn with_idempotency_key(mut self, key: impl Into<String>) -> Self {
        self.idempotency_key = Some(key.into());
        self
    }

    /// Set owner subject.
    pub fn with_owner_subject(mut self, owner_subject: Option<String>) -> Self {
        self.owner_subject = owner_subject;
        self
    }
}

/// Job queue operations.
#[derive(Clone)]
pub struct JobQueue {
    pool: sqlx::PgPool,
}

impl JobQueue {
    /// Default retention for terminal jobs (completed, failed, cancelled).
    const DEFAULT_RETENTION: std::time::Duration = std::time::Duration::from_secs(7 * 24 * 3600);

    /// Create a new job queue.
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    /// Enqueue a new job. If the job has an idempotency key that matches
    /// an existing non-terminal job, returns the existing job's ID.
    pub async fn enqueue(&self, job: JobRecord) -> Result<Uuid, sqlx::Error> {
        let mut conn = self.pool.acquire().await?;
        Self::enqueue_inner(&mut conn, &job).await
    }

    /// Enqueue a job on an existing connection (typically a transaction).
    ///
    /// Use when the dispatch must be atomic with other writes — e.g. a
    /// mutation handler buffering a job that should only become visible
    /// to workers after the surrounding transaction commits.
    pub async fn enqueue_in_conn(
        &self,
        conn: &mut sqlx::PgConnection,
        job: JobRecord,
    ) -> Result<Uuid, sqlx::Error> {
        Self::enqueue_inner(conn, &job).await
    }

    async fn enqueue_inner(
        conn: &mut sqlx::PgConnection,
        job: &JobRecord,
    ) -> Result<Uuid, sqlx::Error> {
        // Fast path: check for existing idempotent job before attempting INSERT.
        // The UNIQUE partial index on idempotency_key guards against races.
        if let Some(ref key) = job.idempotency_key {
            let existing = sqlx::query_scalar!(
                r#"
                SELECT id FROM forge_jobs
                WHERE idempotency_key = $1
                  AND status NOT IN ('completed', 'failed', 'dead_letter', 'cancelled')
                "#,
                key
            )
            .fetch_optional(&mut *conn)
            .await?;

            if let Some(id) = existing {
                return Ok(id);
            }
        }

        sqlx::query!(
            r#"
            INSERT INTO forge_jobs (
                id, job_type, input, job_context, status, priority, attempts, max_attempts,
                worker_capability, idempotency_key, owner_subject, scheduled_at, created_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, $12, $13
            )
            ON CONFLICT DO NOTHING
            "#,
            job.id,
            &job.job_type,
            job.input as _,
            job.job_context as _,
            job.status.as_str(),
            job.priority,
            job.attempts,
            job.max_attempts,
            job.worker_capability as _,
            job.idempotency_key as _,
            job.owner_subject as _,
            job.scheduled_at,
            job.created_at,
        )
        .execute(&mut *conn)
        .await?;

        // If ON CONFLICT fired (race with another enqueue), fetch the winner's ID.
        if let Some(ref key) = job.idempotency_key {
            let id = sqlx::query_scalar!(
                r#"
                SELECT id FROM forge_jobs
                WHERE idempotency_key = $1
                  AND status NOT IN ('completed', 'failed', 'dead_letter', 'cancelled')
                "#,
                key
            )
            .fetch_optional(&mut *conn)
            .await?;

            if let Some(winner) = id {
                return Ok(winner);
            }
        }

        Ok(job.id)
    }

    /// Claim jobs using SKIP LOCKED pattern.
    pub async fn claim(
        &self,
        worker_id: Uuid,
        capabilities: &[String],
        limit: i32,
    ) -> Result<Vec<JobRecord>, sqlx::Error> {
        let rows = sqlx::query!(
            r#"
            WITH claimable AS (
                SELECT id
                FROM forge_jobs
                WHERE status = 'pending'
                  AND scheduled_at <= NOW()
                  AND (worker_capability = ANY($2) OR worker_capability IS NULL)
                ORDER BY priority DESC, scheduled_at ASC
                LIMIT $3
                FOR UPDATE SKIP LOCKED
            )
            UPDATE forge_jobs
            SET
                status = 'claimed',
                worker_id = $1,
                claimed_at = NOW(),
                attempts = attempts + 1
            WHERE id IN (SELECT id FROM claimable)
            RETURNING
                id, job_type, input, output, job_context, status, priority,
                attempts, max_attempts, last_error, worker_capability,
                worker_id, idempotency_key, owner_subject, scheduled_at, created_at,
                claimed_at, started_at, completed_at, failed_at, last_heartbeat,
                cancel_requested_at, cancelled_at, cancel_reason
            "#,
            worker_id,
            capabilities,
            limit as i64,
        )
        .fetch_all(&self.pool)
        .await?;

        let jobs = rows
            .into_iter()
            .map(|row| JobRecord {
                id: row.id,
                job_type: row.job_type,
                input: row.input,
                output: row.output,
                job_context: row.job_context,
                status: row
                    .status
                    .parse()
                    .unwrap_or(forge_core::job::JobStatus::Failed),
                priority: row.priority,
                attempts: row.attempts,
                max_attempts: row.max_attempts,
                last_error: row.last_error,
                worker_capability: row.worker_capability,
                worker_id: row.worker_id,
                idempotency_key: row.idempotency_key,
                owner_subject: row.owner_subject,
                scheduled_at: row.scheduled_at,
                created_at: row.created_at,
                claimed_at: row.claimed_at,
                started_at: row.started_at,
                completed_at: row.completed_at,
                failed_at: row.failed_at,
                last_heartbeat: row.last_heartbeat,
                cancel_requested_at: row.cancel_requested_at,
                cancelled_at: row.cancelled_at,
                cancel_reason: row.cancel_reason,
            })
            .collect();

        Ok(jobs)
    }

    /// Mark job as running.
    /// Mark a claimed job as running. The `(worker_id, attempts)` tuple
    /// fences the transition: stale-reclaim resets the row to `pending` and
    /// a new worker gets a fresh `attempts`, so the original claimant's
    /// `start()` returns `RowNotFound` and aborts before doing real work.
    /// This delivers execute-time idempotency without a separate ledger.
    pub async fn start(
        &self,
        job_id: Uuid,
        worker_id: Uuid,
        attempts: i32,
    ) -> Result<(), sqlx::Error> {
        let result = sqlx::query!(
            r#"
            UPDATE forge_jobs
            SET status = 'running', started_at = NOW(), last_heartbeat = NOW()
            WHERE id = $1
              AND worker_id = $2
              AND attempts = $3
              AND status = 'claimed'
            "#,
            job_id,
            worker_id,
            attempts,
        )
        .execute(&self.pool)
        .await?;

        if result.rows_affected() == 0 {
            return Err(sqlx::Error::RowNotFound);
        }

        Ok(())
    }

    /// Mark job as completed.
    ///
    /// Sets `expires_at` for automatic cleanup. Uses the provided TTL or
    /// defaults to 7 days so completed jobs don't accumulate forever.
    pub async fn complete(
        &self,
        job_id: Uuid,
        output: serde_json::Value,
        ttl: Option<std::time::Duration>,
    ) -> Result<(), sqlx::Error> {
        let retention = ttl.unwrap_or(Self::DEFAULT_RETENTION);
        let expires_at = Some(
            chrono::Utc::now()
                + chrono::Duration::from_std(retention).unwrap_or(chrono::Duration::days(7)),
        );

        sqlx::query!(
            r#"
            UPDATE forge_jobs
            SET
                status = 'completed',
                output = $2,
                completed_at = NOW(),
                cancel_requested_at = NULL,
                cancelled_at = NULL,
                cancel_reason = NULL,
                expires_at = $3
            WHERE id = $1 AND status = 'running'
            "#,
            job_id,
            output as _,
            expires_at,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Mark job as failed, schedule retry or move to dead letter.
    ///
    /// If `ttl` is provided and job moves to dead_letter, sets `expires_at` for automatic cleanup.
    pub async fn fail(
        &self,
        job_id: Uuid,
        error: &str,
        retry_delay: Option<chrono::Duration>,
        ttl: Option<std::time::Duration>,
    ) -> Result<(), sqlx::Error> {
        if let Some(delay) = retry_delay {
            // Schedule retry
            sqlx::query!(
                r#"
                UPDATE forge_jobs
                SET
                    status = 'pending',
                    worker_id = NULL,
                    claimed_at = NULL,
                    started_at = NULL,
                    last_error = $2,
                    scheduled_at = NOW() + make_interval(secs => $3),
                    cancel_requested_at = NULL,
                    cancelled_at = NULL,
                    cancel_reason = NULL
                WHERE id = $1 AND status = 'running'
                "#,
                job_id,
                error,
                delay.num_seconds() as f64,
            )
            .execute(&self.pool)
            .await?;
        } else {
            // Move to dead letter
            let retention = ttl.unwrap_or(Self::DEFAULT_RETENTION);
            let expires_at = Some(
                chrono::Utc::now()
                    + chrono::Duration::from_std(retention).unwrap_or(chrono::Duration::days(7)),
            );

            sqlx::query!(
                r#"
                UPDATE forge_jobs
                SET
                    status = 'dead_letter',
                    last_error = $2,
                    failed_at = NOW(),
                    cancel_requested_at = NULL,
                    cancelled_at = NULL,
                    cancel_reason = NULL,
                    expires_at = $3
                WHERE id = $1 AND status = 'running'
                "#,
                job_id,
                error,
                expires_at,
            )
            .execute(&self.pool)
            .await?;
        }

        Ok(())
    }

    /// Update heartbeat for a running job.
    pub async fn heartbeat(&self, job_id: Uuid) -> Result<(), sqlx::Error> {
        sqlx::query!(
            r#"
            UPDATE forge_jobs
            SET last_heartbeat = NOW()
            WHERE id = $1
            "#,
            job_id,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Update job progress.
    pub async fn update_progress(
        &self,
        job_id: Uuid,
        percent: i32,
        message: &str,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            r#"
            UPDATE forge_jobs
            SET progress_percent = $2, progress_message = $3, last_heartbeat = NOW()
            WHERE id = $1
            "#,
            job_id,
            percent,
            message,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Replace persisted job context.
    pub async fn set_context(
        &self,
        job_id: Uuid,
        context: serde_json::Value,
    ) -> Result<(), sqlx::Error> {
        sqlx::query!(
            r#"
            UPDATE forge_jobs
            SET job_context = $2
            WHERE id = $1
            "#,
            job_id,
            context as _,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Request cancellation for a job.
    ///
    /// If `caller_subject` is provided, the cancellation will only succeed if
    /// the job has no `owner_subject` or the `owner_subject` matches the caller.
    /// This prevents unauthorized users from cancelling other users' jobs.
    pub async fn request_cancel(
        &self,
        job_id: Uuid,
        reason: Option<&str>,
        caller_subject: Option<&str>,
    ) -> Result<bool, sqlx::Error> {
        let row = sqlx::query!(
            "SELECT status, owner_subject FROM forge_jobs WHERE id = $1",
            job_id
        )
        .fetch_optional(&self.pool)
        .await?;

        let (status, owner_subject) = match row {
            Some(r) => (r.status, r.owner_subject),
            None => return Ok(false),
        };

        // Verify ownership: if job has an owner, caller must match.
        // Reject if no caller_subject is provided for an owned job.
        if let Some(ref owner) = owner_subject {
            match caller_subject {
                Some(caller) if caller == owner => { /* authorized */ }
                _ => return Ok(false), // no caller or mismatch -> deny
            }
        }

        // Statuses where this call should be a no-op. Includes the four
        // hard-terminal states plus `cancel_requested`: once a graceful cancel
        // is in flight the worker (or `release_stale`) finalizes the job, and a
        // second cancel must not race that by force-flipping to `cancelled`.
        let noop_statuses = [
            JobStatus::Completed.as_str(),
            JobStatus::Failed.as_str(),
            JobStatus::DeadLetter.as_str(),
            JobStatus::Cancelled.as_str(),
            JobStatus::CancelRequested.as_str(),
        ];

        if status == JobStatus::Running.as_str() {
            let updated = sqlx::query!(
                r#"
                UPDATE forge_jobs
                SET
                    status = 'cancel_requested',
                    cancel_requested_at = NOW(),
                    cancel_reason = COALESCE($2, cancel_reason)
                WHERE id = $1
                  AND status = 'running'
                "#,
                job_id,
                reason,
            )
            .execute(&self.pool)
            .await?;

            return Ok(updated.rows_affected() > 0);
        }

        if noop_statuses.contains(&status.as_str()) {
            return Ok(false);
        }

        let updated = sqlx::query!(
            r#"
            UPDATE forge_jobs
            SET
                status = 'cancelled',
                cancelled_at = NOW(),
                cancel_reason = COALESCE($2, cancel_reason)
            WHERE id = $1
              AND status NOT IN ('completed', 'failed', 'dead_letter', 'cancelled')
            "#,
            job_id,
            reason,
        )
        .execute(&self.pool)
        .await?;

        Ok(updated.rows_affected() > 0)
    }

    /// Mark job as cancelled.
    ///
    /// Sets `expires_at` for automatic cleanup. Uses the provided TTL or
    /// defaults to 7 days.
    pub async fn cancel(
        &self,
        job_id: Uuid,
        reason: Option<&str>,
        ttl: Option<std::time::Duration>,
    ) -> Result<(), sqlx::Error> {
        let retention = ttl.unwrap_or(Self::DEFAULT_RETENTION);
        let expires_at = Some(
            chrono::Utc::now()
                + chrono::Duration::from_std(retention).unwrap_or(chrono::Duration::days(7)),
        );

        sqlx::query!(
            r#"
            UPDATE forge_jobs
            SET
                status = 'cancelled',
                cancelled_at = NOW(),
                cancel_reason = COALESCE($2, cancel_reason),
                expires_at = $3
            WHERE id = $1 AND status NOT IN ('completed', 'failed', 'dead_letter', 'cancelled')
            "#,
            job_id,
            reason,
            expires_at,
        )
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Release stale jobs back to pending — or finalize them as cancelled if a
    /// cancellation was already in flight.
    ///
    /// Jobs with `cancel_requested_at` set are not re-queued: the worker either
    /// already saw the cancel signal (in which case it'll write `cancelled`
    /// itself) or died mid-cancel (in which case re-queuing would silently drop
    /// the cancellation and double-fire any side effects). The latter case is
    /// finalized to `cancelled` here so the job exits its lifecycle cleanly.
    pub async fn release_stale(
        &self,
        stale_threshold: chrono::Duration,
    ) -> Result<u64, sqlx::Error> {
        let secs = stale_threshold.num_seconds() as f64;

        let finalized = sqlx::query!(
            r#"
            UPDATE forge_jobs
            SET
                status = 'cancelled',
                cancelled_at = NOW(),
                cancel_reason = COALESCE(cancel_reason, 'worker died mid-cancel')
            WHERE
                cancel_requested_at IS NOT NULL
                AND status IN ('claimed', 'running', 'cancel_requested')
                AND COALESCE(last_heartbeat, started_at, claimed_at) < NOW() - make_interval(secs => $1)
            "#,
            secs,
        )
        .execute(&self.pool)
        .await?;

        let reset = sqlx::query!(
            r#"
            UPDATE forge_jobs
            SET
                status = 'pending',
                worker_id = NULL,
                claimed_at = NULL,
                started_at = NULL,
                last_heartbeat = NULL
            WHERE
                cancel_requested_at IS NULL
                AND (
                    (
                        status = 'claimed'
                        AND claimed_at < NOW() - make_interval(secs => $1)
                    )
                    OR (
                        status = 'running'
                        AND COALESCE(last_heartbeat, started_at, claimed_at) < NOW() - make_interval(secs => $1)
                    )
                )
            "#,
            secs,
        )
        .execute(&self.pool)
        .await?;

        Ok(finalized.rows_affected() + reset.rows_affected())
    }

    /// Delete expired job records.
    ///
    /// Only deletes terminal jobs (completed, cancelled, failed, dead_letter)
    /// that have passed their TTL.
    pub async fn cleanup_expired(&self) -> Result<u64, sqlx::Error> {
        let result = sqlx::query!(
            r#"
            DELETE FROM forge_jobs
            WHERE expires_at IS NOT NULL
              AND expires_at < NOW()
              AND status IN ('completed', 'cancelled', 'failed', 'dead_letter')
            "#,
        )
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }

    /// Get queue statistics.
    pub async fn stats(&self) -> Result<QueueStats, sqlx::Error> {
        let row = sqlx::query!(
            r#"
            SELECT
                COUNT(*) FILTER (WHERE status = 'pending') as "pending!",
                COUNT(*) FILTER (WHERE status = 'claimed') as "claimed!",
                COUNT(*) FILTER (WHERE status = 'running') as "running!",
                COUNT(*) FILTER (WHERE status = 'completed') as "completed!",
                COUNT(*) FILTER (WHERE status = 'cancelled') as "cancelled!",
                COUNT(*) FILTER (WHERE status = 'failed') as "failed!",
                COUNT(*) FILTER (WHERE status = 'dead_letter') as "dead_letter!"
            FROM forge_jobs
            "#,
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(QueueStats {
            pending: row.pending as u64,
            claimed: row.claimed as u64,
            running: row.running as u64,
            completed: row.completed as u64,
            cancelled: row.cancelled as u64,
            failed: row.failed as u64,
            dead_letter: row.dead_letter as u64,
        })
    }
}

/// Queue statistics.
#[derive(Debug, Clone, Default)]
pub struct QueueStats {
    pub pending: u64,
    pub claimed: u64,
    pub running: u64,
    pub completed: u64,
    pub cancelled: u64,
    pub failed: u64,
    pub dead_letter: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_job_record_creation() {
        let job = JobRecord::new("send_email", serde_json::json!({}), JobPriority::Normal, 3);

        assert_eq!(job.job_type, "send_email");
        assert_eq!(job.status, JobStatus::Pending);
        assert_eq!(job.priority, 50);
        assert_eq!(job.attempts, 0);
        assert_eq!(job.max_attempts, 3);
    }

    #[test]
    fn test_job_record_with_capability() {
        let job = JobRecord::new("transcode", serde_json::json!({}), JobPriority::High, 3)
            .with_capability("media");

        assert_eq!(job.worker_capability, Some("media".to_string()));
        assert_eq!(job.priority, 75);
    }

    #[test]
    fn test_job_record_with_idempotency() {
        let job = JobRecord::new("payment", serde_json::json!({}), JobPriority::Critical, 5)
            .with_idempotency_key("payment-123");

        assert_eq!(job.idempotency_key, Some("payment-123".to_string()));
    }

    #[test]
    fn test_job_record_with_owner_subject() {
        let job = JobRecord::new("task", serde_json::json!({}), JobPriority::Normal, 3)
            .with_owner_subject(Some("user-123".into()));
        assert_eq!(job.owner_subject, Some("user-123".to_string()));
    }

    #[test]
    fn test_priority_ordering() {
        let bg = JobRecord::new("a", serde_json::json!({}), JobPriority::Background, 1);
        let low = JobRecord::new("b", serde_json::json!({}), JobPriority::Low, 1);
        let normal = JobRecord::new("c", serde_json::json!({}), JobPriority::Normal, 1);
        let high = JobRecord::new("d", serde_json::json!({}), JobPriority::High, 1);
        let critical = JobRecord::new("e", serde_json::json!({}), JobPriority::Critical, 1);

        assert!(bg.priority < low.priority);
        assert!(low.priority < normal.priority);
        assert!(normal.priority < high.priority);
        assert!(high.priority < critical.priority);
    }
}

/// Integration tests requiring a real PostgreSQL database.
/// Run with: cargo test -p forge-runtime --features testcontainers
#[cfg(all(test, feature = "testcontainers"))]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::disallowed_methods
)]
// Integration tests run ad-hoc verification queries that aren't part of the
// .sqlx cache; runtime sqlx::query is fine here.
mod integration_tests {
    use super::*;
    use forge_core::testing::{IsolatedTestDb, TestDatabase};

    async fn setup_db(test_name: &str) -> IsolatedTestDb {
        let base = TestDatabase::from_env()
            .await
            .expect("Failed to create test database");
        let db = base
            .isolated(test_name)
            .await
            .expect("Failed to create isolated db");
        let system_sql = crate::pg::migration::get_all_system_sql();
        db.run_sql(&system_sql)
            .await
            .expect("Failed to apply system schema");
        db
    }

    #[tokio::test]
    async fn enqueue_and_claim_job() {
        let db = setup_db("enqueue_and_claim").await;
        let queue = JobQueue::new(db.pool().clone());
        let worker_id = Uuid::new_v4();

        // Enqueue a job
        let job = JobRecord::new(
            "send_email",
            serde_json::json!({"to": "a@b.com"}),
            JobPriority::Normal,
            3,
        );
        let job_id = queue.enqueue(job).await.expect("Failed to enqueue");

        // Claim it
        let claimed = queue
            .claim(worker_id, &[], 10)
            .await
            .expect("Failed to claim");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].id, job_id);
        assert_eq!(claimed[0].job_type, "send_email");
        assert_eq!(claimed[0].status, JobStatus::Claimed);
        assert_eq!(claimed[0].attempts, 1);
        assert!(claimed[0].worker_id.is_some());

        db.cleanup().await.expect("cleanup");
    }

    #[tokio::test]
    async fn claim_respects_skip_locked() {
        let db = setup_db("claim_skip_locked").await;
        let queue = JobQueue::new(db.pool().clone());

        // Enqueue 3 jobs
        for i in 0..3 {
            let job = JobRecord::new(
                format!("job_{i}"),
                serde_json::json!({}),
                JobPriority::Normal,
                3,
            );
            queue.enqueue(job).await.expect("enqueue");
        }

        // Worker 1 claims 2
        let worker1 = Uuid::new_v4();
        let batch1 = queue.claim(worker1, &[], 2).await.expect("claim1");
        assert_eq!(batch1.len(), 2);

        // Worker 2 claims remaining
        let worker2 = Uuid::new_v4();
        let batch2 = queue.claim(worker2, &[], 2).await.expect("claim2");
        assert_eq!(batch2.len(), 1);

        // No overlap
        let ids1: Vec<Uuid> = batch1.iter().map(|j| j.id).collect();
        let ids2: Vec<Uuid> = batch2.iter().map(|j| j.id).collect();
        for id in &ids2 {
            assert!(
                !ids1.contains(id),
                "SKIP LOCKED should prevent duplicate claims"
            );
        }

        db.cleanup().await.expect("cleanup");
    }

    #[tokio::test]
    async fn claim_respects_priority_ordering() {
        let db = setup_db("claim_priority").await;
        let queue = JobQueue::new(db.pool().clone());
        let worker_id = Uuid::new_v4();

        // Enqueue low then high priority
        let low = JobRecord::new("low_job", serde_json::json!({}), JobPriority::Low, 3);
        queue.enqueue(low).await.expect("enqueue low");

        let high = JobRecord::new("high_job", serde_json::json!({}), JobPriority::Critical, 3);
        queue.enqueue(high).await.expect("enqueue high");

        // Claim 1 - should get the high-priority job first
        let claimed = queue.claim(worker_id, &[], 1).await.expect("claim");
        assert_eq!(claimed.len(), 1);
        assert_eq!(claimed[0].job_type, "high_job");

        db.cleanup().await.expect("cleanup");
    }

    #[tokio::test]
    async fn complete_job_lifecycle() {
        let db = setup_db("complete_lifecycle").await;
        let queue = JobQueue::new(db.pool().clone());
        let worker_id = Uuid::new_v4();

        let job = JobRecord::new("process", serde_json::json!({}), JobPriority::Normal, 3);
        let job_id = queue.enqueue(job).await.expect("enqueue");

        // Claim
        queue.claim(worker_id, &[], 1).await.expect("claim");

        // Start
        queue.start(job_id, worker_id, 1).await.expect("start");

        // Complete
        queue
            .complete(job_id, serde_json::json!({"result": "done"}), None)
            .await
            .expect("complete");

        // Verify via stats
        let stats = queue.stats().await.expect("stats");
        assert_eq!(stats.completed, 1);
        assert_eq!(stats.pending, 0);

        db.cleanup().await.expect("cleanup");
    }

    #[tokio::test]
    async fn fail_with_retry_requeues_as_pending() {
        let db = setup_db("fail_retry").await;
        let queue = JobQueue::new(db.pool().clone());
        let worker_id = Uuid::new_v4();

        let job = JobRecord::new("flaky", serde_json::json!({}), JobPriority::Normal, 3);
        let job_id = queue.enqueue(job).await.expect("enqueue");

        queue.claim(worker_id, &[], 1).await.expect("claim");
        queue.start(job_id, worker_id, 1).await.expect("start");

        // Fail with retry delay
        queue
            .fail(
                job_id,
                "transient error",
                Some(chrono::Duration::seconds(0)),
                None,
            )
            .await
            .expect("fail");

        // Should be pending again (retryable)
        let stats = queue.stats().await.expect("stats");
        assert_eq!(stats.pending, 1);
        assert_eq!(stats.dead_letter, 0);

        db.cleanup().await.expect("cleanup");
    }

    #[tokio::test]
    async fn fail_without_retry_goes_to_dead_letter() {
        let db = setup_db("fail_dead_letter").await;
        let queue = JobQueue::new(db.pool().clone());
        let worker_id = Uuid::new_v4();

        let job = JobRecord::new("fatal", serde_json::json!({}), JobPriority::Normal, 1);
        let job_id = queue.enqueue(job).await.expect("enqueue");

        queue.claim(worker_id, &[], 1).await.expect("claim");
        queue.start(job_id, worker_id, 1).await.expect("start");

        // Fail without retry (None delay) -> dead letter
        queue
            .fail(job_id, "permanent error", None, None)
            .await
            .expect("fail");

        let stats = queue.stats().await.expect("stats");
        assert_eq!(stats.dead_letter, 1);
        assert_eq!(stats.pending, 0);

        db.cleanup().await.expect("cleanup");
    }

    #[tokio::test]
    async fn idempotency_key_deduplicates() {
        let db = setup_db("idempotency").await;
        let queue = JobQueue::new(db.pool().clone());

        let job1 = JobRecord::new("pay", serde_json::json!({}), JobPriority::Normal, 3)
            .with_idempotency_key("pay-123");
        let id1 = queue.enqueue(job1).await.expect("enqueue1");

        // Same key -> returns existing job ID
        let job2 = JobRecord::new(
            "pay",
            serde_json::json!({"amount": 200}),
            JobPriority::Normal,
            3,
        )
        .with_idempotency_key("pay-123");
        let id2 = queue.enqueue(job2).await.expect("enqueue2");

        assert_eq!(id1, id2, "Idempotency key should return same job ID");

        // Only one job should exist
        let stats = queue.stats().await.expect("stats");
        assert_eq!(stats.pending, 1);

        db.cleanup().await.expect("cleanup");
    }

    #[tokio::test]
    async fn cancel_pending_job() {
        let db = setup_db("cancel_pending").await;
        let queue = JobQueue::new(db.pool().clone());

        let job = JobRecord::new("task", serde_json::json!({}), JobPriority::Normal, 3);
        let job_id = queue.enqueue(job).await.expect("enqueue");

        let cancelled = queue
            .request_cancel(job_id, Some("no longer needed"), None)
            .await
            .expect("cancel");
        assert!(cancelled);

        let stats = queue.stats().await.expect("stats");
        assert_eq!(stats.cancelled, 1);
        assert_eq!(stats.pending, 0);

        db.cleanup().await.expect("cleanup");
    }

    #[tokio::test]
    async fn cancel_respects_ownership() {
        let db = setup_db("cancel_ownership").await;
        let queue = JobQueue::new(db.pool().clone());

        let job = JobRecord::new("task", serde_json::json!({}), JobPriority::Normal, 3)
            .with_owner_subject(Some("user-alice".into()));
        let job_id = queue.enqueue(job).await.expect("enqueue");

        // Wrong owner can't cancel
        let denied = queue
            .request_cancel(job_id, Some("reason"), Some("user-bob"))
            .await
            .expect("cancel attempt");
        assert!(!denied, "Should deny cancellation from non-owner");

        // Right owner can cancel
        let allowed = queue
            .request_cancel(job_id, Some("reason"), Some("user-alice"))
            .await
            .expect("cancel");
        assert!(allowed, "Should allow owner to cancel");

        db.cleanup().await.expect("cleanup");
    }

    #[tokio::test]
    async fn claim_respects_worker_capability() {
        let db = setup_db("claim_capability").await;
        let queue = JobQueue::new(db.pool().clone());

        // Job requires "gpu" capability
        let job = JobRecord::new("render", serde_json::json!({}), JobPriority::Normal, 3)
            .with_capability("gpu");
        queue.enqueue(job).await.expect("enqueue");

        // Worker without capability can't claim
        let worker_no_cap = Uuid::new_v4();
        let claimed = queue
            .claim(worker_no_cap, &["cpu".into()], 10)
            .await
            .expect("claim");
        assert!(
            claimed.is_empty(),
            "Worker without gpu cap should not claim gpu job"
        );

        // Worker with capability can claim
        let worker_with_cap = Uuid::new_v4();
        let claimed = queue
            .claim(worker_with_cap, &["gpu".into()], 10)
            .await
            .expect("claim");
        assert_eq!(claimed.len(), 1);

        db.cleanup().await.expect("cleanup");
    }

    #[tokio::test]
    async fn heartbeat_updates_timestamp() {
        let db = setup_db("heartbeat").await;
        let queue = JobQueue::new(db.pool().clone());
        let worker_id = Uuid::new_v4();

        let job = JobRecord::new("long_task", serde_json::json!({}), JobPriority::Normal, 3);
        let job_id = queue.enqueue(job).await.expect("enqueue");
        queue.claim(worker_id, &[], 1).await.expect("claim");
        queue.start(job_id, worker_id, 1).await.expect("start");

        // Heartbeat should not error
        queue.heartbeat(job_id).await.expect("heartbeat");

        db.cleanup().await.expect("cleanup");
    }

    #[tokio::test]
    async fn progress_updates_persist() {
        let db = setup_db("progress").await;
        let queue = JobQueue::new(db.pool().clone());
        let worker_id = Uuid::new_v4();

        let job = JobRecord::new("export", serde_json::json!({}), JobPriority::Normal, 3);
        let job_id = queue.enqueue(job).await.expect("enqueue");
        queue.claim(worker_id, &[], 1).await.expect("claim");
        queue.start(job_id, worker_id, 1).await.expect("start");

        queue
            .update_progress(job_id, 50, "Processing...")
            .await
            .expect("progress");
        queue
            .update_progress(job_id, 100, "Done")
            .await
            .expect("progress");

        // Verify via direct query (using runtime query since .sqlx/ lacks metadata for ad-hoc queries)
        let row: (Option<i32>, Option<String>) = sqlx::query_as(
            "SELECT progress_percent, progress_message FROM forge_jobs WHERE id = $1",
        )
        .bind(job_id)
        .fetch_one(db.pool())
        .await
        .expect("query");
        assert_eq!(row.0, Some(100));
        assert_eq!(row.1.as_deref(), Some("Done"));

        db.cleanup().await.expect("cleanup");
    }

    #[tokio::test]
    async fn queue_stats_accurate() {
        let db = setup_db("stats").await;
        let queue = JobQueue::new(db.pool().clone());

        // Start empty
        let stats = queue.stats().await.expect("stats");
        assert_eq!(stats.pending, 0);

        // Add 3 pending
        for _ in 0..3 {
            let job = JobRecord::new("task", serde_json::json!({}), JobPriority::Normal, 3);
            queue.enqueue(job).await.expect("enqueue");
        }

        let stats = queue.stats().await.expect("stats");
        assert_eq!(stats.pending, 3);
        assert_eq!(stats.running, 0);
        assert_eq!(stats.completed, 0);

        db.cleanup().await.expect("cleanup");
    }
}

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
            status: JobStatus::Pending,
            priority: priority.as_i32(),
            attempts: 0,
            max_attempts,
            last_error: None,
            worker_capability: None,
            worker_id: None,
            idempotency_key: None,
            scheduled_at: Utc::now(),
            created_at: Utc::now(),
            claimed_at: None,
            started_at: None,
            completed_at: None,
            failed_at: None,
            last_heartbeat: None,
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
}

/// Job queue operations.
#[derive(Clone)]
pub struct JobQueue {
    pool: sqlx::PgPool,
}

impl JobQueue {
    /// Create a new job queue.
    pub fn new(pool: sqlx::PgPool) -> Self {
        Self { pool }
    }

    /// Enqueue a new job.
    pub async fn enqueue(&self, job: JobRecord) -> Result<Uuid, sqlx::Error> {
        // Check for duplicate if idempotency key is set
        if let Some(ref key) = job.idempotency_key {
            let existing: Option<(Uuid,)> = sqlx::query_as(
                r#"
                SELECT id FROM forge_jobs
                WHERE idempotency_key = $1
                  AND status NOT IN ('completed', 'failed', 'dead_letter')
                "#,
            )
            .bind(key)
            .fetch_optional(&self.pool)
            .await?;

            if let Some((id,)) = existing {
                return Ok(id); // Return existing job ID
            }
        }

        sqlx::query(
            r#"
            INSERT INTO forge_jobs (
                id, job_type, input, status, priority, attempts, max_attempts,
                worker_capability, idempotency_key, scheduled_at, created_at
            ) VALUES (
                $1, $2, $3, $4, $5, $6, $7, $8, $9, $10, $11
            )
            "#,
        )
        .bind(job.id)
        .bind(&job.job_type)
        .bind(&job.input)
        .bind(job.status.as_str())
        .bind(job.priority)
        .bind(job.attempts)
        .bind(job.max_attempts)
        .bind(&job.worker_capability)
        .bind(&job.idempotency_key)
        .bind(job.scheduled_at)
        .bind(job.created_at)
        .execute(&self.pool)
        .await?;

        Ok(job.id)
    }

    /// Claim jobs using SKIP LOCKED pattern.
    pub async fn claim(
        &self,
        worker_id: Uuid,
        capabilities: &[String],
        limit: i32,
    ) -> Result<Vec<JobRecord>, sqlx::Error> {
        let rows = sqlx::query(
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
                id, job_type, input, output, status, priority,
                attempts, max_attempts, last_error, worker_capability,
                worker_id, idempotency_key, scheduled_at, created_at,
                claimed_at, started_at, completed_at, failed_at, last_heartbeat
            "#,
        )
        .bind(worker_id)
        .bind(capabilities)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;

        let jobs = rows
            .iter()
            .map(|row| {
                use sqlx::Row;
                JobRecord {
                    id: row.get("id"),
                    job_type: row.get("job_type"),
                    input: row.get("input"),
                    output: row.get("output"),
                    status: row.get::<String, _>("status").parse().unwrap(),
                    priority: row.get("priority"),
                    attempts: row.get("attempts"),
                    max_attempts: row.get("max_attempts"),
                    last_error: row.get("last_error"),
                    worker_capability: row.get("worker_capability"),
                    worker_id: row.get("worker_id"),
                    idempotency_key: row.get("idempotency_key"),
                    scheduled_at: row.get("scheduled_at"),
                    created_at: row.get("created_at"),
                    claimed_at: row.get("claimed_at"),
                    started_at: row.get("started_at"),
                    completed_at: row.get("completed_at"),
                    failed_at: row.get("failed_at"),
                    last_heartbeat: row.get("last_heartbeat"),
                }
            })
            .collect();

        Ok(jobs)
    }

    /// Mark job as running.
    pub async fn start(&self, job_id: Uuid) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            UPDATE forge_jobs
            SET status = 'running', started_at = NOW()
            WHERE id = $1
            "#,
        )
        .bind(job_id)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Mark job as completed.
    pub async fn complete(
        &self,
        job_id: Uuid,
        output: serde_json::Value,
    ) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            UPDATE forge_jobs
            SET
                status = 'completed',
                output = $2,
                completed_at = NOW()
            WHERE id = $1
            "#,
        )
        .bind(job_id)
        .bind(output)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Mark job as failed, schedule retry or move to dead letter.
    pub async fn fail(
        &self,
        job_id: Uuid,
        error: &str,
        retry_delay: Option<chrono::Duration>,
    ) -> Result<(), sqlx::Error> {
        if let Some(delay) = retry_delay {
            // Schedule retry
            sqlx::query(
                r#"
                UPDATE forge_jobs
                SET
                    status = 'pending',
                    worker_id = NULL,
                    claimed_at = NULL,
                    started_at = NULL,
                    last_error = $2,
                    scheduled_at = NOW() + $3
                WHERE id = $1
                "#,
            )
            .bind(job_id)
            .bind(error)
            .bind(delay)
            .execute(&self.pool)
            .await?;
        } else {
            // Move to dead letter
            sqlx::query(
                r#"
                UPDATE forge_jobs
                SET
                    status = 'dead_letter',
                    last_error = $2,
                    failed_at = NOW()
                WHERE id = $1
                "#,
            )
            .bind(job_id)
            .bind(error)
            .execute(&self.pool)
            .await?;
        }

        Ok(())
    }

    /// Update heartbeat for a running job.
    pub async fn heartbeat(&self, job_id: Uuid) -> Result<(), sqlx::Error> {
        sqlx::query(
            r#"
            UPDATE forge_jobs
            SET last_heartbeat = NOW()
            WHERE id = $1
            "#,
        )
        .bind(job_id)
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
        sqlx::query(
            r#"
            UPDATE forge_jobs
            SET progress_percent = $2, progress_message = $3, last_heartbeat = NOW()
            WHERE id = $1
            "#,
        )
        .bind(job_id)
        .bind(percent)
        .bind(message)
        .execute(&self.pool)
        .await?;

        Ok(())
    }

    /// Release stale jobs back to pending.
    pub async fn release_stale(
        &self,
        stale_threshold: chrono::Duration,
    ) -> Result<u64, sqlx::Error> {
        let result = sqlx::query(
            r#"
            UPDATE forge_jobs
            SET
                status = 'pending',
                worker_id = NULL,
                claimed_at = NULL
            WHERE status IN ('claimed', 'running')
              AND claimed_at < NOW() - $1
            "#,
        )
        .bind(stale_threshold)
        .execute(&self.pool)
        .await?;

        Ok(result.rows_affected())
    }

    /// Get queue statistics.
    pub async fn stats(&self) -> Result<QueueStats, sqlx::Error> {
        let row = sqlx::query(
            r#"
            SELECT
                COUNT(*) FILTER (WHERE status = 'pending') as pending,
                COUNT(*) FILTER (WHERE status = 'claimed') as claimed,
                COUNT(*) FILTER (WHERE status = 'running') as running,
                COUNT(*) FILTER (WHERE status = 'completed') as completed,
                COUNT(*) FILTER (WHERE status = 'failed') as failed,
                COUNT(*) FILTER (WHERE status = 'dead_letter') as dead_letter
            FROM forge_jobs
            "#,
        )
        .fetch_one(&self.pool)
        .await?;

        use sqlx::Row;
        Ok(QueueStats {
            pending: row.get::<i64, _>("pending") as u64,
            claimed: row.get::<i64, _>("claimed") as u64,
            running: row.get::<i64, _>("running") as u64,
            completed: row.get::<i64, _>("completed") as u64,
            failed: row.get::<i64, _>("failed") as u64,
            dead_letter: row.get::<i64, _>("dead_letter") as u64,
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
}

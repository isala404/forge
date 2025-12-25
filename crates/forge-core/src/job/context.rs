use std::sync::mpsc;

use uuid::Uuid;

use crate::function::AuthContext;

/// Context available to job handlers.
pub struct JobContext {
    /// Job ID.
    pub job_id: Uuid,
    /// Job type/name.
    pub job_type: String,
    /// Current attempt number (1-based).
    pub attempt: u32,
    /// Maximum attempts allowed.
    pub max_attempts: u32,
    /// Authentication context (for queries/mutations).
    pub auth: AuthContext,
    /// Database pool.
    db_pool: sqlx::PgPool,
    /// HTTP client for external calls.
    http_client: reqwest::Client,
    /// Progress reporter (sync channel for simplicity).
    progress_tx: Option<mpsc::Sender<ProgressUpdate>>,
}

/// Progress update message.
#[derive(Debug, Clone)]
pub struct ProgressUpdate {
    /// Job ID.
    pub job_id: Uuid,
    /// Progress percentage (0-100).
    pub percentage: u8,
    /// Status message.
    pub message: String,
}

impl JobContext {
    /// Create a new job context.
    pub fn new(
        job_id: Uuid,
        job_type: String,
        attempt: u32,
        max_attempts: u32,
        db_pool: sqlx::PgPool,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            job_id,
            job_type,
            attempt,
            max_attempts,
            auth: AuthContext::unauthenticated(),
            db_pool,
            http_client,
            progress_tx: None,
        }
    }

    /// Set authentication context.
    pub fn with_auth(mut self, auth: AuthContext) -> Self {
        self.auth = auth;
        self
    }

    /// Set progress channel.
    pub fn with_progress(mut self, tx: mpsc::Sender<ProgressUpdate>) -> Self {
        self.progress_tx = Some(tx);
        self
    }

    /// Get database pool.
    pub fn db(&self) -> &sqlx::PgPool {
        &self.db_pool
    }

    /// Get HTTP client.
    pub fn http(&self) -> &reqwest::Client {
        &self.http_client
    }

    /// Report job progress.
    pub fn progress(&self, percentage: u8, message: impl Into<String>) -> crate::Result<()> {
        let update = ProgressUpdate {
            job_id: self.job_id,
            percentage: percentage.min(100),
            message: message.into(),
        };

        if let Some(ref tx) = self.progress_tx {
            tx.send(update)
                .map_err(|e| crate::ForgeError::Job(format!("Failed to send progress: {}", e)))?;
        }

        Ok(())
    }

    /// Send heartbeat to keep job alive (async).
    pub async fn heartbeat(&self) -> crate::Result<()> {
        sqlx::query(
            r#"
            UPDATE forge_jobs
            SET last_heartbeat = NOW()
            WHERE id = $1
            "#,
        )
        .bind(self.job_id)
        .execute(&self.db_pool)
        .await
        .map_err(|e| crate::ForgeError::Database(e.to_string()))?;

        Ok(())
    }

    /// Check if this is a retry attempt.
    pub fn is_retry(&self) -> bool {
        self.attempt > 1
    }

    /// Check if this is the last attempt.
    pub fn is_last_attempt(&self) -> bool {
        self.attempt >= self.max_attempts
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_job_context_creation() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("Failed to create mock pool");

        let job_id = Uuid::new_v4();
        let ctx = JobContext::new(
            job_id,
            "test_job".to_string(),
            1,
            3,
            pool,
            reqwest::Client::new(),
        );

        assert_eq!(ctx.job_id, job_id);
        assert_eq!(ctx.job_type, "test_job");
        assert_eq!(ctx.attempt, 1);
        assert_eq!(ctx.max_attempts, 3);
        assert!(!ctx.is_retry());
        assert!(!ctx.is_last_attempt());
    }

    #[tokio::test]
    async fn test_is_retry() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("Failed to create mock pool");

        let ctx = JobContext::new(
            Uuid::new_v4(),
            "test".to_string(),
            2,
            3,
            pool,
            reqwest::Client::new(),
        );

        assert!(ctx.is_retry());
    }

    #[tokio::test]
    async fn test_is_last_attempt() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("Failed to create mock pool");

        let ctx = JobContext::new(
            Uuid::new_v4(),
            "test".to_string(),
            3,
            3,
            pool,
            reqwest::Client::new(),
        );

        assert!(ctx.is_last_attempt());
    }

    #[test]
    fn test_progress_update() {
        let update = ProgressUpdate {
            job_id: Uuid::new_v4(),
            percentage: 50,
            message: "Halfway there".to_string(),
        };

        assert_eq!(update.percentage, 50);
        assert_eq!(update.message, "Halfway there");
    }
}

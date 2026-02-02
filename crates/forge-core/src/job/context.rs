use std::sync::{Arc, mpsc};

use uuid::Uuid;

use crate::env::{EnvAccess, EnvProvider, RealEnvProvider};
use crate::function::AuthContext;

pub fn empty_context_value() -> serde_json::Value {
    serde_json::Value::Object(serde_json::Map::new())
}

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
    /// Persisted job context data.
    context: Arc<tokio::sync::RwLock<serde_json::Value>>,
    /// Database pool.
    db_pool: sqlx::PgPool,
    /// HTTP client for external calls.
    http_client: reqwest::Client,
    /// Progress reporter (sync channel for simplicity).
    progress_tx: Option<mpsc::Sender<ProgressUpdate>>,
    /// Environment variable provider.
    env_provider: Arc<dyn EnvProvider>,
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
            context: Arc::new(tokio::sync::RwLock::new(empty_context_value())),
            db_pool,
            http_client,
            progress_tx: None,
            env_provider: Arc::new(RealEnvProvider::new()),
        }
    }

    /// Create a new job context with persisted context data.
    pub fn with_context(mut self, context: serde_json::Value) -> Self {
        self.context = Arc::new(tokio::sync::RwLock::new(context));
        self
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

    /// Set environment provider.
    pub fn with_env_provider(mut self, provider: Arc<dyn EnvProvider>) -> Self {
        self.env_provider = provider;
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

    /// Get the current persisted job context.
    pub async fn context(&self) -> serde_json::Value {
        self.context.read().await.clone()
    }

    /// Replace the entire persisted job context.
    pub async fn set_context(&self, context: serde_json::Value) -> crate::Result<()> {
        let mut guard = self.context.write().await;
        *guard = context;
        let persisted_context = Self::clone_and_drop(guard);
        if self.job_id.is_nil() {
            return Ok(());
        }
        self.persist_context_value(persisted_context).await
    }

    /// Update a single context key with a JSON value.
    pub async fn update_context(
        &self,
        key: &str,
        value: serde_json::Value,
    ) -> crate::Result<()> {
        let mut guard = self.context.write().await;
        Self::apply_context_update(&mut guard, key, value);
        let persisted_context = Self::clone_and_drop(guard);
        if self.job_id.is_nil() {
            return Ok(());
        }
        self.persist_context_value(persisted_context).await
    }

    /// Check if cancellation has been requested for this job.
    pub async fn is_cancel_requested(&self) -> crate::Result<bool> {
        let row: Option<(String,)> = sqlx::query_as(
            r#"
            SELECT status
            FROM forge_jobs
            WHERE id = $1
            "#,
        )
        .bind(self.job_id)
        .fetch_optional(&self.db_pool)
        .await
        .map_err(|e| crate::ForgeError::Database(e.to_string()))?;

        Ok(matches!(
            row.as_ref().map(|(status,)| status.as_str()),
            Some("cancel_requested") | Some("cancelled")
        ))
    }

    /// Return an error if cancellation has been requested.
    pub async fn check_cancelled(&self) -> crate::Result<()> {
        if self.is_cancel_requested().await? {
            Err(crate::ForgeError::JobCancelled(
                "Job cancellation requested".to_string(),
            ))
        } else {
            Ok(())
        }
    }

    async fn persist_context_value(&self, context: serde_json::Value) -> crate::Result<()> {
        sqlx::query(
            r#"
            UPDATE forge_jobs
            SET job_context = $2
            WHERE id = $1
            "#,
        )
        .bind(self.job_id)
        .bind(context)
        .execute(&self.db_pool)
        .await
        .map_err(|e| crate::ForgeError::Database(e.to_string()))?;

        Ok(())
    }

    fn apply_context_update(
        context: &mut serde_json::Value,
        key: &str,
        value: serde_json::Value,
    ) {
        if let Some(map) = context.as_object_mut() {
            map.insert(key.to_string(), value);
        } else {
            let mut map = serde_json::Map::new();
            map.insert(key.to_string(), value);
            *context = serde_json::Value::Object(map);
        }
    }

    fn clone_and_drop(
        guard: tokio::sync::RwLockWriteGuard<'_, serde_json::Value>,
    ) -> serde_json::Value {
        let cloned = guard.clone();
        drop(guard);
        cloned
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

impl EnvAccess for JobContext {
    fn env_provider(&self) -> &dyn EnvProvider {
        self.env_provider.as_ref()
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

    #[tokio::test]
    async fn test_context_update_in_memory() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("Failed to create mock pool");

        let ctx = JobContext::new(
            Uuid::nil(),
            "test_job".to_string(),
            1,
            3,
            pool,
            reqwest::Client::new(),
        )
        .with_context(serde_json::json!({"foo": "bar"}));

        let context = ctx.context().await;
        assert_eq!(context["foo"], "bar");
    }
}

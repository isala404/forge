use chrono::{DateTime, Utc};
use uuid::Uuid;

use crate::function::AuthContext;

/// Context available to cron handlers.
pub struct CronContext {
    /// Cron run ID.
    pub run_id: Uuid,
    /// Cron name.
    pub cron_name: String,
    /// Scheduled time (when the cron was supposed to run).
    pub scheduled_time: DateTime<Utc>,
    /// Actual execution time.
    pub execution_time: DateTime<Utc>,
    /// Timezone of the cron.
    pub timezone: String,
    /// Whether this is a catch-up run.
    pub is_catch_up: bool,
    /// Authentication context.
    pub auth: AuthContext,
    /// Database pool.
    db_pool: sqlx::PgPool,
    /// HTTP client.
    http_client: reqwest::Client,
    /// Structured logger.
    pub log: CronLog,
}

impl CronContext {
    /// Create a new cron context.
    pub fn new(
        run_id: Uuid,
        cron_name: String,
        scheduled_time: DateTime<Utc>,
        timezone: String,
        is_catch_up: bool,
        db_pool: sqlx::PgPool,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            run_id,
            cron_name: cron_name.clone(),
            scheduled_time,
            execution_time: Utc::now(),
            timezone,
            is_catch_up,
            auth: AuthContext::unauthenticated(),
            db_pool,
            http_client,
            log: CronLog::new(cron_name),
        }
    }

    /// Get the database pool.
    pub fn db(&self) -> &sqlx::PgPool {
        &self.db_pool
    }

    /// Get the HTTP client.
    pub fn http(&self) -> &reqwest::Client {
        &self.http_client
    }

    /// Get the delay between scheduled and actual execution time.
    pub fn delay(&self) -> chrono::Duration {
        self.execution_time - self.scheduled_time
    }

    /// Check if the cron is running late (more than 1 minute delay).
    pub fn is_late(&self) -> bool {
        self.delay() > chrono::Duration::minutes(1)
    }

    /// Set authentication context.
    pub fn with_auth(mut self, auth: AuthContext) -> Self {
        self.auth = auth;
        self
    }
}

/// Structured logger for cron jobs.
#[derive(Clone)]
pub struct CronLog {
    cron_name: String,
}

impl CronLog {
    /// Create a new cron logger.
    pub fn new(cron_name: String) -> Self {
        Self { cron_name }
    }

    /// Log an info message.
    pub fn info(&self, message: &str, data: serde_json::Value) {
        tracing::info!(
            cron_name = %self.cron_name,
            data = %data,
            "{}",
            message
        );
    }

    /// Log a warning message.
    pub fn warn(&self, message: &str, data: serde_json::Value) {
        tracing::warn!(
            cron_name = %self.cron_name,
            data = %data,
            "{}",
            message
        );
    }

    /// Log an error message.
    pub fn error(&self, message: &str, data: serde_json::Value) {
        tracing::error!(
            cron_name = %self.cron_name,
            data = %data,
            "{}",
            message
        );
    }

    /// Log a debug message.
    pub fn debug(&self, message: &str, data: serde_json::Value) {
        tracing::debug!(
            cron_name = %self.cron_name,
            data = %data,
            "{}",
            message
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_cron_context_creation() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("Failed to create mock pool");

        let run_id = Uuid::new_v4();
        let scheduled = Utc::now() - chrono::Duration::seconds(30);

        let ctx = CronContext::new(
            run_id,
            "test_cron".to_string(),
            scheduled,
            "UTC".to_string(),
            false,
            pool,
            reqwest::Client::new(),
        );

        assert_eq!(ctx.run_id, run_id);
        assert_eq!(ctx.cron_name, "test_cron");
        assert!(!ctx.is_catch_up);
    }

    #[tokio::test]
    async fn test_cron_delay() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/nonexistent")
            .expect("Failed to create mock pool");

        let scheduled = Utc::now() - chrono::Duration::minutes(5);

        let ctx = CronContext::new(
            Uuid::new_v4(),
            "test_cron".to_string(),
            scheduled,
            "UTC".to_string(),
            false,
            pool,
            reqwest::Client::new(),
        );

        assert!(ctx.is_late());
        assert!(ctx.delay() >= chrono::Duration::minutes(5));
    }

    #[test]
    fn test_cron_log() {
        let log = CronLog::new("test_cron".to_string());
        log.info("Test message", serde_json::json!({"key": "value"}));
    }
}

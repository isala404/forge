use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use uuid::Uuid;

use super::registry::CronRegistry;
use forge_core::cron::CronContext;

/// Cron run status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CronStatus {
    /// Pending execution.
    Pending,
    /// Currently running.
    Running,
    /// Completed successfully.
    Completed,
    /// Failed with error.
    Failed,
}

impl CronStatus {
    /// Convert to string for database storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
        }
    }

    /// Parse from string.
    pub fn from_str(s: &str) -> Self {
        match s {
            "pending" => Self::Pending,
            "running" => Self::Running,
            "completed" => Self::Completed,
            "failed" => Self::Failed,
            _ => Self::Pending,
        }
    }
}

/// A cron run record from the database.
#[derive(Debug, Clone)]
pub struct CronRecord {
    /// Run ID.
    pub id: Uuid,
    /// Cron name.
    pub cron_name: String,
    /// Scheduled time.
    pub scheduled_time: DateTime<Utc>,
    /// Timezone.
    pub timezone: String,
    /// Current status.
    pub status: CronStatus,
    /// Node that executed the cron.
    pub node_id: Option<Uuid>,
    /// When execution started.
    pub started_at: Option<DateTime<Utc>>,
    /// When execution completed.
    pub completed_at: Option<DateTime<Utc>>,
    /// Error message if failed.
    pub error: Option<String>,
}

impl CronRecord {
    /// Create a new pending cron record.
    pub fn new(
        cron_name: impl Into<String>,
        scheduled_time: DateTime<Utc>,
        timezone: impl Into<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            cron_name: cron_name.into(),
            scheduled_time,
            timezone: timezone.into(),
            status: CronStatus::Pending,
            node_id: None,
            started_at: None,
            completed_at: None,
            error: None,
        }
    }
}

/// Configuration for the cron runner.
#[derive(Debug, Clone)]
pub struct CronRunnerConfig {
    /// How often to check for due crons.
    pub poll_interval: Duration,
    /// Node ID for this runner.
    pub node_id: Uuid,
    /// Whether this node is the leader (only leaders run crons).
    pub is_leader: bool,
}

impl Default for CronRunnerConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
            node_id: Uuid::new_v4(),
            is_leader: true,
        }
    }
}

/// Cron scheduler and executor.
pub struct CronRunner {
    registry: Arc<CronRegistry>,
    pool: sqlx::PgPool,
    http_client: reqwest::Client,
    config: CronRunnerConfig,
    is_running: Arc<RwLock<bool>>,
}

impl CronRunner {
    /// Create a new cron runner.
    pub fn new(
        registry: Arc<CronRegistry>,
        pool: sqlx::PgPool,
        http_client: reqwest::Client,
        config: CronRunnerConfig,
    ) -> Self {
        Self {
            registry,
            pool,
            http_client,
            config,
            is_running: Arc::new(RwLock::new(false)),
        }
    }

    /// Start the cron runner loop.
    pub async fn run(&self) -> forge_core::Result<()> {
        {
            let mut running = self.is_running.write().await;
            if *running {
                return Ok(());
            }
            *running = true;
        }

        tracing::info!("Cron runner starting");

        loop {
            if !*self.is_running.read().await {
                break;
            }

            if self.config.is_leader {
                if let Err(e) = self.tick().await {
                    tracing::error!(error = %e, "Cron tick failed");
                }
            }

            tokio::time::sleep(self.config.poll_interval).await;
        }

        tracing::info!("Cron runner stopped");
        Ok(())
    }

    /// Stop the cron runner.
    pub async fn stop(&self) {
        let mut running = self.is_running.write().await;
        *running = false;
    }

    /// Execute one tick of the scheduler.
    async fn tick(&self) -> forge_core::Result<()> {
        let now = Utc::now();

        for entry in self.registry.list() {
            let info = &entry.info;

            // Get next scheduled time
            let next_time = info.schedule.next_after_in_tz(now, info.timezone);

            if let Some(scheduled) = next_time {
                // Check if this time is due (within poll interval)
                if scheduled <= now {
                    // Try to claim this cron run
                    if let Ok(claimed) = self.try_claim(info.name, scheduled, info.timezone).await {
                        if claimed {
                            // Execute the cron
                            self.execute_cron(entry, scheduled, false).await;
                        }
                    }
                }
            }

            // Handle catch-up if enabled
            if info.catch_up {
                if let Err(e) = self.handle_catch_up(entry).await {
                    tracing::warn!(
                        cron = info.name,
                        error = %e,
                        "Failed to process catch-up runs"
                    );
                }
            }
        }

        Ok(())
    }

    /// Try to claim a cron run (returns true if claimed successfully).
    async fn try_claim(
        &self,
        cron_name: &str,
        scheduled_time: DateTime<Utc>,
        timezone: &str,
    ) -> forge_core::Result<bool> {
        // Insert with ON CONFLICT DO NOTHING to ensure exactly-once execution
        let result = sqlx::query(
            r#"
            INSERT INTO forge_cron_runs (id, cron_name, scheduled_time, timezone, status, node_id, started_at)
            VALUES ($1, $2, $3, $4, 'running', $5, NOW())
            ON CONFLICT (cron_name, scheduled_time) DO NOTHING
            "#,
        )
        .bind(Uuid::new_v4())
        .bind(cron_name)
        .bind(scheduled_time)
        .bind(timezone)
        .bind(self.config.node_id)
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(result.rows_affected() > 0)
    }

    /// Execute a cron job.
    async fn execute_cron(
        &self,
        entry: &super::registry::CronEntry,
        scheduled_time: DateTime<Utc>,
        is_catch_up: bool,
    ) {
        let info = &entry.info;
        let run_id = Uuid::new_v4();

        tracing::info!(
            cron = info.name,
            scheduled_time = %scheduled_time,
            is_catch_up = is_catch_up,
            "Executing cron"
        );

        let ctx = CronContext::new(
            run_id,
            info.name.to_string(),
            scheduled_time,
            info.timezone.to_string(),
            is_catch_up,
            self.pool.clone(),
            self.http_client.clone(),
        );

        // Execute with timeout
        let handler = entry.handler.clone();
        let result = tokio::time::timeout(info.timeout, handler(&ctx)).await;

        match result {
            Ok(Ok(())) => {
                tracing::info!(cron = info.name, "Cron completed successfully");
                self.mark_completed(info.name, scheduled_time).await;
            }
            Ok(Err(e)) => {
                tracing::error!(cron = info.name, error = %e, "Cron failed");
                self.mark_failed(info.name, scheduled_time, &e.to_string())
                    .await;
            }
            Err(_) => {
                tracing::error!(cron = info.name, "Cron timed out");
                self.mark_failed(info.name, scheduled_time, "Execution timed out")
                    .await;
            }
        }
    }

    /// Mark a cron run as completed.
    async fn mark_completed(&self, cron_name: &str, scheduled_time: DateTime<Utc>) {
        let _ = sqlx::query(
            r#"
            UPDATE forge_cron_runs
            SET status = 'completed', completed_at = NOW()
            WHERE cron_name = $1 AND scheduled_time = $2
            "#,
        )
        .bind(cron_name)
        .bind(scheduled_time)
        .execute(&self.pool)
        .await;
    }

    /// Mark a cron run as failed.
    async fn mark_failed(&self, cron_name: &str, scheduled_time: DateTime<Utc>, error: &str) {
        let _ = sqlx::query(
            r#"
            UPDATE forge_cron_runs
            SET status = 'failed', completed_at = NOW(), error = $3
            WHERE cron_name = $1 AND scheduled_time = $2
            "#,
        )
        .bind(cron_name)
        .bind(scheduled_time)
        .bind(error)
        .execute(&self.pool)
        .await;
    }

    /// Handle catch-up for missed runs.
    async fn handle_catch_up(&self, entry: &super::registry::CronEntry) -> forge_core::Result<()> {
        let info = &entry.info;
        let now = Utc::now();

        // Find the last completed run
        let last_run: Option<(DateTime<Utc>,)> = sqlx::query_as(
            r#"
            SELECT scheduled_time
            FROM forge_cron_runs
            WHERE cron_name = $1 AND status = 'completed'
            ORDER BY scheduled_time DESC
            LIMIT 1
            "#,
        )
        .bind(info.name)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        let start_time = last_run
            .map(|(t,)| t)
            .unwrap_or(now - chrono::Duration::days(1));

        // Get all scheduled times between last run and now
        let missed_times = info.schedule.between_in_tz(start_time, now, info.timezone);

        // Limit catch-up runs
        let to_catch_up: Vec<_> = missed_times
            .into_iter()
            .take(info.catch_up_limit as usize)
            .collect();

        for scheduled in to_catch_up {
            // Try to claim and execute
            if self.try_claim(info.name, scheduled, info.timezone).await? {
                self.execute_cron(entry, scheduled, true).await;
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cron_status_conversion() {
        assert_eq!(CronStatus::Pending.as_str(), "pending");
        assert_eq!(CronStatus::Running.as_str(), "running");
        assert_eq!(CronStatus::Completed.as_str(), "completed");
        assert_eq!(CronStatus::Failed.as_str(), "failed");

        assert_eq!(CronStatus::from_str("pending"), CronStatus::Pending);
        assert_eq!(CronStatus::from_str("running"), CronStatus::Running);
        assert_eq!(CronStatus::from_str("completed"), CronStatus::Completed);
        assert_eq!(CronStatus::from_str("failed"), CronStatus::Failed);
    }

    #[test]
    fn test_cron_record_creation() {
        let record = CronRecord::new("daily_cleanup", Utc::now(), "UTC");
        assert_eq!(record.cron_name, "daily_cleanup");
        assert_eq!(record.timezone, "UTC");
        assert_eq!(record.status, CronStatus::Pending);
        assert!(record.node_id.is_none());
    }

    #[test]
    fn test_cron_runner_config_default() {
        let config = CronRunnerConfig::default();
        assert_eq!(config.poll_interval, Duration::from_secs(1));
        assert!(config.is_leader);
    }
}

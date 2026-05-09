use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use chrono::{DateTime, Utc};
use tokio::sync::RwLock;
use tracing::{Instrument, Span, field};
use uuid::Uuid;

use super::registry::CronRegistry;
use crate::pg::LeaderElection;

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
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseCronStatusError(pub String);

impl std::fmt::Display for ParseCronStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid cron status: '{}'", self.0)
    }
}

impl std::error::Error for ParseCronStatusError {}

impl FromStr for CronStatus {
    type Err = ParseCronStatusError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "pending" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            _ => Err(ParseCronStatusError(s.to_string())),
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
    /// Subject (user/service) that triggered this run, for per-tenant audit.
    /// Mirrors `forge_jobs.owner_subject`. NULL for system-scheduled runs.
    pub owner_subject: Option<String>,
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
            owner_subject: None,
        }
    }
}

/// Configuration for the cron runner.
#[derive(Clone)]
pub struct CronRunnerConfig {
    /// How often to check for due crons.
    pub poll_interval: Duration,
    /// Node ID for this runner.
    pub node_id: Uuid,
    /// Static leadership fallback when no election handle is configured.
    pub is_leader: bool,
    /// Dynamic leader election handle.
    pub leader_election: Option<Arc<LeaderElection>>,
    /// Threshold after which a running cron slot is considered stale.
    pub run_stale_threshold: Duration,
}

impl Default for CronRunnerConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
            node_id: Uuid::new_v4(),
            is_leader: true,
            leader_election: None,
            run_stale_threshold: Duration::from_secs(15 * 60),
        }
    }
}

/// Cron scheduler and executor.
///
/// The scheduler calculates run times and claims execution slots via
/// `forge_cron_runs`. Actual execution is dispatched as a `$cron:{name}`
/// job through the shared worker pool, giving crons retry, timeout,
/// and distributed execution for free.
pub struct CronRunner {
    registry: Arc<CronRegistry>,
    pool: sqlx::PgPool,
    job_queue: crate::jobs::JobQueue,
    config: CronRunnerConfig,
    is_running: Arc<RwLock<bool>>,
}

impl CronRunner {
    /// Create a new cron runner.
    pub fn new(
        registry: Arc<CronRegistry>,
        pool: sqlx::PgPool,
        job_queue: crate::jobs::JobQueue,
        config: CronRunnerConfig,
    ) -> Self {
        Self {
            registry,
            pool,
            job_queue,
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

        tracing::debug!("Cron runner starting");

        loop {
            if !*self.is_running.read().await {
                break;
            }

            if self.is_leader()
                && let Err(e) = self.tick().await
            {
                tracing::warn!(error = %e, "Cron tick failed");
            }

            tokio::time::sleep(self.config.poll_interval).await;
        }

        tracing::debug!("Cron runner stopped");
        Ok(())
    }

    /// Stop the cron runner.
    pub async fn stop(&self) {
        let mut running = self.is_running.write().await;
        *running = false;
    }

    fn is_leader(&self) -> bool {
        self.config
            .leader_election
            .as_ref()
            .map(|e| e.is_leader())
            .unwrap_or(self.config.is_leader)
    }

    /// Execute one tick of the scheduler.
    async fn tick(&self) -> forge_core::Result<()> {
        let tick_span = tracing::info_span!(
            "cron.tick",
            cron.tick_id = %Uuid::new_v4(),
            cron.jobs_checked = field::Empty,
            cron.jobs_executed = field::Empty,
        );

        async {
            let now = Utc::now();
            // Look back 2x poll interval to catch any scheduled times we might have missed
            let window_start = now
                - chrono::Duration::from_std(self.config.poll_interval * 2)
                    .unwrap_or(chrono::Duration::seconds(2));

            let cron_list = self.registry.list();
            let mut jobs_executed = 0u32;

            Span::current().record("cron.jobs_checked", cron_list.len());

            if cron_list.is_empty() {
                tracing::trace!("Cron tick: no crons registered");
            } else {
                tracing::trace!(
                    cron_count = cron_list.len(),
                    "Cron tick checking {} registered crons",
                    cron_list.len()
                );
            }

            for entry in cron_list {
                let info = &entry.info;

                let scheduled_times = info
                    .schedule
                    .between_in_tz(window_start, now, info.timezone);

                // Record missed runs that we found
                if scheduled_times.len() > 1 {
                    tracing::info!(
                        cron.name = info.name,
                        cron.missed_count = scheduled_times.len() - 1,
                        "Detected missed cron runs"
                    );
                    Span::current().record("cron.missed_runs", scheduled_times.len() - 1);
                }

                if !scheduled_times.is_empty() {
                    tracing::trace!(
                        cron = info.name,
                        schedule = info.schedule.expression(),
                        scheduled_count = scheduled_times.len(),
                        "Found scheduled cron runs"
                    );
                }

                for scheduled in scheduled_times {
                    // Try to claim this cron run; only claimed slots execute.
                    if let Ok(Some(run_id)) =
                        self.try_claim(info.name, scheduled, info.timezone).await
                    {
                        self.execute_cron(entry, run_id, scheduled, false).await;
                        jobs_executed += 1;
                    }
                }

                // Handle catch-up if enabled
                if info.catch_up
                    && let Err(e) = self.handle_catch_up(entry).await
                {
                    tracing::warn!(
                        cron = info.name,
                        error = %e,
                        "Failed to process catch-up runs"
                    );
                }
            }

            Span::current().record("cron.jobs_executed", jobs_executed);
            Ok(())
        }
        .instrument(tick_span)
        .await
    }

    /// Try to claim a cron run.
    ///
    /// Returns the run ID if claimed (or stale-reclaimed), otherwise None.
    /// When a leader-election handle is configured, the INSERT is fenced on
    /// the leader's current term: if a new leader has taken over since this
    /// node last acquired the lock, the INSERT silently no-ops (rows_affected
    /// = 0) and we don't execute. Single-node mode (no election handle) skips
    /// the fence.
    async fn try_claim(
        &self,
        cron_name: &str,
        scheduled_time: DateTime<Utc>,
        _timezone: &str,
    ) -> forge_core::Result<Option<Uuid>> {
        let claim_id = Uuid::new_v4();
        let stale_threshold = chrono::Duration::from_std(self.config.run_stale_threshold)
            .unwrap_or(chrono::Duration::minutes(15));

        // -1 disables the DB-side term fence; leadership is already gated by the
        // in-memory is_leader() check before tick() runs.
        let fence_term: i64 = -1;

        // Insert new run, or reclaim stale running row if previous node crashed.
        let result = sqlx::query!(
            r#"
            INSERT INTO forge_cron_runs (id, cron_name, scheduled_time, status, node_id, started_at)
            SELECT $1, $2, $3, 'running', $4, NOW()
            WHERE ($6::bigint) = -1 OR EXISTS (
                SELECT 1 FROM forge_leaders
                WHERE role = 'scheduler'
                  AND node_id = $4
                  AND term = $6
            )
            ON CONFLICT (cron_name, scheduled_time) DO UPDATE
            SET
                id = EXCLUDED.id,
                status = 'running',
                node_id = EXCLUDED.node_id,
                started_at = NOW(),
                completed_at = NULL,
                error = NULL
            WHERE forge_cron_runs.status = 'running'
              AND forge_cron_runs.started_at < NOW() - make_interval(secs => $5)
            "#,
            claim_id,
            cron_name,
            scheduled_time,
            self.config.node_id,
            stale_threshold.num_seconds() as f64,
            fence_term,
        )
        .execute(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        if result.rows_affected() > 0 {
            Ok(Some(claim_id))
        } else {
            Ok(None)
        }
    }

    /// Dispatch a cron run as a job on the shared worker pool.
    async fn execute_cron(
        &self,
        entry: &super::registry::CronEntry,
        run_id: Uuid,
        scheduled_time: DateTime<Utc>,
        is_catch_up: bool,
    ) {
        let info = &entry.info;
        let job_type = format!("$cron:{}", info.name);

        let input = serde_json::json!({
            "run_id": run_id,
            "cron_name": info.name,
            "scheduled_time": scheduled_time.to_rfc3339(),
            "timezone": info.timezone,
            "is_catch_up": is_catch_up,
        });

        let job = crate::jobs::JobRecord::new(
            job_type.clone(),
            input,
            forge_core::job::JobPriority::Normal,
            1,
        );

        match self.job_queue.enqueue(job).await {
            Ok(job_id) => {
                tracing::debug!(
                    cron.name = info.name,
                    cron.run_id = %run_id,
                    job_id = %job_id,
                    is_catch_up,
                    "Cron dispatched as job"
                );
            }
            Err(e) => {
                tracing::error!(
                    cron.name = info.name,
                    cron.run_id = %run_id,
                    error = %e,
                    "Failed to enqueue cron job"
                );
                self.mark_failed(run_id, info.name, &e.to_string()).await;
            }
        }
    }

    /// Mark a cron run as failed.
    async fn mark_failed(&self, run_id: Uuid, cron_name: &str, error: &str) {
        if let Err(e) = sqlx::query!(
            r#"
            UPDATE forge_cron_runs
            SET status = 'failed', completed_at = NOW(), error = $3
            WHERE id = $1 AND node_id = $2
            "#,
            run_id,
            self.config.node_id,
            error,
        )
        .execute(&self.pool)
        .await
        {
            tracing::error!(cron = cron_name, error = %e, "Failed to mark cron failed");
        }
    }

    /// Handle catch-up for missed runs.
    async fn handle_catch_up(&self, entry: &super::registry::CronEntry) -> forge_core::Result<()> {
        let info = &entry.info;
        let now = Utc::now();

        let catch_up_span = tracing::info_span!(
            "cron.catch_up",
            cron.name = info.name,
            cron.missed_count = field::Empty,
            cron.executed_count = field::Empty,
        );

        async {
            // Find the last completed run
            let last_run = sqlx::query_scalar!(
                r#"
                SELECT scheduled_time
                FROM forge_cron_runs
                WHERE cron_name = $1 AND status = 'completed'
                ORDER BY scheduled_time DESC
                LIMIT 1
                "#,
                info.name
            )
            .fetch_optional(&self.pool)
            .await
            .map_err(forge_core::ForgeError::Database)?;

            let start_time = last_run.unwrap_or(now - chrono::Duration::days(1));

            // Get all scheduled times between last run and now
            let missed_times = info.schedule.between_in_tz(start_time, now, info.timezone);

            // Limit catch-up runs
            let to_catch_up: Vec<_> = missed_times
                .into_iter()
                .take(info.catch_up_limit as usize)
                .collect();

            Span::current().record("cron.missed_count", to_catch_up.len());

            if !to_catch_up.is_empty() {
                tracing::info!(
                    cron.name = info.name,
                    cron.catch_up_count = to_catch_up.len(),
                    cron.catch_up_limit = info.catch_up_limit,
                    "Processing catch-up runs"
                );
            }

            let mut executed_count = 0u32;
            for scheduled in to_catch_up {
                // Try to claim and execute
                if let Some(run_id) = self.try_claim(info.name, scheduled, info.timezone).await? {
                    self.execute_cron(entry, run_id, scheduled, true).await;
                    executed_count += 1;
                }
            }

            Span::current().record("cron.executed_count", executed_count);
            Ok(())
        }
        .instrument(catch_up_span)
        .await
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

        assert_eq!("pending".parse::<CronStatus>(), Ok(CronStatus::Pending));
        assert_eq!("running".parse::<CronStatus>(), Ok(CronStatus::Running));
        assert_eq!("completed".parse::<CronStatus>(), Ok(CronStatus::Completed));
        assert_eq!("failed".parse::<CronStatus>(), Ok(CronStatus::Failed));
        assert!("invalid".parse::<CronStatus>().is_err());
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

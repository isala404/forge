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
    /// Cancelled before or during execution.
    Cancelled,
}

impl CronStatus {
    /// Convert to string for database storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
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
            "cancelled" => Ok(Self::Cancelled),
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
    /// Maximum catch-up runs enqueued per cron per tick.
    ///
    /// After a node restart, every catch-up-enabled cron may have a large
    /// backlog of missed executions. Without this limit, the scheduler would
    /// attempt to enqueue all of them in a single tick, creating a job storm.
    /// Setting a per-tick cap lets the worker pool absorb catch-up work
    /// incrementally across consecutive ticks instead of all at once.
    pub max_catch_up_per_tick: u32,
}

impl Default for CronRunnerConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
            node_id: Uuid::new_v4(),
            is_leader: true,
            leader_election: None,
            run_stale_threshold: Duration::from_secs(15 * 60),
            max_catch_up_per_tick: 5,
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

            if self.confirm_leadership_before_tick().await
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

    /// Confirm leadership immediately before executing a cron tick.
    ///
    /// The cached `is_leader` flag is updated on `lock_validate_interval`
    /// (default 1 s), which means there is a window of up to 1 s where the
    /// node has lost the advisory lock but still believes it is leader. For
    /// cron ticks that window is enough to cause split-brain execution.
    ///
    /// When a `LeaderElection` handle is present, this calls
    /// `validate_lock_held()` synchronously. That re-checks `pg_locks` on the
    /// lock-owning connection and atomically drops the `is_leader` flag if the
    /// lock is gone, eliminating the race between the validate interval and the
    /// next tick. For static-leader mode (no election handle) the check reduces
    /// to the cached flag, which is correct because static mode is intentionally
    /// single-node.
    async fn confirm_leadership_before_tick(&self) -> bool {
        match self.config.leader_election.as_ref() {
            Some(election) => {
                if let Err(e) = election.validate_lock_held().await {
                    tracing::debug!(error = %e, "Pre-tick lock validation failed; skipping tick");
                    return false;
                }
                election.is_leader()
            }
            None => self.config.is_leader,
        }
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
                    // Atomically claim and enqueue in a single transaction.
                    if let Ok(Some(_run_id)) =
                        self.try_claim_and_enqueue(entry, scheduled, false).await
                    {
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

    /// Atomically claim a cron run and enqueue its job in a single transaction.
    ///
    /// Returns the run ID if claimed (or stale-reclaimed), otherwise None.
    /// Leadership is already confirmed by `confirm_leadership_before_tick()` in
    /// `run()`, and the `(cron_name, scheduled_time)` UNIQUE constraint provides
    /// the exactly-once guarantee against concurrent claimers.
    ///
    /// The INSERT into `forge_cron_runs` and the INSERT into `forge_jobs` happen
    /// within the same transaction so a crash between the two cannot leave an
    /// orphaned cron_run row without a corresponding worker job.
    async fn try_claim_and_enqueue(
        &self,
        entry: &super::registry::CronEntry,
        scheduled_time: DateTime<Utc>,
        is_catch_up: bool,
    ) -> forge_core::Result<Option<Uuid>> {
        let info = &entry.info;
        let claim_id = Uuid::new_v4();
        let stale_threshold = chrono::Duration::from_std(self.config.run_stale_threshold)
            .unwrap_or(chrono::Duration::minutes(15));

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(forge_core::ForgeError::Database)?;

        // Fetch the stale row's ID before overwriting it so we can cancel the
        // orphaned job it spawned. NULL if no stale row exists for this slot.
        let stale_run_id = sqlx::query_scalar!(
            r#"
            SELECT id FROM forge_cron_runs
            WHERE cron_name = $1
              AND scheduled_time = $2
              AND status = 'running'
              AND started_at < NOW() - make_interval(secs => $3)
            "#,
            info.name,
            scheduled_time,
            stale_threshold.num_seconds() as f64,
        )
        .fetch_optional(&mut *tx)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        // Insert new run, or reclaim stale running row if previous node crashed.
        let result = sqlx::query!(
            r#"
            INSERT INTO forge_cron_runs (id, cron_name, scheduled_time, status, node_id, started_at)
            VALUES ($1, $2, $3, 'running', $4, NOW())
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
            info.name,
            scheduled_time,
            self.config.node_id,
            stale_threshold.num_seconds() as f64,
        )
        .execute(&mut *tx)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        if result.rows_affected() == 0 {
            // Already claimed by another node (or completed); implicit rollback
            return Ok(None);
        }

        // If we reclaimed a stale slot, cancel the orphaned job the previous
        // executor spawned. Without this, that job keeps running (or retrying)
        // alongside the new one, producing duplicate side effects.
        if let Some(old_run_id) = stale_run_id {
            let job_type_cancel = format!("$cron:{}", info.name);
            let old_run_id_str = old_run_id.to_string();
            sqlx::query!(
                r#"
                UPDATE forge_jobs
                SET
                    status = 'cancelled',
                    cancelled_at = NOW(),
                    cancel_reason = 'stale cron run reclaimed by new leader',
                    expires_at = NOW() + INTERVAL '7 days'
                WHERE job_type = $1
                  AND input->>'run_id' = $2
                  AND status NOT IN ('completed', 'failed', 'dead_letter', 'cancelled')
                "#,
                job_type_cancel,
                old_run_id_str,
            )
            .execute(&mut *tx)
            .await
            .map_err(forge_core::ForgeError::Database)?;

            tracing::info!(
                cron.name = info.name,
                cron.old_run_id = %old_run_id,
                cron.new_run_id = %claim_id,
                "Cancelled orphaned job from stale cron run"
            );
        }

        // Enqueue the cron job in the same transaction
        let job_type = format!("$cron:{}", info.name);
        let input = serde_json::json!({
            "run_id": claim_id,
            "cron_name": info.name,
            "scheduled_time": scheduled_time.to_rfc3339(),
            "timezone": info.timezone,
            "is_catch_up": is_catch_up,
        });
        let job =
            crate::jobs::JobRecord::new(job_type, input, forge_core::job::JobPriority::Normal, 3)
                .with_capability(forge_core::config::CRON_QUEUE);
        self.job_queue
            .enqueue_in_conn(&mut tx, job)
            .await
            .map_err(forge_core::ForgeError::Database)?;

        tx.commit()
            .await
            .map_err(forge_core::ForgeError::Database)?;

        tracing::debug!(
            cron.name = info.name,
            cron.run_id = %claim_id,
            is_catch_up,
            "Cron claimed and job enqueued"
        );

        Ok(Some(claim_id))
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

            // Apply two caps:
            //  1. catch_up_limit — total missed runs this cron will ever backfill
            //     (controlled by the handler author via #[cron(catch_up_limit = N)]).
            //  2. max_catch_up_per_tick — hard per-tick ceiling regardless of
            //     catch_up_limit, preventing a job storm on node restart when the
            //     backlog is large. Remaining slots are deferred to the next tick.
            let tick_limit = info
                .catch_up_limit
                .min(self.config.max_catch_up_per_tick) as usize;
            let to_catch_up: Vec<_> = missed_times.into_iter().take(tick_limit).collect();

            Span::current().record("cron.missed_count", to_catch_up.len());

            if !to_catch_up.is_empty() {
                tracing::info!(
                    cron.name = info.name,
                    cron.catch_up_count = to_catch_up.len(),
                    cron.catch_up_limit = info.catch_up_limit,
                    cron.max_catch_up_per_tick = self.config.max_catch_up_per_tick,
                    "Processing catch-up runs"
                );
            }

            let mut executed_count = 0u32;
            for scheduled in to_catch_up {
                // Atomically claim and enqueue in a single transaction.
                if self
                    .try_claim_and_enqueue(entry, scheduled, true)
                    .await?
                    .is_some()
                {
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
        assert_eq!(CronStatus::Cancelled.as_str(), "cancelled");

        assert_eq!("pending".parse::<CronStatus>(), Ok(CronStatus::Pending));
        assert_eq!("running".parse::<CronStatus>(), Ok(CronStatus::Running));
        assert_eq!("completed".parse::<CronStatus>(), Ok(CronStatus::Completed));
        assert_eq!("failed".parse::<CronStatus>(), Ok(CronStatus::Failed));
        assert_eq!("cancelled".parse::<CronStatus>(), Ok(CronStatus::Cancelled));
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

#[cfg(all(test, feature = "testcontainers"))]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::disallowed_methods
)]
mod integration_tests {
    use super::*;
    use crate::cron::registry::CronEntry;
    use forge_core::cron::{CronInfo, CronSchedule};
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

    fn make_entry(name: &'static str, expr: &str) -> CronEntry {
        CronEntry {
            info: CronInfo {
                name,
                schedule: CronSchedule::new(expr).expect("valid expression"),
                ..Default::default()
            },
            // Never invoked by the scheduler under test — the job is dispatched
            // through the worker pool by name. A no-op handler is sufficient.
            handler: Arc::new(|_ctx| Box::pin(async { Ok(()) })),
        }
    }

    fn make_runner(pool: sqlx::PgPool) -> CronRunner {
        let registry = Arc::new(CronRegistry::new());
        let job_queue = crate::jobs::JobQueue::new(pool.clone());
        CronRunner::new(registry, pool, job_queue, CronRunnerConfig::default())
    }

    async fn count_rows(pool: &sqlx::PgPool, sql: &str) -> i64 {
        sqlx::query_scalar::<_, i64>(sql)
            .fetch_one(pool)
            .await
            .unwrap()
    }

    #[tokio::test]
    async fn try_claim_and_enqueue_inserts_run_and_job_atomically() {
        let db = setup_db("cron_claim_enqueue").await;
        let pool = db.pool().clone();
        let runner = make_runner(pool.clone());
        let entry = make_entry("nightly_cleanup", "0 0 * * * *");
        let scheduled = Utc::now() - chrono::Duration::seconds(30);

        let run_id = runner
            .try_claim_and_enqueue(&entry, scheduled, false)
            .await
            .expect("claim ok")
            .expect("claimed some id");

        let runs = count_rows(
            &pool,
            "SELECT COUNT(*) FROM forge_cron_runs WHERE cron_name = 'nightly_cleanup' AND status = 'running'",
        )
        .await;
        assert_eq!(runs, 1, "should have one running cron_run");

        let jobs = count_rows(
            &pool,
            "SELECT COUNT(*) FROM forge_jobs WHERE job_type = '$cron:nightly_cleanup'",
        )
        .await;
        assert_eq!(jobs, 1, "should have one queued $cron: job");

        // The job's input encodes the run_id so the worker reconciles status.
        let input_run_id: String = sqlx::query_scalar(
            "SELECT input->>'run_id' FROM forge_jobs WHERE job_type = '$cron:nightly_cleanup'",
        )
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(input_run_id, run_id.to_string());
    }

    #[tokio::test]
    async fn try_claim_and_enqueue_is_exactly_once_for_same_slot() {
        // The (cron_name, scheduled_time) UNIQUE constraint plus the conditional
        // ON CONFLICT ... WHERE clause is the entire correctness story for
        // distributed cron: same slot claimed twice must yield exactly one job.
        let db = setup_db("cron_exactly_once").await;
        let pool = db.pool().clone();
        let runner = make_runner(pool.clone());
        let entry = make_entry("hourly", "0 0 * * * *");
        let scheduled = Utc::now();

        let first = runner
            .try_claim_and_enqueue(&entry, scheduled, false)
            .await
            .unwrap();
        let second = runner
            .try_claim_and_enqueue(&entry, scheduled, false)
            .await
            .unwrap();

        assert!(first.is_some(), "first claim must succeed");
        assert!(second.is_none(), "second claim must be rejected");

        assert_eq!(
            count_rows(&pool, "SELECT COUNT(*) FROM forge_cron_runs").await,
            1
        );
        assert_eq!(
            count_rows(&pool, "SELECT COUNT(*) FROM forge_jobs").await,
            1
        );
    }

    #[tokio::test]
    async fn try_claim_and_enqueue_reclaims_stale_running_row() {
        // If a previous node crashed mid-execution and left a 'running' row,
        // the next leader after `run_stale_threshold` must adopt the slot,
        // re-issue a job, and overwrite started_at so progress tracking restarts.
        // The orphaned job from the previous executor must be cancelled so it
        // cannot run in parallel with the fresh replacement.
        let db = setup_db("cron_stale_reclaim").await;
        let pool = db.pool().clone();
        let mut runner = make_runner(pool.clone());
        // Short threshold so we don't need to backdate by 15 minutes.
        runner.config.run_stale_threshold = Duration::from_secs(1);
        let entry = make_entry("hourly", "0 0 * * * *");
        let scheduled = Utc::now();

        let original_id = Uuid::new_v4();
        let original_node = Uuid::new_v4();
        // Backdated 'running' row that's older than the stale threshold.
        sqlx::query(
            "INSERT INTO forge_cron_runs (id, cron_name, scheduled_time, status, node_id, started_at)
             VALUES ($1, $2, $3, 'running', $4, NOW() - INTERVAL '5 seconds')",
        )
        .bind(original_id)
        .bind(entry.info.name)
        .bind(scheduled)
        .bind(original_node)
        .execute(&pool)
        .await
        .unwrap();

        // Seed the orphaned job the crashed executor would have created.
        sqlx::query(
            "INSERT INTO forge_jobs (id, job_type, queue, input, job_context, status, priority, attempts, max_attempts, scheduled_at, created_at)
             VALUES ($1, '$cron:hourly', 'cron', $2, '{}', 'pending', 50, 0, 3, NOW(), NOW())",
        )
        .bind(Uuid::new_v4())
        .bind(serde_json::json!({"run_id": original_id.to_string(), "cron_name": "hourly"}))
        .execute(&pool)
        .await
        .unwrap();

        let new_run_id = runner
            .try_claim_and_enqueue(&entry, scheduled, false)
            .await
            .unwrap()
            .expect("stale row should be reclaimed");

        assert_ne!(new_run_id, original_id, "id must be rotated");

        let row: (String, Option<Uuid>) = sqlx::query_as(
            "SELECT status, node_id FROM forge_cron_runs
             WHERE cron_name = $1 AND scheduled_time = $2",
        )
        .bind(entry.info.name)
        .bind(scheduled)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(row.0, "running");
        assert_eq!(row.1, Some(runner.config.node_id));

        // The orphaned job from the previous run must be cancelled.
        let cancelled_count = count_rows(
            &pool,
            "SELECT COUNT(*) FROM forge_jobs WHERE job_type = '$cron:hourly' AND status = 'cancelled'",
        )
        .await;
        assert_eq!(cancelled_count, 1, "orphaned job must be cancelled");

        // The reclaim must also enqueue a fresh job for the new run.
        let active_count = count_rows(
            &pool,
            "SELECT COUNT(*) FROM forge_jobs WHERE job_type = '$cron:hourly' AND status = 'pending'",
        )
        .await;
        assert_eq!(active_count, 1, "exactly one fresh $cron: job must be enqueued");
    }

    #[tokio::test]
    async fn try_claim_and_enqueue_skips_fresh_running_row() {
        // Inverse of the stale case: a fresh 'running' row is another leader's
        // live execution. Reclaiming it would duplicate work.
        let db = setup_db("cron_fresh_skip").await;
        let pool = db.pool().clone();
        let mut runner = make_runner(pool.clone());
        // Long threshold so the fresh row stays fresh for the duration of the
        // test, regardless of host clock skew.
        runner.config.run_stale_threshold = Duration::from_secs(3600);
        let entry = make_entry("hourly", "0 0 * * * *");
        let scheduled = Utc::now();

        let original_id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO forge_cron_runs (id, cron_name, scheduled_time, status, node_id, started_at)
             VALUES ($1, $2, $3, 'running', $4, NOW())",
        )
        .bind(original_id)
        .bind(entry.info.name)
        .bind(scheduled)
        .bind(Uuid::new_v4())
        .execute(&pool)
        .await
        .unwrap();

        let attempt = runner
            .try_claim_and_enqueue(&entry, scheduled, false)
            .await
            .unwrap();

        assert!(attempt.is_none(), "fresh row must not be reclaimed");
        // Existing id must not be rotated.
        let id: Uuid = sqlx::query_scalar(
            "SELECT id FROM forge_cron_runs WHERE cron_name = $1 AND scheduled_time = $2",
        )
        .bind(entry.info.name)
        .bind(scheduled)
        .fetch_one(&pool)
        .await
        .unwrap();
        assert_eq!(id, original_id);
        // And no job was enqueued (the inner INSERT runs only on successful claim).
        assert_eq!(
            count_rows(&pool, "SELECT COUNT(*) FROM forge_jobs").await,
            0
        );
    }

    #[tokio::test]
    async fn try_claim_and_enqueue_skips_completed_slot() {
        // Once a slot is 'completed', it must never be re-run by this scheduler —
        // the ON CONFLICT WHERE clause only matches status='running'.
        let db = setup_db("cron_completed_skip").await;
        let pool = db.pool().clone();
        let runner = make_runner(pool.clone());
        let entry = make_entry("hourly", "0 0 * * * *");
        let scheduled = Utc::now();

        sqlx::query(
            "INSERT INTO forge_cron_runs (id, cron_name, scheduled_time, status, completed_at)
             VALUES ($1, $2, $3, 'completed', NOW())",
        )
        .bind(Uuid::new_v4())
        .bind(entry.info.name)
        .bind(scheduled)
        .execute(&pool)
        .await
        .unwrap();

        let attempt = runner
            .try_claim_and_enqueue(&entry, scheduled, false)
            .await
            .unwrap();
        assert!(attempt.is_none(), "completed slot must not be re-claimed");
        assert_eq!(
            count_rows(&pool, "SELECT COUNT(*) FROM forge_jobs").await,
            0
        );
    }

    #[tokio::test]
    async fn handle_catch_up_caps_at_catch_up_limit() {
        // The catch_up_limit is the safety valve for a node that's been offline
        // for a long time: a daily cron offline for a week shouldn't fire 7
        // simultaneous runs. The cap protects against that thundering herd.
        let db = setup_db("cron_catch_up_limit").await;
        let pool = db.pool().clone();
        let runner = make_runner(pool.clone());
        // Every second so the catch-up window of ~5 minutes yields many
        // candidate times, deterministically exceeding the limit we set.
        let mut entry = make_entry("every_sec", "* * * * * *");
        entry.info.catch_up = true;
        entry.info.catch_up_limit = 3;

        // Seed a 'completed' run 10 seconds ago so handle_catch_up has a
        // non-default start time and finds plenty of missed slots between
        // then and now.
        sqlx::query(
            "INSERT INTO forge_cron_runs (id, cron_name, scheduled_time, status, completed_at)
             VALUES ($1, $2, NOW() - INTERVAL '10 seconds', 'completed', NOW() - INTERVAL '10 seconds')",
        )
        .bind(Uuid::new_v4())
        .bind(entry.info.name)
        .execute(&pool)
        .await
        .unwrap();

        runner.handle_catch_up(&entry).await.unwrap();

        // Total runs = 1 seed completed + at most catch_up_limit new claims.
        let running_count = count_rows(
            &pool,
            "SELECT COUNT(*) FROM forge_cron_runs WHERE status = 'running'",
        )
        .await;
        assert!(
            running_count <= 3,
            "catch_up_limit should cap new claims, got {running_count}"
        );
        assert!(
            running_count > 0,
            "should have claimed at least one missed run"
        );
        assert_eq!(
            count_rows(&pool, "SELECT COUNT(*) FROM forge_jobs").await,
            running_count,
            "one $cron: job per claimed slot"
        );
    }

    #[tokio::test]
    async fn confirm_leadership_falls_back_to_static_when_no_election_handle() {
        // The scheduler can run in single-node mode (no advisory-lock election).
        // confirm_leadership_before_tick must respect the static flag in that case.
        let registry = Arc::new(CronRegistry::new());
        let pool = setup_db("cron_static_leader").await.pool().clone();
        let job_queue = crate::jobs::JobQueue::new(pool.clone());

        let leader = CronRunner::new(
            registry.clone(),
            pool.clone(),
            job_queue.clone(),
            CronRunnerConfig {
                is_leader: true,
                leader_election: None,
                ..Default::default()
            },
        );
        let follower = CronRunner::new(
            registry,
            pool,
            job_queue,
            CronRunnerConfig {
                is_leader: false,
                leader_election: None,
                ..Default::default()
            },
        );

        assert!(leader.confirm_leadership_before_tick().await);
        assert!(!follower.confirm_leadership_before_tick().await);
    }
}

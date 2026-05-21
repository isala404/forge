use std::sync::Arc;
use std::time::Duration;

use forge_core::function::{JobDispatch, KvHandle, WorkflowDispatch};
use tokio::sync::mpsc;
use tracing::Instrument;
use uuid::Uuid;

use super::executor::JobExecutor;
use super::queue::JobQueue;
use super::registry::JobRegistry;
use crate::pg::{LeaderElection, PgNotifyBus};

/// Worker configuration.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Worker ID (auto-generated if not provided).
    pub id: Option<Uuid>,
    /// Worker capabilities. Each capability is the name of a queue this
    /// worker serves; jobs are claimed only when their `worker_capability`
    /// matches one of these tags.
    pub capabilities: Vec<String>,
    /// When true, also claim jobs whose `worker_capability` is NULL. Set on
    /// the `default` queue worker so untagged user jobs run somewhere; other
    /// queues must leave it false to preserve isolation.
    pub claim_untagged: bool,
    /// Maximum concurrent jobs.
    pub max_concurrent: usize,
    /// Reserved capacity for system jobs ($workflow_resume, $cron:*).
    /// These permits are only used by system jobs, preventing user job
    /// floods from starving workflow/cron execution.
    pub system_reserved: usize,
    /// Poll interval when queue is empty.
    pub poll_interval: Duration,
    /// Batch size for claiming jobs.
    pub batch_size: i32,
    /// Stale job cleanup interval.
    pub stale_cleanup_interval: Duration,
    /// Stale job threshold.
    pub stale_threshold: chrono::Duration,
    /// Grace period to wait for in-flight jobs to finish on shutdown before
    /// aborting their tasks. Jobs that exceed this are aborted so shutdown
    /// completes deterministically — they will be reclaimed by the next
    /// startup's stale-reclaim sweep.
    pub shutdown_grace_period: Duration,
    /// Whether this worker is the leader (gates cleanup).
    pub leader_election: Option<Arc<LeaderElection>>,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            id: None,
            capabilities: vec!["default".to_string()],
            claim_untagged: true,
            max_concurrent: 8,
            system_reserved: 4,
            poll_interval: Duration::from_secs(5),
            batch_size: 10,
            stale_cleanup_interval: Duration::from_secs(60),
            stale_threshold: chrono::Duration::minutes(5),
            shutdown_grace_period: Duration::from_secs(30),
            leader_election: None,
        }
    }
}

/// Background job worker.
pub struct Worker {
    id: Uuid,
    config: WorkerConfig,
    queue: JobQueue,
    notify_bus: Arc<PgNotifyBus>,
    executor: Arc<JobExecutor>,
    shutdown_tx: Option<mpsc::Sender<()>>,
}

impl Worker {
    /// Create a new worker.
    pub fn new(
        config: WorkerConfig,
        queue: JobQueue,
        registry: JobRegistry,
        db_pool: sqlx::PgPool,
        notify_bus: Arc<PgNotifyBus>,
    ) -> Self {
        let id = config.id.unwrap_or_else(Uuid::new_v4);
        let executor = Arc::new(JobExecutor::new(queue.clone(), registry, db_pool.clone()));

        Self {
            id,
            config,
            queue,
            notify_bus,
            executor,
            shutdown_tx: None,
        }
    }

    /// Attach a KV store handle so job handlers can call `ctx.kv()`.
    pub fn with_kv(mut self, kv: Arc<dyn KvHandle>) -> Self {
        // Arc::get_mut succeeds here because `self` holds the only reference to
        // the executor at construction time (no tasks spawned yet).
        if let Some(executor) = Arc::get_mut(&mut self.executor) {
            executor.set_kv(kv);
        }
        self
    }

    /// Attach a job dispatcher so handlers can call `ctx.dispatch_job(...)`.
    /// Must be called before [`run`](Self::run) starts spawning tasks — the
    /// executor `Arc` is uniquely owned at construction and shared by `run`.
    pub fn with_job_dispatch(mut self, dispatcher: Arc<dyn JobDispatch>) -> Self {
        if let Some(executor) = Arc::get_mut(&mut self.executor) {
            executor.set_job_dispatch(dispatcher);
        }
        self
    }

    /// Attach a workflow dispatcher so handlers can call
    /// `ctx.start_workflow(...)`. Must be called before
    /// [`run`](Self::run) starts spawning tasks.
    pub fn with_workflow_dispatch(mut self, dispatcher: Arc<dyn WorkflowDispatch>) -> Self {
        if let Some(executor) = Arc::get_mut(&mut self.executor) {
            executor.set_workflow_dispatch(dispatcher);
        }
        self
    }

    /// Get worker ID.
    pub fn id(&self) -> Uuid {
        self.id
    }

    /// Get worker capabilities.
    pub fn capabilities(&self) -> &[String] {
        &self.config.capabilities
    }

    /// Run the worker (blocks until shutdown).
    pub async fn run(&mut self) -> Result<(), WorkerError> {
        let (shutdown_tx, mut shutdown_rx) = mpsc::channel::<()>(1);
        self.shutdown_tx = Some(shutdown_tx);

        // Semaphore for user jobs
        let semaphore = Arc::new(tokio::sync::Semaphore::new(self.config.max_concurrent));
        // Separate semaphore for system jobs ($workflow_resume, $cron:*)
        // so user job floods cannot starve workflow/cron execution.
        let system_semaphore = Arc::new(tokio::sync::Semaphore::new(self.config.system_reserved));

        // Spawn stale and expired cleanup task, gated behind leader election.
        // Only the leader runs cleanup to avoid thundering herd on multi-node.
        let cleanup_queue = self.queue.clone();
        let cleanup_interval = self.config.stale_cleanup_interval;
        let stale_threshold = self.config.stale_threshold;
        let cleanup_leader = self.config.leader_election.clone();
        let shutdown_notify = Arc::new(tokio::sync::Notify::new());
        let cleanup_shutdown = shutdown_notify.clone();
        let cleanup_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cleanup_shutdown.notified() => break,
                    _ = tokio::time::sleep(cleanup_interval) => {}
                }

                // Only run cleanup if we are the leader (or no election configured)
                let is_leader = cleanup_leader
                    .as_ref()
                    .map(|e| e.is_leader())
                    .unwrap_or(true);
                if !is_leader {
                    continue;
                }

                if let Err(e) = cleanup_queue.release_stale(stale_threshold).await {
                    tracing::warn!(error = %e, "Failed to cleanup stale jobs");
                }

                match cleanup_queue.cleanup_expired().await {
                    Ok(count) if count > 0 => {
                        tracing::debug!(count, "Cleaned up expired job records");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Failed to cleanup expired jobs");
                    }
                    _ => {}
                }
            }
        });

        // NOTIFY wakeup via the shared PgNotifyBus (no dedicated connection).
        let wakeup_notify = Arc::new(tokio::sync::Notify::new());
        let wakeup_trigger = wakeup_notify.clone();
        let wakeup_shutdown = shutdown_notify.clone();
        if let Some(mut rx) = self.notify_bus.subscribe("forge_jobs_available") {
            tokio::spawn(async move {
                loop {
                    tokio::select! {
                        _ = wakeup_shutdown.notified() => return,
                        result = rx.recv() => {
                            match result {
                                Ok(_) => wakeup_trigger.notify_one(),
                                Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                                    tracing::debug!(missed = n, "Job wakeup receiver lagged");
                                    wakeup_trigger.notify_one();
                                }
                                Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                            }
                        }
                    }
                }
            });
        }

        tracing::debug!(
            worker_id = %self.id,
            capabilities = ?self.config.capabilities,
            "Worker started"
        );

        // Track in-flight job tasks so shutdown can join (or abort) them
        // instead of dropping the handles and silently abandoning long
        // running jobs.
        let mut job_tasks: tokio::task::JoinSet<()> = tokio::task::JoinSet::new();

        loop {
            // Wait for either a NOTIFY wakeup, poll interval, or shutdown.
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    tracing::debug!(worker_id = %self.id, "Worker shutting down");
                    shutdown_notify.notify_waiters();
                    let _ = cleanup_handle.await;
                    self.drain_jobs(&mut job_tasks).await;
                    break;
                }
                _ = wakeup_notify.notified() => {}
                _ = tokio::time::sleep(self.config.poll_interval) => {}
            }

            // Reap finished tasks so the JoinSet doesn't grow unboundedly
            // between shutdowns. `try_join_next` is non-blocking.
            while job_tasks.try_join_next().is_some() {}

            let user_available = semaphore.available_permits();
            let system_available = system_semaphore.available_permits();
            let available = user_available + system_available;
            if available == 0 {
                continue;
            }

            let batch_size = (available as i32).min(self.config.batch_size);

            let jobs = match self
                .queue
                .claim(
                    self.id,
                    &self.config.capabilities,
                    self.config.claim_untagged,
                    batch_size,
                )
                .await
            {
                Ok(jobs) => jobs,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to claim jobs");
                    continue;
                }
            };

            for job in jobs {
                // Use system semaphore for $workflow_resume and $cron:* jobs,
                // user semaphore for everything else.
                let is_system_job =
                    job.job_type.starts_with("$workflow_") || job.job_type.starts_with("$cron:");
                let permit = if is_system_job {
                    match system_semaphore.clone().try_acquire_owned() {
                        Ok(p) => p,
                        Err(tokio::sync::TryAcquireError::NoPermits) => {
                            // We over-claimed relative to free permits (e.g. a
                            // race with another permit acquisition). Release the
                            // claim immediately so the row goes back to
                            // `pending` with attempts undone, rather than
                            // sitting claimed-but-unstarted until stale-reclaim.
                            tracing::debug!(
                                job_id = %job.id,
                                "System semaphore full, releasing claim"
                            );
                            if let Err(e) = self.queue.release_claim(job.id, self.id).await {
                                tracing::warn!(
                                    job_id = %job.id,
                                    error = %e,
                                    "Failed to release claim after semaphore exhaustion",
                                );
                            }
                            continue;
                        }
                        Err(tokio::sync::TryAcquireError::Closed) => {
                            tracing::error!("System semaphore closed, stopping job processing");
                            break;
                        }
                    }
                } else {
                    match semaphore.clone().try_acquire_owned() {
                        Ok(p) => p,
                        Err(tokio::sync::TryAcquireError::NoPermits) => {
                            // Same rationale as the system-permit branch.
                            tracing::debug!(
                                job_id = %job.id,
                                "Worker semaphore full, releasing claim"
                            );
                            if let Err(e) = self.queue.release_claim(job.id, self.id).await {
                                tracing::warn!(
                                    job_id = %job.id,
                                    error = %e,
                                    "Failed to release claim after semaphore exhaustion",
                                );
                            }
                            continue;
                        }
                        Err(tokio::sync::TryAcquireError::Closed) => {
                            tracing::error!("Worker semaphore closed, stopping job processing");
                            break;
                        }
                    }
                };
                let executor = self.executor.clone();
                let job_id = job.id;
                let job_type = job.job_type.clone();

                job_tasks.spawn(async move {
                    let start = std::time::Instant::now();
                    let span = tracing::info_span!(
                        "job.execute",
                        job_id = %job_id,
                        job_type = %job_type,
                    );

                    let result = executor.execute(&job).instrument(span).await;

                    let duration_secs = start.elapsed().as_secs_f64();

                    match &result {
                        super::executor::ExecutionResult::Completed { .. } => {
                            tracing::info!(job_id = %job_id, job_type = %job_type, duration_ms = (duration_secs * 1000.0) as u64, "Job completed");
                            crate::observability::record_job_execution(
                                &job_type,
                                "completed",
                                duration_secs,
                            );
                        }
                        super::executor::ExecutionResult::Failed { error, retryable } => {
                            if *retryable {
                                tracing::warn!(job_id = %job_id, job_type = %job_type, error = %error, "Job failed, will retry");
                                crate::observability::record_job_execution(
                                    &job_type,
                                    "retrying",
                                    duration_secs,
                                );
                            } else {
                                tracing::error!(job_id = %job_id, job_type = %job_type, error = %error, "Job failed permanently");
                                crate::observability::record_job_execution(
                                    &job_type,
                                    "failed",
                                    duration_secs,
                                );
                            }
                        }
                        super::executor::ExecutionResult::TimedOut { retryable } => {
                            tracing::error!(job_id = %job_id, job_type = %job_type, will_retry = %retryable, "Job timed out");
                            crate::observability::record_job_execution(
                                &job_type,
                                "timeout",
                                duration_secs,
                            );
                        }
                        super::executor::ExecutionResult::Cancelled { reason } => {
                            tracing::info!(job_id = %job_id, job_type = %job_type, reason = %reason, "Job cancelled");
                            crate::observability::record_job_execution(
                                &job_type,
                                "cancelled",
                                duration_secs,
                            );
                        }
                    }

                    drop(permit);
                });
            }
        }

        Ok(())
    }

    /// Request graceful shutdown.
    pub async fn shutdown(&self) {
        if let Some(ref tx) = self.shutdown_tx {
            let _ = tx.send(()).await;
        }
    }

    /// Wait for in-flight job tasks to finish within the configured grace
    /// period. Tasks that exceed the grace period are aborted so shutdown
    /// can complete — they will be requeued by the next process's
    /// stale-reclaim sweep on the `claimed`/`running` rows they left behind.
    async fn drain_jobs(&self, job_tasks: &mut tokio::task::JoinSet<()>) {
        let total = job_tasks.len();
        if total == 0 {
            return;
        }

        let grace = self.config.shutdown_grace_period;
        tracing::info!(
            worker_id = %self.id,
            in_flight = total,
            grace_secs = grace.as_secs(),
            "Draining in-flight jobs before shutdown",
        );

        let mut completed: usize = 0;
        let deadline = tokio::time::Instant::now() + grace;
        loop {
            tokio::select! {
                joined = job_tasks.join_next() => {
                    match joined {
                        Some(_) => completed += 1,
                        None => break,
                    }
                }
                _ = tokio::time::sleep_until(deadline) => {
                    break;
                }
            }
        }

        let aborted = job_tasks.len();
        if aborted > 0 {
            job_tasks.abort_all();
            // Reap the aborted handles so they don't dangle.
            while job_tasks.join_next().await.is_some() {}
        }

        tracing::info!(
            worker_id = %self.id,
            completed,
            aborted,
            total,
            "Worker drain finished",
        );
    }
}

/// Worker errors.
#[derive(Debug, thiserror::Error)]
pub enum WorkerError {
    #[error("Database error: {0}")]
    Database(String),

    #[error("Job execution error: {0}")]
    Execution(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worker_config_default() {
        let config = WorkerConfig::default();
        assert_eq!(config.capabilities, vec!["default".to_string()]);
        assert!(config.claim_untagged);
        assert_eq!(config.max_concurrent, 8);
        assert_eq!(config.system_reserved, 4);
        assert_eq!(config.batch_size, 10);
    }

    #[test]
    fn test_worker_config_custom() {
        let config = WorkerConfig {
            capabilities: vec!["media".to_string(), "general".to_string()],
            max_concurrent: 4,
            ..Default::default()
        };
        assert_eq!(config.capabilities.len(), 2);
        assert_eq!(config.max_concurrent, 4);
    }
}

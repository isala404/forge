use std::sync::Arc;
use std::time::Duration;

use forge_core::function::KvHandle;
use tokio::sync::mpsc;
use tracing::Instrument;
use uuid::Uuid;

use super::executor::JobExecutor;
use super::queue::JobQueue;
use super::registry::JobRegistry;
use crate::pg::LeaderElection;

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
            leader_election: None,
        }
    }
}

/// Background job worker.
pub struct Worker {
    id: Uuid,
    config: WorkerConfig,
    queue: JobQueue,
    pool: sqlx::PgPool,
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
    ) -> Self {
        let id = config.id.unwrap_or_else(Uuid::new_v4);
        let executor = Arc::new(JobExecutor::new(queue.clone(), registry, db_pool.clone()));

        Self {
            id,
            config,
            queue,
            pool: db_pool,
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

        // NOTIFY listener for immediate wakeup on job enqueue.
        // Reconnects with exponential backoff on connection loss.
        let wakeup_notify = Arc::new(tokio::sync::Notify::new());
        let wakeup_trigger = wakeup_notify.clone();
        let wakeup_pool = self.pool.clone();
        let wakeup_shutdown = shutdown_notify.clone();
        tokio::spawn(async move {
            let mut backoff = std::time::Duration::from_millis(500);
            let max_backoff = std::time::Duration::from_secs(30);
            loop {
                let mut listener = match sqlx::postgres::PgListener::connect_with(&wakeup_pool)
                    .await
                {
                    Ok(mut l) => match l.listen("forge_jobs_available").await {
                        Ok(()) => {
                            backoff = std::time::Duration::from_millis(500);
                            l
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "Job listener LISTEN failed, retrying");
                            tokio::select! {
                                _ = tokio::time::sleep(backoff) => {}
                                _ = wakeup_shutdown.notified() => return,
                            }
                            backoff = (backoff * 2).min(max_backoff);
                            continue;
                        }
                    },
                    Err(e) => {
                        tracing::warn!(error = %e, "Job listener connect failed, retrying");
                        tokio::select! {
                            _ = tokio::time::sleep(backoff) => {}
                            _ = wakeup_shutdown.notified() => return,
                        }
                        backoff = (backoff * 2).min(max_backoff);
                        continue;
                    }
                };
                loop {
                    tokio::select! {
                        _ = wakeup_shutdown.notified() => return,
                        notification = listener.recv() => {
                            match notification {
                                Ok(_) => {
                                    wakeup_trigger.notify_one();
                                }
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        "Job listener disconnected, reconnecting"
                                    );
                                    wakeup_trigger.notify_one();
                                    break;
                                }
                            }
                        }
                    }
                }
                tokio::select! {
                    _ = tokio::time::sleep(backoff) => {}
                    _ = wakeup_shutdown.notified() => return,
                }
                backoff = (backoff * 2).min(max_backoff);
            }
        });

        tracing::debug!(
            worker_id = %self.id,
            capabilities = ?self.config.capabilities,
            "Worker started"
        );

        loop {
            // Wait for either a NOTIFY wakeup, poll interval, or shutdown.
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    tracing::debug!(worker_id = %self.id, "Worker shutting down");
                    shutdown_notify.notify_waiters();
                    let _ = cleanup_handle.await;
                    break;
                }
                _ = wakeup_notify.notified() => {}
                _ = tokio::time::sleep(self.config.poll_interval) => {}
            }

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
                            // System slots full; job remains claimed and will be
                            // reclaimed by stale-job cleanup, then re-queued.
                            tracing::debug!(
                                job_id = %job.id,
                                "System semaphore full, job will be reclaimed"
                            );
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
                            // User slots full; job remains claimed and will be
                            // reclaimed by stale-job cleanup, then re-queued.
                            tracing::debug!(
                                job_id = %job.id,
                                "Worker semaphore full, job will be reclaimed"
                            );
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

                tokio::spawn(async move {
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

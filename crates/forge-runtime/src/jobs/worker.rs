use std::sync::Arc;
use std::time::Duration;

use tokio::sync::mpsc;
use tracing::Instrument;
use uuid::Uuid;

use super::executor::JobExecutor;
use super::queue::JobQueue;
use super::registry::JobRegistry;

/// Worker configuration.
#[derive(Debug, Clone)]
pub struct WorkerConfig {
    /// Worker ID (auto-generated if not provided).
    pub id: Option<Uuid>,
    /// Worker capabilities (e.g., ["general", "media"]).
    pub capabilities: Vec<String>,
    /// Maximum concurrent jobs.
    pub max_concurrent: usize,
    /// Poll interval when queue is empty.
    pub poll_interval: Duration,
    /// Batch size for claiming jobs.
    pub batch_size: i32,
    /// Stale job cleanup interval.
    pub stale_cleanup_interval: Duration,
    /// Stale job threshold.
    pub stale_threshold: chrono::Duration,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            id: None,
            capabilities: vec!["general".to_string()],
            max_concurrent: 10,
            poll_interval: Duration::from_millis(100),
            batch_size: 10,
            stale_cleanup_interval: Duration::from_secs(60),
            stale_threshold: chrono::Duration::minutes(5),
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

        // Semaphore to limit concurrent jobs
        let semaphore = Arc::new(tokio::sync::Semaphore::new(self.config.max_concurrent));

        // Spawn stale and expired cleanup task. Tied to a Notify shared with
        // shutdown so it exits cleanly with the worker — orphan cleanup tasks
        // in a multi-node cluster would compete with live workers' jobs.
        let cleanup_queue = self.queue.clone();
        let cleanup_interval = self.config.stale_cleanup_interval;
        let stale_threshold = self.config.stale_threshold;
        let shutdown_notify = Arc::new(tokio::sync::Notify::new());
        let cleanup_shutdown = shutdown_notify.clone();
        let cleanup_handle = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = cleanup_shutdown.notified() => break,
                    _ = tokio::time::sleep(cleanup_interval) => {}
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
        // Falls back to poll_interval if the listener connection fails.
        let wakeup_notify = Arc::new(tokio::sync::Notify::new());
        let wakeup_trigger = wakeup_notify.clone();
        let wakeup_pool = self.pool.clone();
        let wakeup_shutdown = shutdown_notify.clone();
        tokio::spawn(async move {
            let listener = sqlx::postgres::PgListener::connect_with(&wakeup_pool).await;
            let mut listener = match listener {
                Ok(mut l) => {
                    if l.listen("forge_jobs_available").await.is_err() {
                        return;
                    }
                    l
                }
                Err(_) => return,
            };
            loop {
                tokio::select! {
                    _ = wakeup_shutdown.notified() => break,
                    notification = listener.recv() => {
                        if notification.is_ok() {
                            wakeup_trigger.notify_one();
                        }
                    }
                }
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

            let available = semaphore.available_permits();
            if available == 0 {
                continue;
            }

            let batch_size = (available as i32).min(self.config.batch_size);

            let jobs = match self.queue.claim(
                self.id,
                &self.config.capabilities,
                batch_size,
            ).await {
                Ok(jobs) => jobs,
                Err(e) => {
                    tracing::warn!(error = %e, "Failed to claim jobs");
                    continue;
                }
            };

            for job in jobs {
                let permit = match semaphore.clone().acquire_owned().await {
                    Ok(p) => p,
                    Err(_) => {
                        tracing::error!("Worker semaphore closed, stopping job processing");
                        break;
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
                            crate::observability::record_job_execution(&job_type, "completed", duration_secs);
                        }
                        super::executor::ExecutionResult::Failed { error, retryable } => {
                            if *retryable {
                                tracing::warn!(job_id = %job_id, job_type = %job_type, error = %error, "Job failed, will retry");
                                crate::observability::record_job_execution(&job_type, "retrying", duration_secs);
                            } else {
                                tracing::error!(job_id = %job_id, job_type = %job_type, error = %error, "Job failed permanently");
                                crate::observability::record_job_execution(&job_type, "failed", duration_secs);
                            }
                        }
                        super::executor::ExecutionResult::TimedOut { retryable } => {
                            tracing::error!(job_id = %job_id, job_type = %job_type, will_retry = %retryable, "Job timed out");
                            crate::observability::record_job_execution(&job_type, "timeout", duration_secs);
                        }
                        super::executor::ExecutionResult::Cancelled { reason } => {
                            tracing::info!(job_id = %job_id, job_type = %job_type, reason = %reason, "Job cancelled");
                            crate::observability::record_job_execution(&job_type, "cancelled", duration_secs);
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
        assert_eq!(config.capabilities, vec!["general".to_string()]);
        assert_eq!(config.max_concurrent, 10);
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

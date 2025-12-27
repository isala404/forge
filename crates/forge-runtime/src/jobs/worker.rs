use std::sync::Arc;
use std::time::{Duration, Instant};

use forge_core::observability::Metric;
use tokio::sync::mpsc;
use uuid::Uuid;

use super::executor::JobExecutor;
use super::queue::JobQueue;
use super::registry::JobRegistry;
use crate::observability::ObservabilityState;

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
    executor: Arc<JobExecutor>,
    shutdown_tx: Option<mpsc::Sender<()>>,
    observability: Option<ObservabilityState>,
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
        let executor = Arc::new(JobExecutor::new(queue.clone(), registry, db_pool));

        Self {
            id,
            config,
            queue,
            executor,
            shutdown_tx: None,
            observability: None,
        }
    }

    /// Create a new worker with observability.
    pub fn with_observability(
        config: WorkerConfig,
        queue: JobQueue,
        registry: JobRegistry,
        db_pool: sqlx::PgPool,
        observability: ObservabilityState,
    ) -> Self {
        let id = config.id.unwrap_or_else(Uuid::new_v4);
        let executor = Arc::new(JobExecutor::new(queue.clone(), registry, db_pool));

        Self {
            id,
            config,
            queue,
            executor,
            shutdown_tx: None,
            observability: Some(observability),
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

        // Spawn stale cleanup task
        let cleanup_queue = self.queue.clone();
        let cleanup_interval = self.config.stale_cleanup_interval;
        let stale_threshold = self.config.stale_threshold;
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(cleanup_interval).await;
                if let Err(e) = cleanup_queue.release_stale(stale_threshold).await {
                    tracing::error!("Failed to cleanup stale jobs: {}", e);
                }
            }
        });

        tracing::info!(
            worker_id = %self.id,
            capabilities = ?self.config.capabilities,
            "Worker started"
        );

        loop {
            tokio::select! {
                _ = shutdown_rx.recv() => {
                    tracing::info!(worker_id = %self.id, "Worker shutting down");
                    break;
                }
                _ = tokio::time::sleep(self.config.poll_interval) => {
                    // Calculate how many jobs we can claim
                    let available = semaphore.available_permits();
                    if available == 0 {
                        continue;
                    }

                    let batch_size = (available as i32).min(self.config.batch_size);

                    // Claim jobs
                    let jobs = match self.queue.claim(
                        self.id,
                        &self.config.capabilities,
                        batch_size,
                    ).await {
                        Ok(jobs) => jobs,
                        Err(e) => {
                            tracing::error!("Failed to claim jobs: {}", e);
                            continue;
                        }
                    };

                    // Record jobs claimed metric
                    if let Some(ref obs) = self.observability {
                        let mut metric = Metric::counter("jobs_dispatched_total", jobs.len() as f64);
                        metric.labels.insert("worker_id".to_string(), self.id.to_string());
                        obs.record_metric(metric).await;
                    }

                    // Process each job
                    for job in jobs {
                        let permit = semaphore.clone().acquire_owned().await.unwrap();
                        let executor = self.executor.clone();
                        let job_id = job.id;
                        let job_type = job.job_type.clone();
                        let observability = self.observability.clone();
                        let worker_id = self.id;

                        tokio::spawn(async move {
                            let start = Instant::now();

                            tracing::debug!(
                                job_id = %job_id,
                                job_type = %job_type,
                                "Processing job"
                            );

                            let result = executor.execute(&job).await;
                            let duration = start.elapsed();

                            // Record job duration metric
                            if let Some(ref obs) = observability {
                                let mut duration_metric = Metric::gauge(
                                    "job_duration_seconds",
                                    duration.as_secs_f64(),
                                );
                                duration_metric.labels.insert("job_type".to_string(), job_type.clone());
                                duration_metric.labels.insert("worker_id".to_string(), worker_id.to_string());
                                obs.record_metric(duration_metric).await;
                            }

                            match &result {
                                super::executor::ExecutionResult::Completed { .. } => {
                                    tracing::info!(
                                        job_id = %job_id,
                                        job_type = %job_type,
                                        "Job completed"
                                    );

                                    // Record completed metric
                                    if let Some(ref obs) = observability {
                                        let mut metric = Metric::counter("jobs_completed_total", 1.0);
                                        metric.labels.insert("job_type".to_string(), job_type.clone());
                                        metric.labels.insert("worker_id".to_string(), worker_id.to_string());
                                        obs.record_metric(metric).await;
                                    }
                                }
                                super::executor::ExecutionResult::Failed { error, retryable } => {
                                    if *retryable {
                                        tracing::warn!(
                                            job_id = %job_id,
                                            job_type = %job_type,
                                            error = %error,
                                            "Job failed, will retry"
                                        );
                                    } else {
                                        tracing::error!(
                                            job_id = %job_id,
                                            job_type = %job_type,
                                            error = %error,
                                            "Job failed permanently"
                                        );
                                    }

                                    // Record failed metric
                                    if let Some(ref obs) = observability {
                                        let mut metric = Metric::counter("jobs_failed_total", 1.0);
                                        metric.labels.insert("job_type".to_string(), job_type.clone());
                                        metric.labels.insert("worker_id".to_string(), worker_id.to_string());
                                        metric.labels.insert("retryable".to_string(), retryable.to_string());
                                        obs.record_metric(metric).await;
                                    }
                                }
                                super::executor::ExecutionResult::TimedOut { retryable } => {
                                    tracing::warn!(
                                        job_id = %job_id,
                                        job_type = %job_type,
                                        will_retry = %retryable,
                                        "Job timed out"
                                    );

                                    // Record timeout metric
                                    if let Some(ref obs) = observability {
                                        let mut metric = Metric::counter("jobs_timeout_total", 1.0);
                                        metric.labels.insert("job_type".to_string(), job_type.clone());
                                        metric.labels.insert("worker_id".to_string(), worker_id.to_string());
                                        obs.record_metric(metric).await;
                                    }
                                }
                            }

                            drop(permit); // Release semaphore
                        });
                    }
                }
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

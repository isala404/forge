use std::sync::Arc;
use std::time::Duration;

use forge_core::function::AuthContext;
use forge_core::job::JobContext;
use tokio::time::timeout;
use uuid::Uuid;

use super::queue::{JobQueue, JobRecord};
use super::registry::{JobEntry, JobRegistry};

/// Executes jobs with timeout and retry handling.
pub struct JobExecutor {
    queue: JobQueue,
    registry: Arc<JobRegistry>,
    db_pool: sqlx::PgPool,
    http_client: reqwest::Client,
}

impl JobExecutor {
    /// Create a new job executor.
    pub fn new(queue: JobQueue, registry: JobRegistry, db_pool: sqlx::PgPool) -> Self {
        Self {
            queue,
            registry: Arc::new(registry),
            db_pool,
            http_client: reqwest::Client::new(),
        }
    }

    /// Execute a claimed job.
    pub async fn execute(&self, job: &JobRecord) -> ExecutionResult {
        let entry = match self.registry.get(&job.job_type) {
            Some(e) => e,
            None => {
                return ExecutionResult::Failed {
                    error: format!("Unknown job type: {}", job.job_type),
                    retryable: false,
                };
            }
        };

        // Mark job as running
        if let Err(e) = self.queue.start(job.id).await {
            return ExecutionResult::Failed {
                error: format!("Failed to start job: {}", e),
                retryable: true,
            };
        }

        // Create job context
        let ctx = JobContext::new(
            job.id,
            job.job_type.clone(),
            job.attempts as u32,
            job.max_attempts as u32,
            self.db_pool.clone(),
            self.http_client.clone(),
        );

        // Execute with timeout
        let job_timeout = entry.info.timeout;
        let result = timeout(job_timeout, self.run_handler(&entry, &ctx, &job.input)).await;

        match result {
            Ok(Ok(output)) => {
                // Job completed successfully
                if let Err(e) = self.queue.complete(job.id, output.clone()).await {
                    tracing::error!("Failed to mark job {} as complete: {}", job.id, e);
                }
                ExecutionResult::Completed { output }
            }
            Ok(Err(e)) => {
                // Job failed
                let error_msg = e.to_string();
                let should_retry = job.attempts < job.max_attempts;

                let retry_delay = if should_retry {
                    Some(entry.info.retry.calculate_backoff(job.attempts as u32))
                } else {
                    None
                };

                let chrono_delay = retry_delay.map(|d| {
                    chrono::Duration::from_std(d).unwrap_or(chrono::Duration::seconds(60))
                });

                if let Err(e) = self.queue.fail(job.id, &error_msg, chrono_delay).await {
                    tracing::error!("Failed to mark job {} as failed: {}", job.id, e);
                }

                ExecutionResult::Failed {
                    error: error_msg,
                    retryable: should_retry,
                }
            }
            Err(_) => {
                // Timeout
                let error_msg = format!("Job timed out after {:?}", job_timeout);
                let should_retry = job.attempts < job.max_attempts;

                let retry_delay = if should_retry {
                    Some(chrono::Duration::seconds(60))
                } else {
                    None
                };

                if let Err(e) = self.queue.fail(job.id, &error_msg, retry_delay).await {
                    tracing::error!("Failed to mark job {} as timed out: {}", job.id, e);
                }

                ExecutionResult::TimedOut {
                    retryable: should_retry,
                }
            }
        }
    }

    /// Run the job handler.
    async fn run_handler(
        &self,
        entry: &Arc<JobEntry>,
        ctx: &JobContext,
        input: &serde_json::Value,
    ) -> forge_core::Result<serde_json::Value> {
        (entry.handler)(ctx, input.clone()).await
    }
}

/// Result of job execution.
#[derive(Debug)]
pub enum ExecutionResult {
    /// Job completed successfully.
    Completed { output: serde_json::Value },
    /// Job failed.
    Failed { error: String, retryable: bool },
    /// Job timed out.
    TimedOut { retryable: bool },
}

impl ExecutionResult {
    /// Check if execution was successful.
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Completed { .. })
    }

    /// Check if the job should be retried.
    pub fn should_retry(&self) -> bool {
        match self {
            Self::Failed { retryable, .. } => *retryable,
            Self::TimedOut { retryable } => *retryable,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_execution_result_success() {
        let result = ExecutionResult::Completed {
            output: serde_json::json!({}),
        };
        assert!(result.is_success());
        assert!(!result.should_retry());
    }

    #[test]
    fn test_execution_result_failed_retryable() {
        let result = ExecutionResult::Failed {
            error: "test error".to_string(),
            retryable: true,
        };
        assert!(!result.is_success());
        assert!(result.should_retry());
    }

    #[test]
    fn test_execution_result_failed_not_retryable() {
        let result = ExecutionResult::Failed {
            error: "test error".to_string(),
            retryable: false,
        };
        assert!(!result.is_success());
        assert!(!result.should_retry());
    }

    #[test]
    fn test_execution_result_timeout() {
        let result = ExecutionResult::TimedOut { retryable: true };
        assert!(!result.is_success());
        assert!(result.should_retry());
    }
}

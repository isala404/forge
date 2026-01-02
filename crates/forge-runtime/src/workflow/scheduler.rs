use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::event_store::EventStore;
use super::executor::WorkflowExecutor;
use forge_core::Result;

/// Configuration for the workflow scheduler.
#[derive(Debug, Clone)]
pub struct WorkflowSchedulerConfig {
    /// How often to poll for ready workflows.
    pub poll_interval: Duration,
    /// Maximum workflows to process per poll.
    pub batch_size: i32,
    /// Whether to process event-based wakeups.
    pub process_events: bool,
}

impl Default for WorkflowSchedulerConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
            batch_size: 100,
            process_events: true,
        }
    }
}

/// Scheduler for durable workflows.
///
/// Polls the database for suspended workflows that are ready to resume
/// (timer expired or event received) and triggers their execution.
pub struct WorkflowScheduler {
    pool: PgPool,
    executor: Arc<WorkflowExecutor>,
    #[allow(dead_code)]
    event_store: Arc<EventStore>,
    config: WorkflowSchedulerConfig,
}

impl WorkflowScheduler {
    /// Create a new workflow scheduler.
    pub fn new(
        pool: PgPool,
        executor: Arc<WorkflowExecutor>,
        event_store: Arc<EventStore>,
        config: WorkflowSchedulerConfig,
    ) -> Self {
        Self {
            pool,
            executor,
            event_store,
            config,
        }
    }

    /// Run the scheduler until shutdown.
    pub async fn run(&self, shutdown: CancellationToken) {
        let mut interval = tokio::time::interval(self.config.poll_interval);

        tracing::info!(
            poll_interval = ?self.config.poll_interval,
            batch_size = self.config.batch_size,
            "Workflow scheduler started"
        );

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(e) = self.process_ready_workflows().await {
                        tracing::error!(error = %e, "Failed to process ready workflows");
                    }
                }
                _ = shutdown.cancelled() => {
                    tracing::info!("Workflow scheduler shutting down");
                    break;
                }
            }
        }
    }

    /// Process workflows that are ready to resume.
    async fn process_ready_workflows(&self) -> Result<()> {
        // Query for workflows ready to wake (timer or event timeout)
        let workflows: Vec<(Uuid, Option<String>)> = sqlx::query_as(
            r#"
            SELECT id, waiting_for_event FROM forge_workflow_runs
            WHERE status = 'waiting' AND (
                (wake_at IS NOT NULL AND wake_at <= NOW())
                OR (event_timeout_at IS NOT NULL AND event_timeout_at <= NOW())
            )
            ORDER BY COALESCE(wake_at, event_timeout_at) ASC
            LIMIT $1
            FOR UPDATE SKIP LOCKED
            "#,
        )
        .bind(self.config.batch_size)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        let count = workflows.len();
        if count > 0 {
            tracing::debug!(count = count, "Processing ready workflows");
        }

        for (workflow_id, waiting_for_event) in workflows {
            if waiting_for_event.is_some() {
                // Event timeout - resume with timeout error
                self.resume_with_timeout(workflow_id).await;
            } else {
                // Timer expired - normal resume
                self.resume_workflow(workflow_id).await;
            }
        }

        // Also check for workflows waiting for events that now have events
        if self.config.process_events {
            self.process_event_wakeups().await?;
        }

        Ok(())
    }

    /// Process workflows that have pending events.
    async fn process_event_wakeups(&self) -> Result<()> {
        // Find workflows waiting for events that have matching events
        // Use a subquery to avoid DISTINCT with FOR UPDATE
        let workflows: Vec<(Uuid, String)> = sqlx::query_as(
            r#"
            SELECT wr.id, wr.waiting_for_event
            FROM forge_workflow_runs wr
            WHERE wr.status = 'waiting'
                AND wr.waiting_for_event IS NOT NULL
                AND EXISTS (
                    SELECT 1 FROM forge_workflow_events we
                    WHERE we.correlation_id = wr.id::text
                    AND we.event_name = wr.waiting_for_event
                    AND we.consumed_at IS NULL
                )
            LIMIT $1
            FOR UPDATE OF wr SKIP LOCKED
            "#,
        )
        .bind(self.config.batch_size)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        for (workflow_id, _event_name) in workflows {
            self.resume_with_event(workflow_id).await;
        }

        Ok(())
    }

    /// Resume a workflow after timer expiry.
    async fn resume_workflow(&self, workflow_run_id: Uuid) {
        // Clear wake state
        if let Err(e) = sqlx::query(
            r#"
            UPDATE forge_workflow_runs
            SET wake_at = NULL, suspended_at = NULL, status = 'running'
            WHERE id = $1
            "#,
        )
        .bind(workflow_run_id)
        .execute(&self.pool)
        .await
        {
            tracing::error!(
                workflow_run_id = %workflow_run_id,
                error = %e,
                "Failed to clear wake state"
            );
            return;
        }

        // Resume execution - use resume_from_sleep so ctx.sleep() returns immediately
        if let Err(e) = self.executor.resume_from_sleep(workflow_run_id).await {
            tracing::error!(
                workflow_run_id = %workflow_run_id,
                error = %e,
                "Failed to resume workflow"
            );
        } else {
            tracing::info!(
                workflow_run_id = %workflow_run_id,
                "Resumed workflow after timer"
            );
        }
    }

    /// Resume a workflow after event timeout.
    async fn resume_with_timeout(&self, workflow_run_id: Uuid) {
        // Clear waiting state
        if let Err(e) = sqlx::query(
            r#"
            UPDATE forge_workflow_runs
            SET waiting_for_event = NULL, event_timeout_at = NULL, suspended_at = NULL, status = 'running'
            WHERE id = $1
            "#,
        )
        .bind(workflow_run_id)
        .execute(&self.pool)
        .await
        {
            tracing::error!(
                workflow_run_id = %workflow_run_id,
                error = %e,
                "Failed to clear waiting state"
            );
            return;
        }

        // Resume execution - the workflow will get a timeout error
        if let Err(e) = self.executor.resume(workflow_run_id).await {
            tracing::error!(
                workflow_run_id = %workflow_run_id,
                error = %e,
                "Failed to resume workflow after timeout"
            );
        } else {
            tracing::info!(
                workflow_run_id = %workflow_run_id,
                "Resumed workflow after event timeout"
            );
        }
    }

    /// Resume a workflow that received an event.
    async fn resume_with_event(&self, workflow_run_id: Uuid) {
        // Clear waiting state
        if let Err(e) = sqlx::query(
            r#"
            UPDATE forge_workflow_runs
            SET waiting_for_event = NULL, event_timeout_at = NULL, suspended_at = NULL, status = 'running'
            WHERE id = $1
            "#,
        )
        .bind(workflow_run_id)
        .execute(&self.pool)
        .await
        {
            tracing::error!(
                workflow_run_id = %workflow_run_id,
                error = %e,
                "Failed to clear waiting state for event"
            );
            return;
        }

        // Resume execution
        if let Err(e) = self.executor.resume(workflow_run_id).await {
            tracing::error!(
                workflow_run_id = %workflow_run_id,
                error = %e,
                "Failed to resume workflow after event"
            );
        } else {
            tracing::info!(
                workflow_run_id = %workflow_run_id,
                "Resumed workflow after event received"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_scheduler_config_default() {
        let config = WorkflowSchedulerConfig::default();
        assert_eq!(config.poll_interval, Duration::from_secs(1));
        assert_eq!(config.batch_size, 100);
        assert!(config.process_events);
    }
}

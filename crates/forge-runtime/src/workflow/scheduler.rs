use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::bridge::WORKFLOW_RESUME_JOB;
use super::event_store::EventStore;
use crate::jobs::JobQueue;
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
/// (timer expired or event received) and enqueues `$workflow_resume` jobs
/// for the worker pool. Also listens for NOTIFY events on the
/// `forge_workflow_wakeup` channel for immediate wakeup when a workflow
/// event is inserted.
pub struct WorkflowScheduler {
    pool: PgPool,
    job_queue: JobQueue,
    event_store: Arc<EventStore>,
    config: WorkflowSchedulerConfig,
}

impl WorkflowScheduler {
    /// Create a new workflow scheduler.
    pub fn new(
        pool: PgPool,
        job_queue: JobQueue,
        event_store: Arc<EventStore>,
        config: WorkflowSchedulerConfig,
    ) -> Self {
        Self {
            pool,
            job_queue,
            event_store,
            config,
        }
    }

    /// Run the scheduler until shutdown.
    ///
    /// Combines polling with NOTIFY-driven wakeup. When a workflow event is
    /// inserted, the `forge_workflow_event_notify` trigger fires a NOTIFY on
    /// the `forge_workflow_wakeup` channel, and we process immediately instead
    /// of waiting for the next poll cycle. Polling remains as a fallback at a
    /// longer interval (10x the base) to catch anything missed.
    pub async fn run(&self, shutdown: CancellationToken) {
        let fallback_interval = self.config.poll_interval * 10;
        let mut interval = tokio::time::interval(fallback_interval);
        let mut cleanup_interval = tokio::time::interval(Duration::from_secs(3600));

        // Set up NOTIFY listener for immediate wakeup
        let mut listener = match sqlx::postgres::PgListener::connect_with(&self.pool).await {
            Ok(mut l) => {
                if let Err(e) = l.listen("forge_workflow_wakeup").await {
                    tracing::warn!(error = %e, "Failed to listen on workflow wakeup channel, using poll-only mode");
                }
                Some(l)
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to create workflow wakeup listener, using poll-only mode");
                None
            }
        };

        tracing::debug!(
            poll_interval = ?fallback_interval,
            batch_size = self.config.batch_size,
            notify_enabled = listener.is_some(),
            "Workflow scheduler started"
        );

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if let Err(e) = self.process_ready_workflows().await {
                        tracing::warn!(error = %e, "Failed to process ready workflows");
                    }
                }
                notification = async {
                    match listener.as_mut() {
                        Some(l) => l.recv().await,
                        None => std::future::pending().await,
                    }
                } => {
                    match notification {
                        Ok(_) => {
                            if let Err(e) = self.process_ready_workflows().await {
                                tracing::warn!(error = %e, "Failed to process workflows after wakeup");
                            }
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "Workflow wakeup listener error, will retry on next poll");
                        }
                    }
                }
                _ = cleanup_interval.tick() => {
                    // Periodically clean up consumed events older than 24 hours
                    let cutoff = chrono::Utc::now() - chrono::Duration::hours(24);
                    match self.event_store.cleanup_consumed_events(cutoff).await {
                        Ok(count) if count > 0 => {
                            tracing::debug!(count, "Cleaned up consumed workflow events");
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "Failed to clean up consumed events");
                        }
                        _ => {}
                    }
                }
                _ = shutdown.cancelled() => {
                    tracing::debug!("Workflow scheduler shutting down");
                    break;
                }
            }
        }
    }

    /// Process workflows that are ready to resume.
    async fn process_ready_workflows(&self) -> Result<()> {
        // Query for workflows ready to wake (timer or event timeout)
        let workflows = sqlx::query!(
            r#"
            SELECT id, workflow_name, workflow_version, workflow_signature, waiting_for_event
            FROM forge_workflow_runs
            WHERE status = 'waiting' AND (
                (wake_at IS NOT NULL AND wake_at <= NOW())
                OR (event_timeout_at IS NOT NULL AND event_timeout_at <= NOW())
            )
            ORDER BY COALESCE(wake_at, event_timeout_at) ASC
            LIMIT $1
            FOR UPDATE SKIP LOCKED
            "#,
            self.config.batch_size as i64
        )
        .fetch_all(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        let count = workflows.len();
        if count > 0 {
            tracing::trace!(count, "Processing ready workflows");
        }

        for workflow in workflows {
            if workflow.waiting_for_event.is_some() {
                // Event timeout - resume with timeout error
                self.resume_with_timeout(workflow.id).await;
            } else {
                // Timer expired - normal resume
                self.resume_workflow(workflow.id).await;
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
        let workflows = sqlx::query!(
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
            self.config.batch_size as i64
        )
        .fetch_all(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        for workflow in workflows {
            let workflow_id = workflow.id;
            let Some(event_name) = workflow.waiting_for_event else {
                continue;
            };
            // Consume the event via event_store so it's marked as processed
            match self
                .event_store
                .consume_event(&event_name, &workflow_id.to_string(), workflow_id)
                .await
            {
                Ok(Some(_event)) => {
                    self.resume_with_event(workflow_id).await;
                }
                Ok(None) => {
                    tracing::debug!(
                        workflow_run_id = %workflow_id,
                        event_name = %event_name,
                        "Event already consumed, skipping wakeup"
                    );
                }
                Err(e) => {
                    tracing::warn!(
                        workflow_run_id = %workflow_id,
                        error = %e,
                        "Failed to consume workflow event"
                    );
                }
            }
        }

        Ok(())
    }

    /// Resume a workflow after timer expiry by enqueuing a job.
    async fn resume_workflow(&self, workflow_run_id: Uuid) {
        if let Err(e) = sqlx::query!(
            r#"
            UPDATE forge_workflow_runs
            SET wake_at = NULL, suspended_at = NULL, status = 'running'
            WHERE id = $1
            "#,
            workflow_run_id,
        )
        .execute(&self.pool)
        .await
        {
            tracing::warn!(workflow_run_id = %workflow_run_id, error = %e, "Failed to clear wake state");
            return;
        }

        self.enqueue_resume(workflow_run_id, true, "timer").await;
    }

    /// Resume a workflow after event timeout by enqueuing a job.
    async fn resume_with_timeout(&self, workflow_run_id: Uuid) {
        if let Err(e) = sqlx::query!(
            r#"
            UPDATE forge_workflow_runs
            SET waiting_for_event = NULL, event_timeout_at = NULL, suspended_at = NULL, status = 'running'
            WHERE id = $1
            "#,
            workflow_run_id,
        )
        .execute(&self.pool)
        .await
        {
            tracing::warn!(workflow_run_id = %workflow_run_id, error = %e, "Failed to clear waiting state");
            return;
        }

        self.enqueue_resume(workflow_run_id, false, "event_timeout")
            .await;
    }

    /// Resume a workflow that received an event by enqueuing a job.
    async fn resume_with_event(&self, workflow_run_id: Uuid) {
        if let Err(e) = sqlx::query!(
            r#"
            UPDATE forge_workflow_runs
            SET waiting_for_event = NULL, event_timeout_at = NULL, suspended_at = NULL, status = 'running'
            WHERE id = $1
            "#,
            workflow_run_id,
        )
        .execute(&self.pool)
        .await
        {
            tracing::warn!(workflow_run_id = %workflow_run_id, error = %e, "Failed to clear waiting state for event");
            return;
        }

        self.enqueue_resume(workflow_run_id, false, "event").await;
    }

    /// Enqueue a `$workflow_resume` job for the worker pool.
    async fn enqueue_resume(&self, workflow_run_id: Uuid, from_sleep: bool, trigger: &str) {
        let input = serde_json::json!({
            "run_id": workflow_run_id.to_string(),
            "from_sleep": from_sleep,
        });
        let job = crate::jobs::JobRecord::new(
            WORKFLOW_RESUME_JOB.to_string(),
            input,
            forge_core::job::JobPriority::High,
            1,
        );
        match self.job_queue.enqueue(job).await {
            Ok(job_id) => {
                tracing::debug!(
                    workflow_run_id = %workflow_run_id,
                    job_id = %job_id,
                    trigger,
                    "Enqueued workflow resume job"
                );
            }
            Err(e) => {
                tracing::error!(
                    workflow_run_id = %workflow_run_id,
                    error = %e,
                    trigger,
                    "Failed to enqueue workflow resume job"
                );
            }
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

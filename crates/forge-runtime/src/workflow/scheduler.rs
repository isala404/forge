use std::sync::Arc;
use std::time::Duration;

use sqlx::PgPool;
use tokio_util::sync::CancellationToken;
use uuid::Uuid;

use super::bridge::WORKFLOW_RESUME_JOB;
use super::event_store::EventStore;
use crate::jobs::JobQueue;
use crate::pg::LeaderElection;
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
    /// Leader election handle for leader-gated operations (cleanup).
    pub leader_election: Option<Arc<LeaderElection>>,
}

impl Default for WorkflowSchedulerConfig {
    fn default() -> Self {
        Self {
            poll_interval: Duration::from_secs(1),
            batch_size: 100,
            process_events: true,
            leader_election: None,
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

    /// Returns true if this node is the leader (or no election is configured).
    fn is_leader(&self) -> bool {
        self.config
            .leader_election
            .as_ref()
            .map(|e| e.is_leader())
            .unwrap_or(true)
    }

    /// Run the scheduler until shutdown.
    ///
    /// Combines polling with NOTIFY-driven wakeup. When a workflow event is
    /// inserted, the `forge_workflow_event_notify` trigger fires a NOTIFY on
    /// the `forge_workflow_wakeup` channel, and we process immediately instead
    /// of waiting for the next poll cycle. Polling remains the wakeup path for
    /// durable sleeps (no NOTIFY fires when `wake_at` expires), so the timer
    /// runs at `poll_interval` directly.
    pub async fn run(&self, shutdown: CancellationToken) {
        let mut interval = tokio::time::interval(self.config.poll_interval);
        let mut cleanup_interval = tokio::time::interval(Duration::from_secs(3600));

        // NOTIFY listener for immediate wakeup. Reconnects with backoff.
        let wakeup = Arc::new(tokio::sync::Notify::new());
        let wakeup_trigger = wakeup.clone();
        let wakeup_pool = self.pool.clone();
        let wakeup_shutdown = shutdown.clone();
        tokio::spawn(async move {
            let mut backoff = std::time::Duration::from_millis(500);
            let max_backoff = std::time::Duration::from_secs(30);
            loop {
                let mut listener =
                    match sqlx::postgres::PgListener::connect_with(&wakeup_pool).await {
                        Ok(mut l) => match l.listen("forge_workflow_wakeup").await {
                            Ok(()) => {
                                backoff = std::time::Duration::from_millis(500);
                                l
                            }
                            Err(e) => {
                                tracing::warn!(
                                    error = %e,
                                    "Workflow listener LISTEN failed, retrying"
                                );
                                tokio::select! {
                                    _ = tokio::time::sleep(backoff) => {}
                                    _ = wakeup_shutdown.cancelled() => return,
                                }
                                backoff = (backoff * 2).min(max_backoff);
                                continue;
                            }
                        },
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "Workflow listener connect failed, retrying"
                            );
                            tokio::select! {
                                _ = tokio::time::sleep(backoff) => {}
                                _ = wakeup_shutdown.cancelled() => return,
                            }
                            backoff = (backoff * 2).min(max_backoff);
                            continue;
                        }
                    };
                loop {
                    tokio::select! {
                        _ = wakeup_shutdown.cancelled() => return,
                        notification = listener.recv() => {
                            match notification {
                                Ok(_) => wakeup_trigger.notify_one(),
                                Err(e) => {
                                    tracing::warn!(
                                        error = %e,
                                        "Workflow listener disconnected, reconnecting"
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
                    _ = wakeup_shutdown.cancelled() => return,
                }
                backoff = (backoff * 2).min(max_backoff);
            }
        });

        tracing::debug!(
            poll_interval = ?self.config.poll_interval,
            batch_size = self.config.batch_size,
            "Workflow scheduler started"
        );

        loop {
            tokio::select! {
                _ = interval.tick() => {
                    if self.is_leader()
                        && let Err(e) = self.process_ready_workflows().await
                    {
                        tracing::warn!(error = %e, "Failed to process ready workflows");
                    }
                }
                _ = wakeup.notified() => {
                    if self.is_leader()
                        && let Err(e) = self.process_ready_workflows().await
                    {
                        tracing::warn!(error = %e, "Failed to process workflows after wakeup");
                    }
                }
                _ = cleanup_interval.tick() => {
                    // Only the leader runs cleanup to avoid thundering herd on DELETE.
                    if self.is_leader() {
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
        // Cancellations take priority over timer/event wakeups: if an operator
        // requested cancel, we run the cancel job first and skip any pending
        // resume work for the same run.
        self.process_cancel_requests().await?;

        // Query for workflows ready to wake (timer or event timeout)
        let workflows = sqlx::query!(
            r#"
            SELECT id, workflow_name, workflow_version, workflow_signature, waiting_for_event
            FROM forge_workflow_runs
            WHERE cancel_requested_at IS NULL
              AND (
                (status = 'sleeping' AND wake_at IS NOT NULL AND wake_at <= NOW())
                OR (status = 'waiting' AND event_timeout_at IS NOT NULL AND event_timeout_at <= NOW())
              )
            ORDER BY COALESCE(wake_at, event_timeout_at) ASC
            LIMIT $1
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
                self.claim_and_resume(workflow.id, false, "event_timeout")
                    .await;
            } else {
                self.claim_and_resume(workflow.id, true, "timer").await;
            }
        }

        // Also check for workflows waiting for events that now have events
        if self.config.process_events {
            self.process_event_wakeups().await?;
        }

        Ok(())
    }

    /// Process workflows with a pending cancel request.
    ///
    /// These rows are surfaced by the `forge_workflow_runs_cancel_notify`
    /// trigger on `forge_workflow_wakeup`, so this method is normally
    /// driven by NOTIFY and completes within a single poll cycle.
    async fn process_cancel_requests(&self) -> Result<()> {
        let workflows = sqlx::query!(
            r#"
            SELECT id, cancel_reason
            FROM forge_workflow_runs
            WHERE cancel_requested_at IS NOT NULL
              AND status IN ('pending', 'running', 'sleeping', 'waiting')
            ORDER BY cancel_requested_at ASC
            LIMIT $1
            "#,
            self.config.batch_size as i64
        )
        .fetch_all(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        for row in workflows {
            if !self.try_claim_waiting(row.id).await {
                continue;
            }
            let reason = row
                .cancel_reason
                .unwrap_or_else(|| "operator cancel".to_string());
            self.enqueue_cancel(row.id, &reason).await;
        }

        Ok(())
    }

    /// Enqueue a `$workflow_resume` job carrying the cancel flag.
    async fn enqueue_cancel(&self, workflow_run_id: Uuid, reason: &str) {
        let input = serde_json::json!({
            "run_id": workflow_run_id.to_string(),
            "from_sleep": false,
            "cancel": true,
            "reason": reason,
        });
        let job = crate::jobs::JobRecord::new(
            WORKFLOW_RESUME_JOB.to_string(),
            input,
            forge_core::job::JobPriority::High,
            3,
        )
        .with_capability(forge_core::config::WORKFLOWS_QUEUE);
        match self.job_queue.enqueue(job).await {
            Ok(job_id) => {
                tracing::debug!(
                    workflow_run_id = %workflow_run_id,
                    job_id = %job_id,
                    trigger = "cancel",
                    "Enqueued workflow cancel job"
                );
            }
            Err(e) => {
                tracing::error!(
                    workflow_run_id = %workflow_run_id,
                    error = %e,
                    "Failed to enqueue workflow cancel job"
                );
            }
        }
    }

    /// Process workflows that have pending events.
    async fn process_event_wakeups(&self) -> Result<()> {
        // Find workflows waiting for events that have matching events.
        // try_claim_waiting gates the actual state transition.
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
                    self.claim_and_resume(workflow_id, false, "event").await;
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

    /// Atomically claim a workflow and enqueue a resume job in a single transaction.
    /// If the claim fails (row already claimed), the transaction is rolled back
    /// and no resume job is enqueued.
    async fn claim_and_resume(&self, workflow_run_id: Uuid, from_sleep: bool, trigger: &str) {
        let result: std::result::Result<(), sqlx::Error> = async {
            let mut tx = self.pool.begin().await?;

            // Claim: transition from sleeping/waiting to running
            // Runtime query: rewritten for single-transaction claim+resume.
            // Convert to query!() after next `cargo sqlx prepare`.
            #[allow(clippy::disallowed_methods)]
            let claimed = sqlx::query(
                r#"
                UPDATE forge_workflow_runs
                SET wake_at = NULL, waiting_for_event = NULL, event_timeout_at = NULL,
                    suspended_at = NULL, status = 'running'
                WHERE id = $1 AND status IN ('sleeping', 'waiting')
                "#,
            )
            .bind(workflow_run_id)
            .execute(&mut *tx)
            .await?;

            if claimed.rows_affected() == 0 {
                // Already claimed by another scheduler; rollback is implicit on drop
                return Ok(());
            }

            // Enqueue resume job in the same transaction
            let input = serde_json::json!({
                "run_id": workflow_run_id.to_string(),
                "from_sleep": from_sleep,
            });
            let job = crate::jobs::JobRecord::new(
                WORKFLOW_RESUME_JOB.to_string(),
                input,
                forge_core::job::JobPriority::High,
                3,
            )
            .with_capability(forge_core::config::WORKFLOWS_QUEUE);
            self.job_queue.enqueue_in_conn(&mut tx, job).await?;

            tx.commit().await?;

            tracing::debug!(
                workflow_run_id = %workflow_run_id,
                trigger,
                "Claimed workflow and enqueued resume job"
            );
            Ok(())
        }
        .await;

        if let Err(e) = result {
            tracing::warn!(
                workflow_run_id = %workflow_run_id,
                error = %e,
                trigger,
                "Failed to claim and resume workflow"
            );
        }
    }

    /// Atomically transition a workflow from `sleeping`/`waiting` to `running`.
    /// Returns `false` if the row was already claimed by another scheduler.
    /// Used by cancel path which has its own enqueue logic.
    async fn try_claim_waiting(&self, workflow_run_id: Uuid) -> bool {
        match sqlx::query!(
            r#"
            UPDATE forge_workflow_runs
            SET wake_at = NULL, waiting_for_event = NULL, event_timeout_at = NULL,
                suspended_at = NULL, status = 'running'
            WHERE id = $1 AND status IN ('sleeping', 'waiting')
            "#,
            workflow_run_id,
        )
        .execute(&self.pool)
        .await
        .map(|r| r.rows_affected())
        {
            Ok(n) => n > 0,
            Err(e) => {
                tracing::warn!(workflow_run_id = %workflow_run_id, error = %e, "Failed to claim workflow for resume");
                false
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

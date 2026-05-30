use std::sync::Arc;
use std::time::Duration;

use forge_core::CircuitBreakerClient;
use forge_core::function::{JobDispatch, KvHandle, WorkflowDispatch};
use forge_core::job::{JobContext, ProgressUpdate};
use tokio::time::timeout;

use super::queue::{JobQueue, JobRecord};
use super::registry::{JobEntry, JobRegistry};
use crate::observability;

/// How often to poll the progress channel between updates from the running job.
/// Short enough that progress propagates to subscribers within one frame; long
/// enough that an idle job doesn't burn CPU on the polling task.
const PROGRESS_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// Executes jobs with timeout and retry handling.
pub struct JobExecutor {
    queue: JobQueue,
    registry: Arc<JobRegistry>,
    db_pool: sqlx::PgPool,
    http_client: CircuitBreakerClient,
    kv: Option<Arc<dyn KvHandle>>,
    job_dispatch: Option<Arc<dyn JobDispatch>>,
    workflow_dispatch: Option<Arc<dyn WorkflowDispatch>>,
}

impl JobExecutor {
    const HEARTBEAT_INTERVAL: Duration = Duration::from_secs(30);

    pub fn new(queue: JobQueue, registry: JobRegistry, db_pool: sqlx::PgPool) -> Self {
        Self {
            queue,
            registry: Arc::new(registry),
            db_pool,
            http_client: CircuitBreakerClient::with_ssrf_protection(),
            kv: None,
            job_dispatch: None,
            workflow_dispatch: None,
        }
    }

    /// Attach a KV store handle so job handlers can call `ctx.kv()`.
    pub fn with_kv(mut self, kv: Arc<dyn KvHandle>) -> Self {
        self.kv = Some(kv);
        self
    }

    pub fn set_kv(&mut self, kv: Arc<dyn KvHandle>) {
        self.kv = Some(kv);
    }

    pub fn set_job_dispatch(&mut self, dispatcher: Arc<dyn JobDispatch>) {
        self.job_dispatch = Some(dispatcher);
    }

    pub fn set_workflow_dispatch(&mut self, dispatcher: Arc<dyn WorkflowDispatch>) {
        self.workflow_dispatch = Some(dispatcher);
    }

    /// Execute a claimed job.
    ///
    /// # Semaphore cost on lost-claim races
    ///
    /// The `start()` fence check (marking the job as running) can fail with
    /// `RowNotFound` when another worker reclaimed a stale claim between this
    /// worker's `claim()` and `start()` calls. When that happens execution is
    /// aborted before any real work is done, but **the semaphore permit is still
    /// consumed for the full round-trip of the fence check**. Under a sustained
    /// stale-reclaim storm this can exhaust the worker concurrency limit.
    ///
    /// Monitor the `worker_lost_claim_total` metric (tagged by `job_type`) to
    /// quantify the race rate. Persistent elevation means `stale_threshold` is
    /// too low relative to observed heartbeat latency; raise it until the
    /// counter returns to near-zero.
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

        if matches!(job.status, forge_core::job::JobStatus::Cancelled) {
            return ExecutionResult::Cancelled {
                reason: Self::cancellation_reason(job, "Job cancelled"),
            };
        }

        // Mark job as running. start() fences on (worker_id, attempts), so a
        // stale-reclaim race (this worker's claim was reassigned to another
        // node after a heartbeat gap) returns RowNotFound and we abort before
        // doing real work. The other worker's attempts counter will differ
        // because claim() increments it, making the transition unambiguous.
        let worker_id = match job.worker_id {
            Some(id) => id,
            None => {
                return ExecutionResult::Failed {
                    error: "Claimed job has no worker_id".to_string(),
                    retryable: false,
                };
            }
        };
        if let Err(e) = self.queue.start(job.id, worker_id, job.attempts).await {
            if matches!(e, sqlx::Error::RowNotFound) {
                observability::record_lost_claim(&job.job_type);
                tracing::warn!(
                    job_id = %job.id,
                    job_type = %job.job_type,
                    "Job claim was lost (likely stale-reclaim race); skipping execution",
                );
                return ExecutionResult::Cancelled {
                    reason: Self::cancellation_reason(job, "Claim lost to another worker"),
                };
            }
            return ExecutionResult::Failed {
                error: format!("Failed to start job: {}", e),
                retryable: true,
            };
        }

        let (progress_tx, progress_rx) = std::sync::mpsc::channel::<ProgressUpdate>();

        // try_recv() + async sleep avoids blocking the tokio runtime on a sync channel
        let progress_queue = self.queue.clone();
        let progress_job_id = job.id;
        tokio::spawn(async move {
            loop {
                match progress_rx.try_recv() {
                    Ok(update) => {
                        if let Err(e) = progress_queue
                            .update_progress(
                                progress_job_id,
                                update.percentage as i32,
                                &update.message,
                            )
                            .await
                        {
                            tracing::debug!(job_id = %progress_job_id, error = %e, "Failed to update job progress");
                        }
                    }
                    Err(std::sync::mpsc::TryRecvError::Empty) => {
                        tokio::time::sleep(PROGRESS_POLL_INTERVAL).await;
                    }
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                        break;
                    }
                }
            }
        });

        let mut ctx = {
            let mut c = JobContext::new(
                job.id,
                job.job_type.clone(),
                job.attempts as u32,
                job.max_attempts as u32,
                self.db_pool.clone(),
                self.http_client.clone(),
            )
            .with_saved(job.job_context.clone())
            .with_progress(progress_tx);
            if let Some(ref kv) = self.kv {
                c = c.with_kv(Arc::clone(kv));
            }
            if let Some(ref dispatcher) = self.job_dispatch {
                c = c.with_job_dispatch(Arc::clone(dispatcher));
            }
            if let Some(ref dispatcher) = self.workflow_dispatch {
                c = c.with_workflow_dispatch(Arc::clone(dispatcher));
            }
            c
        };
        if let Some(ref subject) = job.owner_subject {
            // Defense in depth: the job row stores `tenant_id` and
            // `owner_subject` independently. We trust the dispatcher to pair
            // them correctly, but if a stale/forged dispatch slipped a
            // mismatched pair through, the handler would execute cross-tenant
            // (#6 in issues doc). There's no system `users` table to consult,
            // so the framework can't reject the row authoritatively here. Warn
            // when an owner principal is present without a tenant — the shape
            // most likely to indicate a dispatch path that lost tenancy. (Single-
            // tenant apps legitimately dispatch with no tenant_id, so this is a
            // warning, not an assertion.)
            if job.tenant_id.is_none() && uuid::Uuid::parse_str(subject).is_ok() {
                tracing::warn!(
                    job_id = %job.id,
                    job_type = %job.job_type,
                    owner_subject = %subject,
                    "Job has UUID owner_subject but no tenant_id; if the app is multi-tenant this is a dispatch-side bug — the handler will run with empty tenant scope"
                );
            }
            let mut claims = std::collections::HashMap::new();
            if let Some(tid) = job.tenant_id {
                claims.insert(
                    "tenant_id".to_string(),
                    serde_json::Value::String(tid.to_string()),
                );
            }
            let auth = if let Ok(uuid) = uuid::Uuid::parse_str(subject) {
                forge_core::AuthContext::authenticated(uuid, Vec::new(), claims)
            } else {
                claims.insert(
                    "sub".to_string(),
                    serde_json::Value::String(subject.clone()),
                );
                forge_core::AuthContext::authenticated_without_uuid(Vec::new(), claims)
            };
            ctx = ctx.with_auth(auth);
        }

        // Jobs store the owner subject but not their roles. When a job reaches
        // the executor, the auth context is reconstructed with an empty role
        // list regardless of what roles the dispatcher held. A `require_role`
        // check at execution time is therefore impossible without a schema
        // migration to persist roles at dispatch time.
        //
        // The role check IS enforced at RPC dispatch time (router.rs). Jobs
        // dispatched programmatically via `ctx.dispatch_job()` bypass that
        // check. If the job has `require_role` and was NOT dispatched through
        // the RPC layer, the check is silently skipped here — log a warning so
        // the gap is visible in production traces.
        if let Some(required_role) = entry.info.required_role
            && job.owner_subject.is_some()
        {
            tracing::warn!(
                job_id = %job.id,
                job_type = %job.job_type,
                required_role = %required_role,
                "job has require_role but roles are not persisted in the job \
                 record; role enforcement is dispatch-time only (RPC path) — \
                 jobs dispatched programmatically skip this check",
            );
        }

        if let Some(tenant_id) = job.tenant_id {
            ctx = ctx.with_tenant_id(tenant_id);
        }

        ctx.set_http_timeout(entry.info.http_timeout);

        let heartbeat_queue = self.queue.clone();
        let heartbeat_job_id = job.id;
        let (heartbeat_stop_tx, mut heartbeat_stop_rx) = tokio::sync::watch::channel(false);
        let heartbeat_task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = tokio::time::sleep(Self::HEARTBEAT_INTERVAL) => {
                        if let Err(e) = heartbeat_queue.heartbeat(heartbeat_job_id).await {
                            tracing::debug!(job_id = %heartbeat_job_id, error = %e, "Failed to update job heartbeat");
                        }
                    }
                    changed = heartbeat_stop_rx.changed() => {
                        if changed.is_err() || *heartbeat_stop_rx.borrow() {
                            break;
                        }
                    }
                }
            }
        });

        // Drop guard: ensures the heartbeat task is aborted even if `execute`
        // is cancelled (e.g. `drain_jobs` calling `abort_all` on shutdown).
        // Without this the task would keep refreshing `last_heartbeat` for up
        // to 30s, blocking `release_stale` from requeueing the row (#9 in
        // issues doc).
        struct HeartbeatGuard {
            stop: tokio::sync::watch::Sender<bool>,
            handle: Option<tokio::task::JoinHandle<()>>,
        }
        impl Drop for HeartbeatGuard {
            fn drop(&mut self) {
                let _ = self.stop.send(true);
                if let Some(h) = self.handle.take() {
                    h.abort();
                }
            }
        }
        let mut heartbeat_guard = HeartbeatGuard {
            stop: heartbeat_stop_tx,
            handle: Some(heartbeat_task),
        };

        let job_timeout = entry.info.timeout;
        let exec_start = std::time::Instant::now();
        let result = timeout(job_timeout, self.run_handler(&entry, &ctx, &job.input)).await;
        let exec_duration_ms = exec_start.elapsed().as_millis() as i32;

        // Happy path: signal the heartbeat task and await it cleanly. The
        // guard will still run on early return, abort() is a no-op on a
        // joined handle.
        let _ = heartbeat_guard.stop.send(true);
        if let Some(h) = heartbeat_guard.handle.take() {
            let _ = h.await;
        }

        let ttl = entry.info.ttl;

        match result {
            Ok(Ok(output)) => {
                if let Err(e) = self.queue.complete(job.id, output.clone(), ttl).await {
                    tracing::error!(job_id = %job.id, error = %e, "Failed to complete job");
                }
                crate::signals::emit_server_execution(
                    &job.job_type,
                    "job",
                    exec_duration_ms,
                    true,
                    None,
                );
                ExecutionResult::Completed { output }
            }
            Ok(Err(e)) => {
                let error_msg = e.to_string();
                let cancel_requested = match ctx.is_cancel_requested().await {
                    Ok(value) => value,
                    Err(err) => {
                        tracing::debug!(job_id = %job.id, error = %err, "Failed to check cancellation status");
                        false
                    }
                };
                if matches!(e, forge_core::ForgeError::JobCancelled(_)) || cancel_requested {
                    let reason = Self::cancellation_reason(job, "Job cancellation requested");
                    if let Err(e) = self.queue.cancel(job.id, Some(&reason), ttl).await {
                        tracing::debug!(job_id = %job.id, error = %e, "Failed to cancel job");
                    }
                    if let Err(e) = self
                        .run_compensation(&entry, &ctx, &job.input, &reason)
                        .await
                    {
                        tracing::error!(job_id = %job.id, error = %e, "Job compensation failed");
                    }
                    crate::signals::emit_server_execution(
                        &job.job_type,
                        "job",
                        exec_duration_ms,
                        false,
                        Some(format!("cancelled: {}", reason)),
                    );
                    return ExecutionResult::Cancelled { reason };
                }
                let should_retry = job.attempts < job.max_attempts;

                let retry_delay = if should_retry {
                    Some(entry.info.retry.calculate_backoff(job.attempts as u32))
                } else {
                    None
                };

                let chrono_delay = retry_delay.map(|d| {
                    chrono::Duration::from_std(d).unwrap_or(chrono::Duration::seconds(60))
                });

                if let Err(e) = self.queue.fail(job.id, &error_msg, chrono_delay, ttl).await {
                    tracing::error!(job_id = %job.id, error = %e, "Failed to record job failure");
                }

                crate::signals::emit_server_execution(
                    &job.job_type,
                    "job",
                    exec_duration_ms,
                    false,
                    Some(error_msg.clone()),
                );

                ExecutionResult::Failed {
                    error: error_msg,
                    retryable: should_retry,
                }
            }
            Err(_) => {
                let error_msg = format!("Job timed out after {:?}", job_timeout);
                let should_retry = job.attempts < job.max_attempts;

                // Mirror the failure path: honor the job's configured backoff
                // strategy rather than hardcoding 60s (#13 in issues doc).
                let retry_delay = if should_retry {
                    let std_delay = entry.info.retry.calculate_backoff(job.attempts as u32);
                    Some(
                        chrono::Duration::from_std(std_delay)
                            .unwrap_or(chrono::Duration::seconds(60)),
                    )
                } else {
                    None
                };

                if let Err(e) = self.queue.fail(job.id, &error_msg, retry_delay, ttl).await {
                    tracing::error!(job_id = %job.id, error = %e, "Failed to record job timeout");
                }

                crate::signals::emit_server_execution(
                    &job.job_type,
                    "job",
                    exec_duration_ms,
                    false,
                    Some(error_msg),
                );

                ExecutionResult::TimedOut {
                    retryable: should_retry,
                }
            }
        }
    }

    async fn run_handler(
        &self,
        entry: &Arc<JobEntry>,
        ctx: &JobContext,
        input: &serde_json::Value,
    ) -> forge_core::Result<serde_json::Value> {
        (entry.handler)(ctx, input.clone()).await
    }

    async fn run_compensation(
        &self,
        entry: &Arc<JobEntry>,
        ctx: &JobContext,
        input: &serde_json::Value,
        reason: &str,
    ) -> forge_core::Result<()> {
        (entry.compensation)(ctx, input.clone(), reason).await
    }

    fn cancellation_reason(job: &JobRecord, fallback: &str) -> String {
        job.cancel_reason
            .clone()
            .unwrap_or_else(|| fallback.to_string())
    }
}

#[derive(Debug)]
pub enum ExecutionResult {
    Completed { output: serde_json::Value },
    Failed { error: String, retryable: bool },
    TimedOut { retryable: bool },
    Cancelled { reason: String },
}

impl ExecutionResult {
    pub fn is_success(&self) -> bool {
        matches!(self, Self::Completed { .. })
    }

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

    #[test]
    fn test_execution_result_cancelled() {
        let result = ExecutionResult::Cancelled {
            reason: "user request".to_string(),
        };
        assert!(!result.is_success());
        assert!(!result.should_retry());
    }

    #[test]
    fn timed_out_not_retryable_does_not_request_retry() {
        // The TimedOut branch in should_retry returns its own `retryable` flag —
        // make sure the false case isn't lost when the worker passes through.
        let result = ExecutionResult::TimedOut { retryable: false };
        assert!(!result.should_retry());
        assert!(!result.is_success());
    }

    #[test]
    fn heartbeat_interval_is_30_seconds() {
        // The stale-reclaim window in JobQueue assumes heartbeats land well
        // under it; if this jumps, the cleanup deadline in queue.rs must move
        // in lockstep.
        assert_eq!(JobExecutor::HEARTBEAT_INTERVAL, Duration::from_secs(30));
    }

    #[test]
    fn execution_result_debug_includes_error_payload() {
        // Workers log ExecutionResult via Debug on the unhappy path; the error
        // message must be present so triage doesn't need to re-derive it.
        let result = ExecutionResult::Failed {
            error: "connection refused".to_string(),
            retryable: false,
        };
        let rendered = format!("{result:?}");
        assert!(rendered.contains("connection refused"), "got: {rendered}");
    }
}

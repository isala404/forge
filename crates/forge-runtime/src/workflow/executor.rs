use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use dashmap::DashMap;
use tokio::sync::{Mutex, RwLock};
use uuid::Uuid;

use super::bridge::WORKFLOW_RESUME_JOB;
use super::registry::WorkflowRegistry;
use super::state::{WorkflowRecord, WorkflowStepRecord};
use crate::jobs::{JobQueue, JobRecord};
use forge_core::CircuitBreakerClient;
use forge_core::function::{KvHandle, WorkflowDispatch};
use forge_core::job::JobPriority;
use forge_core::workflow::{CompensationHandler, StepStatus, WorkflowContext, WorkflowStatus};

/// Workflow execution result.
#[derive(Debug)]
pub enum WorkflowResult {
    Completed(serde_json::Value),
    Suspended {
        reason: String,
    },
    Failed {
        error: String,
    },
    Blocked {
        status: forge_core::workflow::WorkflowStatus,
        reason: String,
    },
}

/// In-memory compensation state for a running workflow.
struct CompensationState {
    handlers: HashMap<String, CompensationHandler>,
    completed_steps: Vec<String>,
}

/// State needed when resuming a workflow from sleep/wait.
struct ResumeState {
    started_at: chrono::DateTime<chrono::Utc>,
    from_sleep: bool,
}

/// Executes workflows.
pub struct WorkflowExecutor {
    registry: Arc<WorkflowRegistry>,
    pool: sqlx::PgPool,
    job_queue: JobQueue,
    http_client: CircuitBreakerClient,
    compensation_state: Arc<RwLock<HashMap<Uuid, CompensationState>>>,
    /// Per-run serialization: execute and cancel of the same run_id never
    /// overlap. Without this guard a cancel landing concurrently with a
    /// resume would yank the live `CompensationState` and double-fire
    /// compensation while the handler is still mid-step (#3 in issues doc).
    /// Entry is removed by the holder when the workflow reaches a terminal
    /// state; in-flight holders elsewhere keep their Arc alive.
    run_locks: Arc<DashMap<Uuid, Arc<Mutex<()>>>>,
    kv: Option<Arc<dyn KvHandle>>,
}

impl WorkflowExecutor {
    pub fn new(
        registry: Arc<WorkflowRegistry>,
        pool: sqlx::PgPool,
        job_queue: JobQueue,
        http_client: CircuitBreakerClient,
    ) -> Self {
        Self {
            registry,
            pool,
            job_queue,
            http_client,
            compensation_state: Arc::new(RwLock::new(HashMap::new())),
            run_locks: Arc::new(DashMap::new()),
            kv: None,
        }
    }

    /// Returns an `Arc<Mutex<()>>` keyed by `run_id`, creating one if absent.
    /// Holders take the mutex around any code that touches `compensation_state`
    /// or runs the workflow handler to keep cancel and execute serialized.
    fn run_lock(&self, run_id: Uuid) -> Arc<Mutex<()>> {
        self.run_locks
            .entry(run_id)
            .or_insert_with(|| Arc::new(Mutex::new(())))
            .clone()
    }

    pub fn with_kv(mut self, kv: Arc<dyn KvHandle>) -> Self {
        self.kv = Some(kv);
        self
    }

    /// Start a new workflow on the active version.
    /// Inserts the workflow row and enqueues a `$workflow_resume` job atomically
    /// in a single transaction so a crash between the two cannot leave an
    /// orphaned workflow row without a corresponding resume job.
    pub async fn start<I: serde::Serialize>(
        &self,
        workflow_name: &str,
        input: I,
        owner_subject: Option<String>,
        trace_id: Option<String>,
    ) -> forge_core::Result<Uuid> {
        let entry = self.registry.get_active(workflow_name).ok_or_else(|| {
            forge_core::ForgeError::NotFound(format!(
                "No active version of workflow '{}'",
                workflow_name
            ))
        })?;

        let input_value = serde_json::to_value(input)?;

        let mut record = WorkflowRecord::new(
            workflow_name,
            entry.info.version,
            entry.info.signature,
            input_value,
            owner_subject,
        );
        if let Some(tid) = trace_id {
            record = record.with_trace_id(tid);
        }
        let run_id = record.id;

        let mut tx = self
            .pool
            .begin()
            .await
            .map_err(forge_core::ForgeError::Database)?;

        Self::insert_workflow_record(&mut tx, &record).await?;

        // Enqueue a resume job in the same transaction so the worker only sees
        // it once the workflow row is committed.
        let resume_input = serde_json::json!({
            "run_id": run_id.to_string(),
            "from_sleep": false,
        });
        let job = JobRecord::new(
            WORKFLOW_RESUME_JOB.to_string(),
            resume_input,
            JobPriority::High,
            3,
        )
        .with_capability(forge_core::config::WORKFLOWS_QUEUE);
        self.job_queue
            .enqueue_in_conn(&mut tx, job)
            .await
            .map_err(forge_core::ForgeError::Database)?;

        tx.commit()
            .await
            .map_err(forge_core::ForgeError::Database)?;

        // #15: A DB trigger on `forge_jobs` (`v001_initial.sql`) already
        // PERFORM pg_notify('forge_jobs_available', ...) on every insert.
        // PostgreSQL buffers NOTIFY in the source transaction and delivers on
        // commit, so the resume job is visible to workers as soon as the row
        // is. No extra NOTIFY needed here.

        Ok(run_id)
    }

    async fn execute_workflow(
        &self,
        run_id: Uuid,
        entry: &super::registry::WorkflowEntry,
        input: serde_json::Value,
        resume: Option<ResumeState>,
        owner_subject: Option<String>,
    ) -> forge_core::Result<WorkflowResult> {
        // Serialize against concurrent cancel for the same run.
        let lock = self.run_lock(run_id);
        let _guard = lock.lock().await;

        self.claim_for_execution(run_id).await?;

        let signal_label = if resume.is_some() {
            "workflow_resume"
        } else {
            "workflow"
        };

        let mut ctx = match resume {
            Some(rs) => {
                let step_records = self.get_workflow_steps(run_id).await?;
                let mut step_states = HashMap::new();
                for step in step_records {
                    step_states.insert(
                        step.step_name.clone(),
                        forge_core::workflow::StepState {
                            name: step.step_name,
                            status: step.status,
                            result: step.result,
                            error: step.error,
                            started_at: step.started_at,
                            completed_at: step.completed_at,
                        },
                    );
                }
                let saved_state = self.load_saved_state(run_id).await?;
                let mut c = WorkflowContext::resumed(
                    run_id,
                    entry.info.name.to_string(),
                    rs.started_at,
                    self.pool.clone(),
                    self.http_client.clone(),
                )
                .with_step_states(step_states)
                .with_saved_state(saved_state);
                if rs.from_sleep {
                    c = c.with_resumed_from_sleep();
                }
                c
            }
            None => WorkflowContext::new(
                run_id,
                entry.info.name.to_string(),
                self.pool.clone(),
                self.http_client.clone(),
            ),
        };
        if let Some(ref kv) = self.kv {
            ctx = ctx.with_kv(Arc::clone(kv));
        }
        if let Some(ref subject) = owner_subject {
            let auth = if let Ok(uuid) = uuid::Uuid::parse_str(subject) {
                forge_core::AuthContext::authenticated(
                    uuid,
                    Vec::new(),
                    std::collections::HashMap::new(),
                )
            } else {
                let mut claims = std::collections::HashMap::new();
                claims.insert(
                    "sub".to_string(),
                    serde_json::Value::String(subject.clone()),
                );
                forge_core::AuthContext::authenticated_without_uuid(Vec::new(), claims)
            };
            ctx = ctx.with_auth(auth);
        }
        ctx.set_http_timeout(entry.info.http_timeout);

        let handler = entry.handler.clone();
        let exec_start = std::time::Instant::now();
        // PER-RESUME timeout. `entry.info.timeout` bounds a single resume
        // call, not the whole workflow run. A workflow that sleeps for an
        // hour and then runs for 4m59s under a 5m timeout will pass, even
        // if it suspends and resumes many times — total wall-clock is
        // unbounded by this guard (#4 in issues doc). Tracking total-run
        // budget requires a new column on `forge_workflow_runs`; until
        // then the field name is intentionally treated as `step_timeout`
        // semantics by callers.
        let step_timeout = entry.info.timeout;
        let result = tokio::time::timeout(step_timeout, handler(&ctx, input)).await;
        let exec_duration_ms = exec_start.elapsed().as_millis().min(i32::MAX as u128) as i32;

        let comp = CompensationState {
            handlers: ctx.compensation_handlers(),
            completed_steps: ctx.completed_steps_reversed().into_iter().rev().collect(),
        };
        self.compensation_state.write().await.insert(run_id, comp);

        match result {
            Ok(Ok(output)) => {
                self.complete_workflow(run_id, output.clone()).await?;
                self.compensation_state.write().await.remove(&run_id);
                crate::signals::emit_server_execution(
                    entry.info.name,
                    signal_label,
                    exec_duration_ms,
                    true,
                    None,
                );
                Ok(WorkflowResult::Completed(output))
            }
            Ok(Err(e)) => {
                // Suspension is signaled via a typed error variant. Match on
                // the variant directly — never inspect message text — and
                // handle it before any failure-mapping path so it never
                // surfaces as an internal error to callers.
                if matches!(e, forge_core::ForgeError::WorkflowSuspended(_)) {
                    let suspend_reason = ctx.take_suspend_reason();
                    self.persist_saved_state(run_id, &ctx.take_saved_state())
                        .await?;
                    let reason = match suspend_reason {
                        Some(r) if r.is_sleep() => "sleep".to_string(),
                        Some(r) => r
                            .event_name()
                            .map(|n| format!("waiting_event:{n}"))
                            .unwrap_or_else(|| "suspended".to_string()),
                        None => "suspended".to_string(),
                    };
                    return Ok(WorkflowResult::Suspended { reason });
                }
                let err_str = e.to_string();
                self.fail_workflow(run_id, &err_str).await?;
                crate::signals::emit_server_execution(
                    entry.info.name,
                    signal_label,
                    exec_duration_ms,
                    false,
                    Some(err_str.clone()),
                );
                Ok(WorkflowResult::Failed { error: err_str })
            }
            Err(_) => {
                self.fail_workflow(run_id, "Workflow timed out").await?;
                crate::signals::emit_server_execution(
                    entry.info.name,
                    signal_label,
                    exec_duration_ms,
                    false,
                    Some("Workflow timed out".to_string()),
                );
                Ok(WorkflowResult::Failed {
                    error: "Workflow timed out".to_string(),
                })
            }
        }
    }

    pub async fn resume(&self, run_id: Uuid) -> forge_core::Result<WorkflowResult> {
        self.resume_internal(run_id, false).await
    }

    pub async fn resume_from_sleep(&self, run_id: Uuid) -> forge_core::Result<WorkflowResult> {
        self.resume_internal(run_id, true).await
    }

    async fn resume_internal(
        &self,
        run_id: Uuid,
        from_sleep: bool,
    ) -> forge_core::Result<WorkflowResult> {
        let record = self.get_workflow(run_id).await?;

        // Check if workflow is resumable. `Pending` is included so that runs
        // started inside a transactional mutation — which only INSERT the row
        // and enqueue a `$workflow_resume` job — pick up cleanly when the
        // worker fires the bridge handler post-commit.
        match record.status {
            WorkflowStatus::Pending
            | WorkflowStatus::Running
            | WorkflowStatus::Sleeping
            | WorkflowStatus::Waiting
            | WorkflowStatus::BlockedMissingVersion
            | WorkflowStatus::BlockedSignatureMismatch
            | WorkflowStatus::BlockedMissingHandler => {}
            status if status.is_terminal() => {
                return Err(forge_core::ForgeError::Validation(format!(
                    "Cannot resume workflow in {} state",
                    status.as_str()
                )));
            }
            _ => {}
        }

        match self.registry.validate_resume(
            &record.workflow_name,
            &record.workflow_version,
            &record.workflow_signature,
        ) {
            Ok(entry) => {
                let resume = ResumeState {
                    started_at: record.started_at,
                    from_sleep,
                };
                self.execute_workflow(
                    run_id,
                    entry,
                    record.input,
                    Some(resume),
                    record.owner_subject,
                )
                .await
            }
            Err(reason) => {
                let blocked_status = reason.to_blocked_status();
                let description = reason.description();
                self.block_workflow(run_id, blocked_status, &description)
                    .await?;
                tracing::warn!(
                    workflow_run_id = %run_id,
                    workflow_name = %record.workflow_name,
                    workflow_version = %record.workflow_version,
                    status = %blocked_status.as_str(),
                    reason = %description,
                    "Workflow run blocked (will retry on next deploy)"
                );
                Ok(WorkflowResult::Blocked {
                    status: blocked_status,
                    reason: description,
                })
            }
        }
    }

    pub async fn status(&self, run_id: Uuid) -> forge_core::Result<WorkflowRecord> {
        self.get_workflow(run_id).await
    }

    /// Cancel a workflow and run compensation.
    ///
    /// Compensation follows the saga pattern: steps are undone in reverse order
    /// of their completion. The run ends in Failed with error = "cancelled".
    ///
    /// If compensation handlers are not in memory (e.g. after a process restart),
    /// compensation cannot run. The workflow is failed with a clear reason so
    /// operators know manual remediation is required. This is an honest limitation:
    /// in-memory closures cannot survive restarts.
    pub async fn cancel(&self, run_id: Uuid, reason: &str) -> forge_core::Result<()> {
        // Serialize against a concurrent resume/execute for this run. Without
        // the lock the live handler could be mid-step while we yank its
        // compensation state and flip the row to `failed` (#3 in issues doc).
        let lock = self.run_lock(run_id);
        let _guard = lock.lock().await;

        if let Some(state) = self.compensation_state.write().await.remove(&run_id) {
            let comp_failures = self.run_compensation(run_id, &state).await?;
            // #17/#18: persist the cancel reason in a dedicated column and
            // surface compensation failures as a structured summary in `error`
            // so operators don't have to grep logs to learn which steps need
            // manual remediation.
            let comp_summary = if comp_failures.is_empty() {
                None
            } else {
                Some(format!(
                    "{} compensation(s) failed: {}",
                    comp_failures.len(),
                    comp_failures.join("; ")
                ))
            };
            self.finalize_cancel(run_id, reason, comp_summary.as_deref())
                .await?;
        } else {
            tracing::error!(
                workflow_run_id = %run_id,
                "Compensation handlers lost (process restarted since workflow began); \
                 manual remediation required for any side effects from completed steps"
            );
            self.finalize_cancel(
                run_id,
                reason,
                Some("compensation skipped: handlers lost on restart, manual remediation required"),
            )
            .await?;
        }

        // Run is terminal; drop the per-run mutex entry so the map doesn't
        // accumulate. Holders that captured the Arc before this point keep
        // their own reference alive.
        self.run_locks.remove(&run_id);

        Ok(())
    }

    /// Request asynchronous cancellation of a workflow run.
    ///
    /// Sets `cancel_requested_at` on the row, which fires a NOTIFY on
    /// `forge_workflow_wakeup`. The scheduler picks the row up immediately,
    /// claims it, and dispatches a `$workflow_resume` job that runs
    /// compensation and ends the workflow in `failed` (with `error =
    /// "cancelled: {reason}"`).
    ///
    /// For runs suspended on a long durable sleep this is the only way to
    /// stop them before `wake_at` fires; latency from request to terminal
    /// state is bounded by the NOTIFY round-trip plus one resume-job
    /// dispatch, typically well under 50 ms in steady state.
    ///
    /// Returns `false` if the run is already in a terminal state or no row
    /// matched.
    pub async fn request_cancel(
        &self,
        run_id: Uuid,
        reason: &str,
        caller_subject: Option<&str>,
    ) -> forge_core::Result<bool> {
        // Ownership check parallels `JobQueue::request_cancel`: if the run has
        // an `owner_subject`, the caller must match (or be `None`, which means
        // system / internal). Without this any caller holding the dispatcher
        // could cancel any workflow run by ID (#10 in issues doc).
        //
        // Runtime query — adds a parameter; avoids invalidating .sqlx/.
        #[allow(clippy::disallowed_methods)]
        let result = sqlx::query(
            r#"
            UPDATE forge_workflow_runs
            SET cancel_requested_at = NOW(),
                cancel_reason = $2
            WHERE id = $1
              AND status IN ('pending', 'running', 'sleeping', 'waiting')
              AND cancel_requested_at IS NULL
              AND (
                  owner_subject IS NULL
                  OR $3::text IS NULL
                  OR owner_subject = $3::text
              )
            "#,
        )
        .bind(run_id)
        .bind(reason)
        .bind(caller_subject)
        .execute(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        Ok(result.rows_affected() > 0)
    }

    async fn run_compensation(
        &self,
        run_id: Uuid,
        state: &CompensationState,
    ) -> forge_core::Result<Vec<String>> {
        let steps = self.get_workflow_steps(run_id).await?;
        let mut failures: Vec<String> = Vec::new();

        for step_name in state.completed_steps.iter().rev() {
            if let Some(handler) = state.handlers.get(step_name) {
                let step_result = steps
                    .iter()
                    .find(|s| &s.step_name == step_name)
                    .and_then(|s| s.result.clone())
                    .unwrap_or(serde_json::Value::Null);

                match handler(step_result).await {
                    Ok(()) => {
                        tracing::info!(
                            workflow_run_id = %run_id,
                            step = %step_name,
                            "Compensation completed"
                        );
                        self.update_step_status(run_id, step_name, StepStatus::Compensated)
                            .await?;
                    }
                    Err(e) => {
                        let err_str = e.to_string();
                        tracing::error!(
                            workflow_run_id = %run_id,
                            step = %step_name,
                            error = %err_str,
                            "Compensation failed"
                        );
                        // Tag the step row so operators can see exactly which
                        // compensations failed and need manual remediation
                        // (#18 in issues doc).
                        self.update_step_status_with_error(
                            run_id,
                            step_name,
                            StepStatus::CompensationFailed,
                            Some(&err_str),
                        )
                        .await?;
                        failures.push(format!("{step_name}: {err_str}"));
                    }
                }
            } else {
                self.update_step_status(run_id, step_name, StepStatus::Compensated)
                    .await?;
            }
        }
        Ok(failures)
    }

    async fn get_workflow_steps(
        &self,
        workflow_run_id: Uuid,
    ) -> forge_core::Result<Vec<WorkflowStepRecord>> {
        let rows = sqlx::query!(
            r#"
            SELECT id, workflow_run_id, step_name, status, result, error, started_at, completed_at
            FROM forge_workflow_steps
            WHERE workflow_run_id = $1
            ORDER BY started_at ASC
            "#,
            workflow_run_id,
        )
        .fetch_all(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        rows.into_iter()
            .map(|row| {
                let status = row.status.parse().map_err(|e| {
                    forge_core::ForgeError::internal(format!(
                        "Invalid step status '{}': {}",
                        row.status, e
                    ))
                })?;
                Ok(WorkflowStepRecord {
                    id: row.id,
                    workflow_run_id: row.workflow_run_id,
                    step_name: row.step_name,
                    status,
                    result: row.result,
                    error: row.error,
                    started_at: row.started_at,
                    completed_at: row.completed_at,
                })
            })
            .collect()
    }

    async fn update_step_status(
        &self,
        workflow_run_id: Uuid,
        step_name: &str,
        status: StepStatus,
    ) -> forge_core::Result<()> {
        sqlx::query!(
            r#"
            UPDATE forge_workflow_steps
            SET status = $3
            WHERE workflow_run_id = $1 AND step_name = $2
            "#,
            workflow_run_id,
            step_name,
            status.as_str(),
        )
        .execute(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        Ok(())
    }

    /// Update step status and optionally write an error message. Used for
    /// surfacing `CompensationFailed` so operators can locate stuck side
    /// effects without diving into logs.
    async fn update_step_status_with_error(
        &self,
        workflow_run_id: Uuid,
        step_name: &str,
        status: StepStatus,
        error: Option<&str>,
    ) -> forge_core::Result<()> {
        // forge_workflow_steps is a runtime-owned system table; offline .sqlx
        // cache doesn't always include it.
        #[allow(clippy::disallowed_methods)]
        sqlx::query(
            r#"
            UPDATE forge_workflow_steps
            SET status = $3,
                error = COALESCE($4, error)
            WHERE workflow_run_id = $1 AND step_name = $2
            "#,
        )
        .bind(workflow_run_id)
        .bind(step_name)
        .bind(status.as_str())
        .bind(error)
        .execute(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        Ok(())
    }

    // forge_workflow_runs is a runtime-owned system table; offline .sqlx cache
    // doesn't always include it.
    #[allow(clippy::disallowed_methods)]
    async fn persist_saved_state(
        &self,
        run_id: Uuid,
        state: &std::collections::HashMap<String, serde_json::Value>,
    ) -> forge_core::Result<()> {
        if state.is_empty() {
            return Ok(());
        }
        let json = serde_json::to_value(state)
            .map_err(|e| forge_core::ForgeError::Serialization(e.to_string()))?;
        sqlx::query(
            "INSERT INTO forge_workflow_state (run_id, saved_state, updated_at) \
             VALUES ($1, $2, NOW()) \
             ON CONFLICT (run_id) DO UPDATE SET saved_state = $2, updated_at = NOW()",
        )
        .bind(run_id)
        .bind(json)
        .execute(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;
        Ok(())
    }

    #[allow(clippy::disallowed_methods)]
    async fn load_saved_state(
        &self,
        run_id: Uuid,
    ) -> forge_core::Result<std::collections::HashMap<String, serde_json::Value>> {
        let row: Option<(serde_json::Value,)> =
            sqlx::query_as("SELECT saved_state FROM forge_workflow_state WHERE run_id = $1")
                .bind(run_id)
                .fetch_optional(&self.pool)
                .await
                .map_err(forge_core::ForgeError::Database)?;

        match row {
            Some((json,)) => serde_json::from_value(json)
                .map_err(|e| forge_core::ForgeError::Deserialization(e.to_string())),
            None => Ok(std::collections::HashMap::new()),
        }
    }

    async fn insert_workflow_record(
        conn: &mut sqlx::PgConnection,
        record: &WorkflowRecord,
    ) -> forge_core::Result<()> {
        sqlx::query!(
            r#"
            INSERT INTO forge_workflow_runs (
                id, workflow_name, workflow_version, workflow_signature,
                owner_subject, input, status, current_step,
                started_at, trace_id
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
            "#,
            record.id,
            &record.workflow_name,
            &record.workflow_version,
            &record.workflow_signature,
            record.owner_subject as _,
            record.input as _,
            record.status.as_str(),
            record.current_step as _,
            record.started_at,
            record.trace_id.as_deref(),
        )
        .execute(&mut *conn)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        Ok(())
    }

    async fn get_workflow(&self, run_id: Uuid) -> forge_core::Result<WorkflowRecord> {
        let row = sqlx::query!(
            r#"
            SELECT id, workflow_name, workflow_version, workflow_signature,
                   owner_subject, input, output, status, blocking_reason,
                   resolution_reason, current_step, started_at,
                   completed_at, error, trace_id,
                   cancel_requested_at, cancel_reason
            FROM forge_workflow_runs
            WHERE id = $1
            "#,
            run_id,
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        let row = row.ok_or_else(|| {
            forge_core::ForgeError::NotFound(format!("Workflow run {} not found", run_id))
        })?;

        let status = row.status.parse().map_err(|e| {
            forge_core::ForgeError::internal(format!(
                "Invalid workflow status '{}': {}",
                row.status, e
            ))
        })?;
        Ok(WorkflowRecord {
            id: row.id,
            workflow_name: row.workflow_name,
            workflow_version: row.workflow_version,
            workflow_signature: row.workflow_signature,
            owner_subject: row.owner_subject,
            input: row.input,
            output: row.output,
            status,
            blocking_reason: row.blocking_reason,
            resolution_reason: row.resolution_reason,
            current_step: row.current_step,
            started_at: row.started_at,
            completed_at: row.completed_at,
            error: row.error,
            trace_id: row.trace_id,
            cancel_requested_at: row.cancel_requested_at,
            cancel_reason: row.cancel_reason,
        })
    }

    /// Atomically claim a workflow for execution (transition to Running).
    ///
    /// Rejects `running → running`: a row already in `running` is being
    /// executed by another handler. Re-entering races the live handler's
    /// compensation state and step writes (#2 in the issues doc). The
    /// scheduler claims rows from `(sleeping, waiting)` only; the job-queue's
    /// `FOR UPDATE SKIP LOCKED` plus the per-run advisory lock taken in
    /// `execute_workflow` keep concurrent resumes serialized end-to-end.
    ///
    /// The cancel bridge calls `force_claim_for_cancel` (which permits the
    /// `running → running` transition for compensation) instead of going
    /// through this path.
    async fn claim_for_execution(&self, run_id: Uuid) -> forge_core::Result<()> {
        // Runtime query — schema unchanged, just a tighter status set; avoids
        // touching `.sqlx/` for an internal helper.
        #[allow(clippy::disallowed_methods)]
        let result = sqlx::query(
            "UPDATE forge_workflow_runs SET status = 'running' WHERE id = $1 AND status IN ('pending', 'sleeping', 'waiting')",
        )
        .bind(run_id)
        .execute(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        if result.rows_affected() == 0 {
            return Err(forge_core::ForgeError::InvalidState(format!(
                "Cannot claim workflow {} for execution: invalid state or not found",
                run_id
            )));
        }

        Ok(())
    }

    async fn complete_workflow(
        &self,
        run_id: Uuid,
        output: serde_json::Value,
    ) -> forge_core::Result<()> {
        let result = sqlx::query!(
            "UPDATE forge_workflow_runs SET status = 'completed', output = $1, completed_at = NOW() WHERE id = $2 AND status = 'running'",
            output as _,
            run_id,
        )
        .execute(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        if result.rows_affected() == 0 {
            return Err(forge_core::ForgeError::InvalidState(format!(
                "Cannot complete workflow {}: not in 'running' state",
                run_id
            )));
        }

        Ok(())
    }

    /// Finalize a cancellation in the database.
    ///
    /// Writes the cancel reason to the dedicated `cancel_reason` column rather
    /// than smuggling it into `error`. `error` carries the compensation
    /// failure summary (if any) so operators have a single place to look for
    /// remediation work. Sets `completed_at` so dashboards stop showing the
    /// run as in-flight (#17 in issues doc).
    async fn finalize_cancel(
        &self,
        run_id: Uuid,
        reason: &str,
        compensation_summary: Option<&str>,
    ) -> forge_core::Result<()> {
        // forge_workflow_runs is a runtime-owned system table; offline .sqlx
        // cache doesn't always include it.
        #[allow(clippy::disallowed_methods)]
        let result = sqlx::query(
            r#"
            UPDATE forge_workflow_runs
            SET status = 'failed',
                cancel_reason = COALESCE(cancel_reason, $1),
                error = $2,
                completed_at = NOW()
            WHERE id = $3
              AND status IN ('running', 'sleeping', 'waiting', 'pending')
            "#,
        )
        .bind(reason)
        .bind(compensation_summary)
        .bind(run_id)
        .execute(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        if result.rows_affected() == 0 {
            return Err(forge_core::ForgeError::InvalidState(format!(
                "Cannot finalize cancel for workflow {}: not in a valid state",
                run_id
            )));
        }
        Ok(())
    }

    async fn fail_workflow(&self, run_id: Uuid, error: &str) -> forge_core::Result<()> {
        let result = sqlx::query!(
            "UPDATE forge_workflow_runs SET status = 'failed', error = $1, completed_at = NOW() WHERE id = $2 AND status IN ('running', 'sleeping', 'waiting', 'pending')",
            error,
            run_id,
        )
        .execute(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        if result.rows_affected() == 0 {
            return Err(forge_core::ForgeError::InvalidState(format!(
                "Cannot fail workflow {}: not in a valid state for failure",
                run_id
            )));
        }

        Ok(())
    }

    async fn block_workflow(
        &self,
        run_id: Uuid,
        status: forge_core::workflow::WorkflowStatus,
        reason: &str,
    ) -> forge_core::Result<()> {
        // Uses runtime query because the status value is dynamic and the
        // sqlx offline cache doesn't have an entry for this parameterized form.
        // #19: also set `completed_at` so blocked runs leave the active list
        // (otherwise dashboards treat them as in-flight indefinitely).
        #[allow(clippy::disallowed_methods)]
        sqlx::query(
            "UPDATE forge_workflow_runs SET status = $1, error = $2, completed_at = NOW() WHERE id = $3 AND status IN ('running', 'sleeping', 'waiting', 'pending')",
        )
        .bind(status.as_str())
        .bind(reason)
        .bind(run_id)
        .execute(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        Ok(())
    }

    pub async fn save_step(&self, step: &WorkflowStepRecord) -> forge_core::Result<()> {
        sqlx::query!(
            r#"
            INSERT INTO forge_workflow_steps (
                id, workflow_run_id, step_name, status, result, error, started_at, completed_at
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            ON CONFLICT (workflow_run_id, step_name) DO UPDATE SET
                status = EXCLUDED.status,
                result = EXCLUDED.result,
                error = EXCLUDED.error,
                started_at = COALESCE(forge_workflow_steps.started_at, EXCLUDED.started_at),
                completed_at = EXCLUDED.completed_at
            "#,
            step.id,
            step.workflow_run_id,
            &step.step_name,
            step.status.as_str(),
            step.result as _,
            step.error as _,
            step.started_at,
            step.completed_at,
        )
        .execute(&self.pool)
        .await
        .map_err(forge_core::ForgeError::Database)?;

        Ok(())
    }
}

impl WorkflowDispatch for WorkflowExecutor {
    fn get_info(&self, workflow_name: &str) -> Option<forge_core::workflow::WorkflowInfo> {
        self.registry
            .get_active(workflow_name)
            .map(|e| e.info.clone())
    }

    fn start_by_name(
        &self,
        workflow_name: &str,
        input: serde_json::Value,
        owner_subject: Option<String>,
        trace_id: Option<String>,
    ) -> Pin<Box<dyn Future<Output = forge_core::Result<Uuid>> + Send + '_>> {
        let workflow_name = workflow_name.to_string();
        Box::pin(async move {
            self.start(&workflow_name, input, owner_subject, trace_id)
                .await
        })
    }

    fn start_in_conn<'a>(
        &'a self,
        conn: &'a mut sqlx::PgConnection,
        workflow_name: &'a str,
        input: serde_json::Value,
        owner_subject: Option<String>,
        trace_id: Option<String>,
    ) -> Pin<Box<dyn Future<Output = forge_core::Result<Uuid>> + Send + 'a>> {
        Box::pin(async move {
            let entry = self.registry.get_active(workflow_name).ok_or_else(|| {
                forge_core::ForgeError::NotFound(format!(
                    "No active version of workflow '{}'",
                    workflow_name
                ))
            })?;

            let mut record = WorkflowRecord::new(
                workflow_name,
                entry.info.version,
                entry.info.signature,
                input,
                owner_subject,
            );
            if let Some(tid) = trace_id {
                record = record.with_trace_id(tid);
            }
            let run_id = record.id;

            Self::insert_workflow_record(conn, &record).await?;

            // Enqueue the resume job in the same transaction so the worker
            // only sees it once the mutation commits.
            let resume_input = serde_json::json!({
                "run_id": run_id.to_string(),
                "from_sleep": false,
            });
            let job = JobRecord::new(
                WORKFLOW_RESUME_JOB.to_string(),
                resume_input,
                JobPriority::High,
                3,
            )
            .with_capability(forge_core::config::WORKFLOWS_QUEUE);
            self.job_queue
                .enqueue_in_conn(conn, job)
                .await
                .map_err(forge_core::ForgeError::Database)?;

            Ok(run_id)
        })
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn test_workflow_result_types() {
        let completed = WorkflowResult::Completed(serde_json::json!({}));
        let _suspended = WorkflowResult::Suspended {
            reason: "timer".to_string(),
        };
        let _failed = WorkflowResult::Failed {
            error: "test".to_string(),
        };

        match completed {
            WorkflowResult::Completed(_) => {}
            _ => panic!("Expected Completed"),
        }
    }
}

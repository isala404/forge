use std::collections::HashMap;
use std::sync::Arc;

use tokio::sync::RwLock;
use uuid::Uuid;

use super::registry::WorkflowRegistry;
use super::state::{WorkflowRecord, WorkflowStepRecord};
use forge_core::workflow::{CompensationHandler, StepStatus, WorkflowContext, WorkflowStatus};

/// Workflow execution result.
#[derive(Debug)]
pub enum WorkflowResult {
    /// Workflow completed successfully.
    Completed(serde_json::Value),
    /// Workflow is waiting for an external event.
    Waiting { event_type: String },
    /// Workflow failed.
    Failed { error: String },
    /// Workflow was compensated.
    Compensated,
}

/// Compensation state for a running workflow.
struct CompensationState {
    handlers: HashMap<String, CompensationHandler>,
    completed_steps: Vec<String>,
}

/// Executes workflows.
pub struct WorkflowExecutor {
    registry: Arc<WorkflowRegistry>,
    pool: sqlx::PgPool,
    http_client: reqwest::Client,
    /// Compensation state for active workflows (run_id -> state).
    compensation_state: Arc<RwLock<HashMap<Uuid, CompensationState>>>,
}

impl WorkflowExecutor {
    /// Create a new workflow executor.
    pub fn new(
        registry: Arc<WorkflowRegistry>,
        pool: sqlx::PgPool,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            registry,
            pool,
            http_client,
            compensation_state: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Start a new workflow.
    pub async fn start<I: serde::Serialize>(
        &self,
        workflow_name: &str,
        input: I,
    ) -> forge_core::Result<Uuid> {
        let entry = self.registry.get(workflow_name).ok_or_else(|| {
            forge_core::ForgeError::NotFound(format!("Workflow '{}' not found", workflow_name))
        })?;

        let input_value = serde_json::to_value(input)?;

        let record = WorkflowRecord::new(workflow_name, entry.info.version, input_value.clone());
        let run_id = record.id;

        // Persist workflow record
        self.save_workflow(&record).await?;

        // Execute workflow
        self.execute_workflow(run_id, entry, input_value).await?;

        Ok(run_id)
    }

    /// Execute a workflow.
    async fn execute_workflow(
        &self,
        run_id: Uuid,
        entry: &super::registry::WorkflowEntry,
        input: serde_json::Value,
    ) -> forge_core::Result<WorkflowResult> {
        // Update status to running
        self.update_workflow_status(run_id, WorkflowStatus::Running)
            .await?;

        // Create workflow context
        let ctx = WorkflowContext::new(
            run_id,
            entry.info.name.to_string(),
            entry.info.version,
            self.pool.clone(),
            self.http_client.clone(),
        );

        // Execute workflow with timeout
        let handler = entry.handler.clone();
        let result = tokio::time::timeout(entry.info.timeout, handler(&ctx, input)).await;

        // Capture compensation state after execution
        let compensation_state = CompensationState {
            handlers: ctx.compensation_handlers(),
            completed_steps: ctx.completed_steps_reversed().into_iter().rev().collect(),
        };
        self.compensation_state
            .write()
            .await
            .insert(run_id, compensation_state);

        match result {
            Ok(Ok(output)) => {
                // Mark as completed, clean up compensation state
                self.complete_workflow(run_id, output.clone()).await?;
                self.compensation_state.write().await.remove(&run_id);
                Ok(WorkflowResult::Completed(output))
            }
            Ok(Err(e)) => {
                // Mark as failed - compensation can be triggered via cancel
                self.fail_workflow(run_id, &e.to_string()).await?;
                Ok(WorkflowResult::Failed {
                    error: e.to_string(),
                })
            }
            Err(_) => {
                // Timeout
                self.fail_workflow(run_id, "Workflow timed out").await?;
                Ok(WorkflowResult::Failed {
                    error: "Workflow timed out".to_string(),
                })
            }
        }
    }

    /// Resume a workflow from where it left off.
    pub async fn resume(&self, run_id: Uuid) -> forge_core::Result<WorkflowResult> {
        let record = self.get_workflow(run_id).await?;

        let entry = self.registry.get(&record.workflow_name).ok_or_else(|| {
            forge_core::ForgeError::NotFound(format!(
                "Workflow '{}' not found",
                record.workflow_name
            ))
        })?;

        // Check if workflow is resumable
        match record.status {
            WorkflowStatus::Running | WorkflowStatus::Waiting => {
                // Can resume
            }
            status if status.is_terminal() => {
                return Err(forge_core::ForgeError::Validation(format!(
                    "Cannot resume workflow in {} state",
                    status.as_str()
                )));
            }
            _ => {}
        }

        self.execute_workflow(run_id, entry, record.input).await
    }

    /// Get workflow status.
    pub async fn status(&self, run_id: Uuid) -> forge_core::Result<WorkflowRecord> {
        self.get_workflow(run_id).await
    }

    /// Cancel a workflow and run compensation.
    pub async fn cancel(&self, run_id: Uuid) -> forge_core::Result<()> {
        self.update_workflow_status(run_id, WorkflowStatus::Compensating)
            .await?;

        // Get compensation state
        let state = self.compensation_state.write().await.remove(&run_id);

        if let Some(state) = state {
            // Get completed steps with results from database
            let steps = self.get_workflow_steps(run_id).await?;

            // Run compensation in reverse order
            for step_name in state.completed_steps.iter().rev() {
                if let Some(handler) = state.handlers.get(step_name) {
                    // Find the step result
                    let step_result = steps
                        .iter()
                        .find(|s| &s.step_name == step_name)
                        .and_then(|s| s.result.clone())
                        .unwrap_or(serde_json::Value::Null);

                    // Run compensation handler
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
                            tracing::error!(
                                workflow_run_id = %run_id,
                                step = %step_name,
                                error = %e,
                                "Compensation failed"
                            );
                            // Continue with other compensations even if one fails
                        }
                    }
                } else {
                    // No handler, just mark as compensated
                    self.update_step_status(run_id, step_name, StepStatus::Compensated)
                        .await?;
                }
            }
        } else {
            // No in-memory state, try to compensate from DB state
            // This handles the case where the server restarted
            tracing::warn!(
                workflow_run_id = %run_id,
                "No compensation state found, marking as compensated without handlers"
            );
        }

        self.update_workflow_status(run_id, WorkflowStatus::Compensated)
            .await?;

        Ok(())
    }

    /// Get workflow steps from database.
    async fn get_workflow_steps(
        &self,
        workflow_run_id: Uuid,
    ) -> forge_core::Result<Vec<WorkflowStepRecord>> {
        let rows = sqlx::query(
            r#"
            SELECT id, workflow_run_id, step_name, status, result, error, started_at, completed_at
            FROM forge_workflow_steps
            WHERE workflow_run_id = $1
            ORDER BY started_at ASC
            "#,
        )
        .bind(workflow_run_id)
        .fetch_all(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        use sqlx::Row;
        Ok(rows
            .into_iter()
            .map(|row| WorkflowStepRecord {
                id: row.get("id"),
                workflow_run_id: row.get("workflow_run_id"),
                step_name: row.get("step_name"),
                status: StepStatus::from_str(row.get("status")),
                result: row.get("result"),
                error: row.get("error"),
                started_at: row.get("started_at"),
                completed_at: row.get("completed_at"),
            })
            .collect())
    }

    /// Update step status.
    async fn update_step_status(
        &self,
        workflow_run_id: Uuid,
        step_name: &str,
        status: StepStatus,
    ) -> forge_core::Result<()> {
        sqlx::query(
            r#"
            UPDATE forge_workflow_steps
            SET status = $3
            WHERE workflow_run_id = $1 AND step_name = $2
            "#,
        )
        .bind(workflow_run_id)
        .bind(step_name)
        .bind(status.as_str())
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(())
    }

    /// Save workflow record to database.
    async fn save_workflow(&self, record: &WorkflowRecord) -> forge_core::Result<()> {
        sqlx::query(
            r#"
            INSERT INTO forge_workflow_runs (
                id, workflow_name, input, status, current_step,
                step_results, started_at, trace_id
            ) VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
            "#,
        )
        .bind(record.id)
        .bind(&record.workflow_name)
        .bind(&record.input)
        .bind(record.status.as_str())
        .bind(&record.current_step)
        .bind(&record.step_results)
        .bind(record.started_at)
        .bind(&record.trace_id)
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(())
    }

    /// Get workflow record from database.
    async fn get_workflow(&self, run_id: Uuid) -> forge_core::Result<WorkflowRecord> {
        let row = sqlx::query(
            r#"
            SELECT id, workflow_name, input, output, status, current_step,
                   step_results, started_at, completed_at, error, trace_id
            FROM forge_workflow_runs
            WHERE id = $1
            "#,
        )
        .bind(run_id)
        .fetch_optional(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        let row = row.ok_or_else(|| {
            forge_core::ForgeError::NotFound(format!("Workflow run {} not found", run_id))
        })?;

        use sqlx::Row;
        Ok(WorkflowRecord {
            id: row.get("id"),
            workflow_name: row.get("workflow_name"),
            version: 1, // TODO: Add version column
            input: row.get("input"),
            output: row.get("output"),
            status: WorkflowStatus::from_str(row.get("status")),
            current_step: row.get("current_step"),
            step_results: row.get("step_results"),
            started_at: row.get("started_at"),
            completed_at: row.get("completed_at"),
            error: row.get("error"),
            trace_id: row.get("trace_id"),
        })
    }

    /// Update workflow status.
    async fn update_workflow_status(
        &self,
        run_id: Uuid,
        status: WorkflowStatus,
    ) -> forge_core::Result<()> {
        sqlx::query(
            r#"
            UPDATE forge_workflow_runs
            SET status = $2
            WHERE id = $1
            "#,
        )
        .bind(run_id)
        .bind(status.as_str())
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(())
    }

    /// Mark workflow as completed.
    async fn complete_workflow(
        &self,
        run_id: Uuid,
        output: serde_json::Value,
    ) -> forge_core::Result<()> {
        sqlx::query(
            r#"
            UPDATE forge_workflow_runs
            SET status = 'completed', output = $2, completed_at = NOW()
            WHERE id = $1
            "#,
        )
        .bind(run_id)
        .bind(output)
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(())
    }

    /// Mark workflow as failed.
    async fn fail_workflow(&self, run_id: Uuid, error: &str) -> forge_core::Result<()> {
        sqlx::query(
            r#"
            UPDATE forge_workflow_runs
            SET status = 'failed', error = $2, completed_at = NOW()
            WHERE id = $1
            "#,
        )
        .bind(run_id)
        .bind(error)
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(())
    }

    /// Save step record.
    pub async fn save_step(&self, step: &WorkflowStepRecord) -> forge_core::Result<()> {
        sqlx::query(
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
        )
        .bind(step.id)
        .bind(step.workflow_run_id)
        .bind(&step.step_name)
        .bind(step.status.as_str())
        .bind(&step.result)
        .bind(&step.error)
        .bind(step.started_at)
        .bind(step.completed_at)
        .execute(&self.pool)
        .await
        .map_err(|e| forge_core::ForgeError::Database(e.to_string()))?;

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workflow_result_types() {
        let completed = WorkflowResult::Completed(serde_json::json!({}));
        let waiting = WorkflowResult::Waiting {
            event_type: "approval".to_string(),
        };
        let failed = WorkflowResult::Failed {
            error: "test".to_string(),
        };
        let compensated = WorkflowResult::Compensated;

        // Just ensure they can be created
        match completed {
            WorkflowResult::Completed(_) => {}
            _ => panic!("Expected Completed"),
        }
    }
}

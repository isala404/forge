use std::sync::Arc;
use std::time::Duration;

use forge_core::job::{JobContext, JobInfo};
use serde_json::Value;

use super::executor::WorkflowExecutor;
use crate::jobs::registry::{BoxedJobHandler, JobRegistry};

/// Job type for workflow resume operations.
pub const WORKFLOW_RESUME_JOB: &str = "$workflow_resume";

/// Register the `$workflow_resume` bridge handler in the job registry.
///
/// The workflow scheduler enqueues these jobs when a suspended workflow is
/// ready to continue (timer expired, event received, or event timeout).
/// The worker pool picks them up and calls the executor's resume methods.
pub fn register_workflow_bridge(executor: Arc<WorkflowExecutor>, job_registry: &mut JobRegistry) {
    let handler: BoxedJobHandler = Arc::new(move |_ctx: &JobContext, args: Value| {
        let executor = executor.clone();
        Box::pin(async move {
            let run_id: uuid::Uuid = args
                .get("run_id")
                .and_then(Value::as_str)
                .and_then(|s| uuid::Uuid::parse_str(s).ok())
                .ok_or_else(|| {
                    forge_core::ForgeError::InvalidArgument(
                        "Missing or invalid run_id in workflow resume job".to_string(),
                    )
                })?;

            let from_sleep: bool = args
                .get("from_sleep")
                .and_then(Value::as_bool)
                .unwrap_or(false);

            let cancel: bool = args.get("cancel").and_then(Value::as_bool).unwrap_or(false);

            if cancel {
                let reason = args
                    .get("reason")
                    .and_then(Value::as_str)
                    .unwrap_or("operator cancel")
                    .to_string();
                return match executor.cancel(run_id, &reason).await {
                    Ok(()) => Ok(Value::Null),
                    Err(e) => {
                        tracing::warn!(
                            workflow_run_id = %run_id,
                            error = %e,
                            "Workflow cancel finalization failed"
                        );
                        Err(e)
                    }
                };
            }

            let result = if from_sleep {
                executor.resume_from_sleep(run_id).await
            } else {
                executor.resume(run_id).await
            };

            match result {
                Ok(_) => Ok(Value::Null),
                Err(e) => {
                    tracing::warn!(
                        workflow_run_id = %run_id,
                        from_sleep,
                        error = %e,
                        "Workflow resume failed"
                    );
                    Err(e)
                }
            }
        })
    });

    let info = JobInfo {
        name: WORKFLOW_RESUME_JOB,
        timeout: Duration::from_secs(3600),
        ..JobInfo::default()
    };

    job_registry.register_system(WORKFLOW_RESUME_JOB.to_string(), info, handler);
}

use std::future::Future;
use std::pin::Pin;

use uuid::Uuid;

use crate::error::Result;
use crate::job::JobInfo;
use crate::workflow::WorkflowInfo;

/// Trait for dispatching jobs from function contexts.
///
/// This trait allows mutation and action contexts to dispatch background jobs
/// without directly depending on the runtime's JobDispatcher.
pub trait JobDispatch: Send + Sync {
    /// Get job info by name for auth checking.
    fn get_info(&self, job_type: &str) -> Option<JobInfo>;

    /// Dispatch a job by its registered name.
    ///
    /// # Arguments
    /// * `job_type` - The registered name of the job type
    /// * `args` - JSON-serialized arguments for the job
    ///
    /// # Returns
    /// The UUID of the dispatched job
    fn dispatch_by_name(
        &self,
        job_type: &str,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<Uuid>> + Send + '_>>;

    /// Request cancellation for a job.
    fn cancel(
        &self,
        job_id: Uuid,
        reason: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<bool>> + Send + '_>>;
}

/// Trait for starting workflows from function contexts.
///
/// This trait allows mutation and action contexts to start workflows
/// without directly depending on the runtime's WorkflowExecutor.
pub trait WorkflowDispatch: Send + Sync {
    /// Get workflow info by name for auth checking.
    fn get_info(&self, workflow_name: &str) -> Option<WorkflowInfo>;

    /// Start a workflow by its registered name.
    ///
    /// # Arguments
    /// * `workflow_name` - The registered name of the workflow
    /// * `input` - JSON-serialized input for the workflow
    ///
    /// # Returns
    /// The UUID of the started workflow run
    fn start_by_name(
        &self,
        workflow_name: &str,
        input: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<Uuid>> + Send + '_>>;
}

use std::future::Future;
use std::pin::Pin;

use uuid::Uuid;

use crate::error::Result;

/// Trait for dispatching jobs from function contexts.
///
/// This trait allows mutation and action contexts to dispatch background jobs
/// without directly depending on the runtime's JobDispatcher.
pub trait JobDispatch: Send + Sync {
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
}

/// Trait for starting workflows from function contexts.
///
/// This trait allows mutation and action contexts to start workflows
/// without directly depending on the runtime's WorkflowExecutor.
pub trait WorkflowDispatch: Send + Sync {
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

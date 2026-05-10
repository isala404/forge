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
        owner_subject: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<Uuid>> + Send + '_>>;

    /// Dispatch a job on an existing connection — typically the live
    /// transaction inside a `MutationContext`. The insert participates in
    /// the surrounding transaction, so the job only becomes visible to
    /// workers after commit and is rolled back on failure.
    fn dispatch_in_conn<'a>(
        &'a self,
        conn: &'a mut sqlx::PgConnection,
        job_type: &'a str,
        args: serde_json::Value,
        owner_subject: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<Uuid>> + Send + 'a>>;

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
    /// * `trace_id` - Trace identifier from the caller's request, propagated
    ///   onto the run row so observability links request → workflow.
    fn start_by_name(
        &self,
        workflow_name: &str,
        input: serde_json::Value,
        owner_subject: Option<String>,
        trace_id: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<Uuid>> + Send + '_>>;

    /// Start a workflow on an existing connection — typically the live
    /// transaction inside a `MutationContext`. The run row and its
    /// `$workflow_resume` job are written in the same transaction so the
    /// worker only picks the run up after commit.
    fn start_in_conn<'a>(
        &'a self,
        conn: &'a mut sqlx::PgConnection,
        workflow_name: &'a str,
        input: serde_json::Value,
        owner_subject: Option<String>,
        trace_id: Option<String>,
    ) -> Pin<Box<dyn Future<Output = Result<Uuid>> + Send + 'a>>;
}

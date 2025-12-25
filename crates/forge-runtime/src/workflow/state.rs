use chrono::{DateTime, Utc};
use forge_core::workflow::{StepStatus, WorkflowStatus};
use uuid::Uuid;

/// A workflow run record in the database.
#[derive(Debug, Clone)]
pub struct WorkflowRecord {
    /// Unique workflow run ID.
    pub id: Uuid,
    /// Workflow name.
    pub workflow_name: String,
    /// Workflow version.
    pub version: u32,
    /// Input data as JSON.
    pub input: serde_json::Value,
    /// Output data as JSON (if completed).
    pub output: Option<serde_json::Value>,
    /// Current status.
    pub status: WorkflowStatus,
    /// Current step name.
    pub current_step: Option<String>,
    /// Step results as JSON map.
    pub step_results: serde_json::Value,
    /// When the workflow started.
    pub started_at: DateTime<Utc>,
    /// When the workflow completed.
    pub completed_at: Option<DateTime<Utc>>,
    /// Error message if failed.
    pub error: Option<String>,
    /// Trace ID for distributed tracing.
    pub trace_id: Option<String>,
}

impl WorkflowRecord {
    /// Create a new workflow record.
    pub fn new(workflow_name: impl Into<String>, version: u32, input: serde_json::Value) -> Self {
        Self {
            id: Uuid::new_v4(),
            workflow_name: workflow_name.into(),
            version,
            input,
            output: None,
            status: WorkflowStatus::Created,
            current_step: None,
            step_results: serde_json::json!({}),
            started_at: Utc::now(),
            completed_at: None,
            error: None,
            trace_id: None,
        }
    }

    /// Set trace ID.
    pub fn with_trace_id(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self
    }

    /// Mark as running.
    pub fn start(&mut self) {
        self.status = WorkflowStatus::Running;
    }

    /// Mark as completed.
    pub fn complete(&mut self, output: serde_json::Value) {
        self.status = WorkflowStatus::Completed;
        self.output = Some(output);
        self.completed_at = Some(Utc::now());
    }

    /// Mark as failed.
    pub fn fail(&mut self, error: impl Into<String>) {
        self.status = WorkflowStatus::Failed;
        self.error = Some(error.into());
        self.completed_at = Some(Utc::now());
    }

    /// Mark as compensating.
    pub fn compensating(&mut self) {
        self.status = WorkflowStatus::Compensating;
    }

    /// Mark as compensated.
    pub fn compensated(&mut self) {
        self.status = WorkflowStatus::Compensated;
        self.completed_at = Some(Utc::now());
    }

    /// Update current step.
    pub fn set_current_step(&mut self, step: impl Into<String>) {
        self.current_step = Some(step.into());
    }

    /// Add step result.
    pub fn add_step_result(&mut self, step_name: &str, result: serde_json::Value) {
        if let Some(obj) = self.step_results.as_object_mut() {
            obj.insert(step_name.to_string(), result);
        }
    }
}

/// A workflow step record in the database.
#[derive(Debug, Clone)]
pub struct WorkflowStepRecord {
    /// Step record ID.
    pub id: Uuid,
    /// Parent workflow run ID.
    pub workflow_run_id: Uuid,
    /// Step name.
    pub step_name: String,
    /// Step status.
    pub status: StepStatus,
    /// Step result as JSON.
    pub result: Option<serde_json::Value>,
    /// Error message if failed.
    pub error: Option<String>,
    /// When the step started.
    pub started_at: Option<DateTime<Utc>>,
    /// When the step completed.
    pub completed_at: Option<DateTime<Utc>>,
}

impl WorkflowStepRecord {
    /// Create a new step record.
    pub fn new(workflow_run_id: Uuid, step_name: impl Into<String>) -> Self {
        Self {
            id: Uuid::new_v4(),
            workflow_run_id,
            step_name: step_name.into(),
            status: StepStatus::Pending,
            result: None,
            error: None,
            started_at: None,
            completed_at: None,
        }
    }

    /// Mark as running.
    pub fn start(&mut self) {
        self.status = StepStatus::Running;
        self.started_at = Some(Utc::now());
    }

    /// Mark as completed.
    pub fn complete(&mut self, result: serde_json::Value) {
        self.status = StepStatus::Completed;
        self.result = Some(result);
        self.completed_at = Some(Utc::now());
    }

    /// Mark as failed.
    pub fn fail(&mut self, error: impl Into<String>) {
        self.status = StepStatus::Failed;
        self.error = Some(error.into());
        self.completed_at = Some(Utc::now());
    }

    /// Mark as compensated.
    pub fn compensate(&mut self) {
        self.status = StepStatus::Compensated;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workflow_record_creation() {
        let record = WorkflowRecord::new("test_workflow", 1, serde_json::json!({}));
        assert_eq!(record.workflow_name, "test_workflow");
        assert_eq!(record.version, 1);
        assert_eq!(record.status, WorkflowStatus::Created);
    }

    #[test]
    fn test_workflow_record_transitions() {
        let mut record = WorkflowRecord::new("test", 1, serde_json::json!({}));

        record.start();
        assert_eq!(record.status, WorkflowStatus::Running);

        record.complete(serde_json::json!({"result": "ok"}));
        assert_eq!(record.status, WorkflowStatus::Completed);
        assert!(record.completed_at.is_some());
    }

    #[test]
    fn test_workflow_step_record() {
        let workflow_id = Uuid::new_v4();
        let mut step = WorkflowStepRecord::new(workflow_id, "step1");

        assert_eq!(step.step_name, "step1");
        assert_eq!(step.status, StepStatus::Pending);

        step.start();
        assert_eq!(step.status, StepStatus::Running);

        step.complete(serde_json::json!({}));
        assert_eq!(step.status, StepStatus::Completed);
    }
}

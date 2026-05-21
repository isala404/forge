use chrono::{DateTime, Utc};
use forge_core::workflow::{StepStatus, WorkflowStatus};
use uuid::Uuid;

/// A workflow run record in the database.
#[derive(Debug, Clone)]
pub struct WorkflowRecord {
    pub id: Uuid,
    pub workflow_name: String,
    pub workflow_version: String,
    pub workflow_signature: String,
    pub owner_subject: Option<String>,
    pub input: serde_json::Value,
    pub output: Option<serde_json::Value>,
    pub status: WorkflowStatus,
    pub blocking_reason: Option<String>,
    pub resolution_reason: Option<String>,
    pub current_step: Option<String>,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub trace_id: Option<String>,
    pub cancel_requested_at: Option<DateTime<Utc>>,
    pub cancel_reason: Option<String>,
}

impl WorkflowRecord {
    pub fn new(
        workflow_name: impl Into<String>,
        workflow_version: impl Into<String>,
        workflow_signature: impl Into<String>,
        input: serde_json::Value,
        owner_subject: Option<String>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            workflow_name: workflow_name.into(),
            workflow_version: workflow_version.into(),
            workflow_signature: workflow_signature.into(),
            owner_subject,
            input,
            output: None,
            status: WorkflowStatus::Pending,
            blocking_reason: None,
            resolution_reason: None,
            current_step: None,
            started_at: Utc::now(),
            completed_at: None,
            error: None,
            trace_id: None,
            cancel_requested_at: None,
            cancel_reason: None,
        }
    }

    pub fn with_trace_id(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self
    }
}

/// A workflow step record in the database.
#[derive(Debug, Clone)]
pub struct WorkflowStepRecord {
    pub id: Uuid,
    pub workflow_run_id: Uuid,
    pub step_name: String,
    pub status: StepStatus,
    pub result: Option<serde_json::Value>,
    pub error: Option<String>,
    pub started_at: Option<DateTime<Utc>>,
    pub completed_at: Option<DateTime<Utc>>,
}

impl WorkflowStepRecord {
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
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn test_workflow_record_creation() {
        let record =
            WorkflowRecord::new("test_workflow", "v1", "abc123", serde_json::json!({}), None);
        assert_eq!(record.workflow_name, "test_workflow");
        assert_eq!(record.workflow_version, "v1");
        assert_eq!(record.workflow_signature, "abc123");
        assert_eq!(record.status, WorkflowStatus::Pending);
    }

    #[test]
    fn test_workflow_record_with_trace_id() {
        let record = WorkflowRecord::new("test", "v1", "sig", serde_json::json!({}), None)
            .with_trace_id("trace-abc-123");
        assert_eq!(record.trace_id.as_deref(), Some("trace-abc-123"));
    }

    #[test]
    fn test_workflow_record_with_owner() {
        let record = WorkflowRecord::new(
            "onboarding",
            "v1",
            "sig",
            serde_json::json!({}),
            Some("user-alice".into()),
        );
        assert_eq!(record.owner_subject.as_deref(), Some("user-alice"));
    }

    #[test]
    fn test_step_record_defaults() {
        let step = WorkflowStepRecord::new(Uuid::new_v4(), "charge_card");
        assert_eq!(step.step_name, "charge_card");
        assert_eq!(step.status, StepStatus::Pending);
        assert!(step.started_at.is_none());
    }
}

use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::time::Duration;

use serde::{de::DeserializeOwned, Serialize};

use super::context::WorkflowContext;
use crate::Result;

/// Trait for workflow handlers.
pub trait ForgeWorkflow: Send + Sync + 'static {
    /// Input type for the workflow.
    type Input: DeserializeOwned + Serialize + Send + Sync;
    /// Output type for the workflow.
    type Output: Serialize + Send;

    /// Get workflow metadata.
    fn info() -> WorkflowInfo;

    /// Execute the workflow.
    fn execute(
        ctx: &WorkflowContext,
        input: Self::Input,
    ) -> Pin<Box<dyn Future<Output = Result<Self::Output>> + Send + '_>>;
}

/// Workflow metadata.
#[derive(Debug, Clone)]
pub struct WorkflowInfo {
    /// Workflow name.
    pub name: &'static str,
    /// Workflow version.
    pub version: u32,
    /// Default timeout for the entire workflow.
    pub timeout: Duration,
    /// Whether the workflow is deprecated.
    pub deprecated: bool,
}

impl Default for WorkflowInfo {
    fn default() -> Self {
        Self {
            name: "",
            version: 1,
            timeout: Duration::from_secs(86400), // 24 hours
            deprecated: false,
        }
    }
}

/// Workflow execution status.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowStatus {
    /// Workflow is created but not started.
    Created,
    /// Workflow is actively running.
    Running,
    /// Workflow is waiting for an external event.
    Waiting,
    /// Workflow completed successfully.
    Completed,
    /// Workflow failed and is running compensation.
    Compensating,
    /// Workflow compensation completed.
    Compensated,
    /// Workflow failed (compensation also failed or not available).
    Failed,
}

impl WorkflowStatus {
    /// Convert to string for database storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Created => "created",
            Self::Running => "running",
            Self::Waiting => "waiting",
            Self::Completed => "completed",
            Self::Compensating => "compensating",
            Self::Compensated => "compensated",
            Self::Failed => "failed",
        }
    }

    /// Check if the workflow is terminal (no longer running).
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Compensated | Self::Failed)
    }
}

impl FromStr for WorkflowStatus {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        Ok(match s {
            "created" => Self::Created,
            "running" => Self::Running,
            "waiting" => Self::Waiting,
            "completed" => Self::Completed,
            "compensating" => Self::Compensating,
            "compensated" => Self::Compensated,
            "failed" => Self::Failed,
            _ => Self::Created,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_workflow_info_default() {
        let info = WorkflowInfo::default();
        assert_eq!(info.name, "");
        assert_eq!(info.version, 1);
        assert!(!info.deprecated);
    }

    #[test]
    fn test_workflow_status_conversion() {
        assert_eq!(WorkflowStatus::Running.as_str(), "running");
        assert_eq!(WorkflowStatus::Completed.as_str(), "completed");
        assert_eq!(WorkflowStatus::Compensating.as_str(), "compensating");

        assert_eq!(
            "running".parse::<WorkflowStatus>(),
            Ok(WorkflowStatus::Running)
        );
        assert_eq!(
            "completed".parse::<WorkflowStatus>(),
            Ok(WorkflowStatus::Completed)
        );
    }

    #[test]
    fn test_workflow_status_is_terminal() {
        assert!(!WorkflowStatus::Running.is_terminal());
        assert!(!WorkflowStatus::Waiting.is_terminal());
        assert!(WorkflowStatus::Completed.is_terminal());
        assert!(WorkflowStatus::Failed.is_terminal());
        assert!(WorkflowStatus::Compensated.is_terminal());
    }
}

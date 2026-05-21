use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::time::Duration;

use serde::{Serialize, de::DeserializeOwned};

use super::context::WorkflowContext;
use crate::Result;

/// Trait for durable workflow handlers.
pub trait ForgeWorkflow: crate::__sealed::Sealed + Send + Sync + 'static {
    type Input: DeserializeOwned + Serialize + Send + Sync;
    type Output: Serialize + Send;

    fn info() -> WorkflowInfo;

    fn execute(
        ctx: &WorkflowContext,
        input: Self::Input,
    ) -> Pin<Box<dyn Future<Output = Result<Self::Output>> + Send + '_>>;
}

/// Lifecycle state of a workflow definition version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum WorkflowDefStatus {
    #[default]
    Active,
    Deprecated,
    Staging,
}

impl WorkflowDefStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Deprecated => "deprecated",
            Self::Staging => "staging",
        }
    }

    pub fn is_active(self) -> bool {
        matches!(self, Self::Active)
    }

    pub fn is_deprecated(self) -> bool {
        matches!(self, Self::Deprecated)
    }
}

/// Metadata for a registered workflow handler.
#[derive(Debug, Clone)]
pub struct WorkflowInfo {
    pub name: &'static str,
    pub version: &'static str,
    /// Blake3 hash of the persisted contract (name, version, step/wait keys, types).
    pub signature: &'static str,
    pub status: WorkflowDefStatus,
    pub timeout: Duration,
    pub http_timeout: Option<Duration>,
    pub is_public: bool,
    pub required_role: Option<&'static str>,
}

impl WorkflowInfo {
    pub fn is_active(&self) -> bool {
        self.status.is_active()
    }

    pub fn is_deprecated(&self) -> bool {
        self.status.is_deprecated()
    }
}

impl Default for WorkflowInfo {
    fn default() -> Self {
        Self {
            name: "",
            version: "v1",
            signature: "",
            status: WorkflowDefStatus::Active,
            timeout: Duration::from_secs(86400), // 24 hours
            http_timeout: None,
            is_public: false,
            required_role: None,
        }
    }
}

/// Workflow execution status. Blocked variants are non-terminal.
///
/// `Compensating` is non-terminal — the run is actively unwinding completed
/// steps. `Compensated`, `Retired`, and `CancelledByOperator` are terminal:
/// they each describe a distinct end state that callers must be able to
/// distinguish from a generic `Failed`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum WorkflowStatus {
    Pending,
    Running,
    Sleeping,
    Waiting,
    Completed,
    Failed,
    /// Compensation is running (reverse-order rollback of completed steps).
    /// Non-terminal — the run will transition to `Compensated` (or `Failed`
    /// if a handler errored).
    Compensating,
    /// Compensation finished successfully; the run is rolled back and done.
    Compensated,
    /// Operator marked the run as unresumable (e.g., schema drift after a
    /// failed deploy). Terminal — manual intervention required to re-run.
    Retired,
    /// Operator force-aborted the run. Terminal — distinct from `Failed`
    /// because the cause was external, not the workflow itself.
    CancelledByOperator,
    BlockedMissingVersion,
    BlockedSignatureMismatch,
    BlockedMissingHandler,
}

impl WorkflowStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Sleeping => "sleeping",
            Self::Waiting => "waiting",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Compensating => "compensating",
            Self::Compensated => "compensated",
            Self::Retired => "retired_unresumable",
            Self::CancelledByOperator => "cancelled_by_operator",
            Self::BlockedMissingVersion => "blocked_missing_version",
            Self::BlockedSignatureMismatch => "blocked_signature_mismatch",
            Self::BlockedMissingHandler => "blocked_missing_handler",
        }
    }

    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed
                | Self::Failed
                | Self::Compensated
                | Self::Retired
                | Self::CancelledByOperator
        )
    }

    pub fn is_blocked(&self) -> bool {
        matches!(
            self,
            Self::BlockedMissingVersion
                | Self::BlockedSignatureMismatch
                | Self::BlockedMissingHandler
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseWorkflowStatusError(pub String);

impl std::fmt::Display for ParseWorkflowStatusError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid workflow status: '{}'", self.0)
    }
}

impl std::error::Error for ParseWorkflowStatusError {}

impl FromStr for WorkflowStatus {
    type Err = ParseWorkflowStatusError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s {
            "pending" | "created" => Ok(Self::Pending),
            "running" => Ok(Self::Running),
            "sleeping" => Ok(Self::Sleeping),
            "waiting" => Ok(Self::Waiting),
            "completed" => Ok(Self::Completed),
            "failed" => Ok(Self::Failed),
            "compensating" => Ok(Self::Compensating),
            "compensated" => Ok(Self::Compensated),
            "retired_unresumable" => Ok(Self::Retired),
            "cancelled_by_operator" => Ok(Self::CancelledByOperator),
            "blocked_missing_version" => Ok(Self::BlockedMissingVersion),
            "blocked_signature_mismatch" => Ok(Self::BlockedSignatureMismatch),
            "blocked_missing_handler" => Ok(Self::BlockedMissingHandler),
            _ => Err(ParseWorkflowStatusError(s.to_string())),
        }
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn test_workflow_info_default() {
        let info = WorkflowInfo::default();
        assert_eq!(info.name, "");
        assert_eq!(info.version, "v1");
        assert_eq!(info.status, WorkflowDefStatus::Active);
        assert!(info.is_active());
        assert!(!info.is_deprecated());
    }

    #[test]
    fn test_workflow_status_conversion() {
        assert_eq!(WorkflowStatus::Pending.as_str(), "pending");
        assert_eq!(WorkflowStatus::Running.as_str(), "running");
        assert_eq!(WorkflowStatus::Sleeping.as_str(), "sleeping");
        assert_eq!(WorkflowStatus::Waiting.as_str(), "waiting");
        assert_eq!(WorkflowStatus::Completed.as_str(), "completed");
        assert_eq!(WorkflowStatus::Failed.as_str(), "failed");

        assert_eq!(
            "pending".parse::<WorkflowStatus>(),
            Ok(WorkflowStatus::Pending)
        );
        assert_eq!(
            "running".parse::<WorkflowStatus>(),
            Ok(WorkflowStatus::Running)
        );
        assert_eq!(
            "sleeping".parse::<WorkflowStatus>(),
            Ok(WorkflowStatus::Sleeping)
        );
    }

    #[test]
    fn test_workflow_status_distinct_parsing() {
        // `created` is the only true legacy alias — every other previously
        // collapsed string now parses to its own distinct variant.
        assert_eq!(
            "created".parse::<WorkflowStatus>(),
            Ok(WorkflowStatus::Pending)
        );
        assert_eq!(
            "compensating".parse::<WorkflowStatus>(),
            Ok(WorkflowStatus::Compensating)
        );
        assert_eq!(
            "compensated".parse::<WorkflowStatus>(),
            Ok(WorkflowStatus::Compensated)
        );
        assert_eq!(
            "retired_unresumable".parse::<WorkflowStatus>(),
            Ok(WorkflowStatus::Retired)
        );
        assert_eq!(
            "cancelled_by_operator".parse::<WorkflowStatus>(),
            Ok(WorkflowStatus::CancelledByOperator)
        );
        assert_eq!(
            "blocked_missing_version".parse::<WorkflowStatus>(),
            Ok(WorkflowStatus::BlockedMissingVersion)
        );
        assert_eq!(
            "blocked_signature_mismatch".parse::<WorkflowStatus>(),
            Ok(WorkflowStatus::BlockedSignatureMismatch)
        );
        assert_eq!(
            "blocked_missing_handler".parse::<WorkflowStatus>(),
            Ok(WorkflowStatus::BlockedMissingHandler)
        );
    }

    #[test]
    fn test_workflow_status_is_terminal() {
        assert!(!WorkflowStatus::Running.is_terminal());
        assert!(!WorkflowStatus::Waiting.is_terminal());
        assert!(!WorkflowStatus::Sleeping.is_terminal());
        assert!(!WorkflowStatus::Pending.is_terminal());
        // Compensating is non-terminal — it's actively unwinding.
        assert!(!WorkflowStatus::Compensating.is_terminal());
        assert!(!WorkflowStatus::BlockedMissingVersion.is_terminal());
        assert!(!WorkflowStatus::BlockedSignatureMismatch.is_terminal());
        assert!(!WorkflowStatus::BlockedMissingHandler.is_terminal());
        assert!(WorkflowStatus::Completed.is_terminal());
        assert!(WorkflowStatus::Failed.is_terminal());
        // Each new terminal variant resolves to its own end-state.
        assert!(WorkflowStatus::Compensated.is_terminal());
        assert!(WorkflowStatus::Retired.is_terminal());
        assert!(WorkflowStatus::CancelledByOperator.is_terminal());
    }

    #[test]
    fn workflow_status_terminal_variants_round_trip_as_str() {
        for variant in [
            WorkflowStatus::Compensating,
            WorkflowStatus::Compensated,
            WorkflowStatus::Retired,
            WorkflowStatus::CancelledByOperator,
        ] {
            let s = variant.as_str();
            let parsed: WorkflowStatus = s.parse().expect("round trip");
            assert_eq!(parsed, variant, "{s} did not round-trip");
        }
    }

    #[test]
    fn workflow_def_status_default_is_active() {
        // Default must be Active so a freshly-constructed WorkflowInfo accepts
        // new runs without an explicit status set.
        assert_eq!(WorkflowDefStatus::default(), WorkflowDefStatus::Active);
    }

    #[test]
    fn workflow_def_status_as_str_round_trips_all_variants() {
        assert_eq!(WorkflowDefStatus::Active.as_str(), "active");
        assert_eq!(WorkflowDefStatus::Deprecated.as_str(), "deprecated");
        assert_eq!(WorkflowDefStatus::Staging.as_str(), "staging");
    }

    #[test]
    fn workflow_def_status_active_predicate_only_matches_active() {
        assert!(WorkflowDefStatus::Active.is_active());
        assert!(!WorkflowDefStatus::Deprecated.is_active());
        assert!(!WorkflowDefStatus::Staging.is_active());
    }

    #[test]
    fn workflow_def_status_deprecated_predicate_only_matches_deprecated() {
        assert!(!WorkflowDefStatus::Active.is_deprecated());
        assert!(WorkflowDefStatus::Deprecated.is_deprecated());
        assert!(!WorkflowDefStatus::Staging.is_deprecated());
    }

    #[test]
    fn workflow_info_active_and_deprecated_track_status() {
        let deprecated = WorkflowInfo {
            status: WorkflowDefStatus::Deprecated,
            ..WorkflowInfo::default()
        };
        assert!(!deprecated.is_active());
        assert!(deprecated.is_deprecated());

        let staging = WorkflowInfo {
            status: WorkflowDefStatus::Staging,
            ..WorkflowInfo::default()
        };
        assert!(!staging.is_active());
        assert!(!staging.is_deprecated());

        let active = WorkflowInfo {
            status: WorkflowDefStatus::Active,
            ..WorkflowInfo::default()
        };
        assert!(active.is_active());
        assert!(!active.is_deprecated());
    }

    #[test]
    fn workflow_info_default_timeout_is_one_day() {
        let info = WorkflowInfo::default();
        assert_eq!(info.timeout, Duration::from_secs(86_400));
        assert!(info.http_timeout.is_none());
        assert!(!info.is_public);
        assert!(info.required_role.is_none());
        assert!(info.signature.is_empty());
    }

    #[test]
    fn workflow_status_parse_rejects_unknown() {
        let err = "garbage".parse::<WorkflowStatus>().unwrap_err();
        assert_eq!(err.0, "garbage");
        // Display must echo the bad value so logs pinpoint the typo.
        let msg = err.to_string();
        assert!(msg.contains("garbage"), "display dropped value: {msg}");
        assert!(msg.contains("invalid workflow status"));
    }

    #[test]
    fn parse_workflow_status_error_eq_uses_inner_string() {
        // PartialEq is derived, so equality is by inner String only.
        assert_eq!(
            ParseWorkflowStatusError("x".to_string()),
            ParseWorkflowStatusError("x".to_string())
        );
        assert_ne!(
            ParseWorkflowStatusError("x".to_string()),
            ParseWorkflowStatusError("y".to_string())
        );
    }

    #[test]
    fn workflow_status_blocked_variants_parse_distinctly() {
        assert_eq!(
            "blocked_missing_version".parse::<WorkflowStatus>(),
            Ok(WorkflowStatus::BlockedMissingVersion)
        );
        assert_eq!(
            "blocked_signature_mismatch".parse::<WorkflowStatus>(),
            Ok(WorkflowStatus::BlockedSignatureMismatch)
        );
        assert_eq!(
            "blocked_missing_handler".parse::<WorkflowStatus>(),
            Ok(WorkflowStatus::BlockedMissingHandler)
        );
        for blocked in [
            WorkflowStatus::BlockedMissingVersion,
            WorkflowStatus::BlockedSignatureMismatch,
            WorkflowStatus::BlockedMissingHandler,
        ] {
            assert!(blocked.is_blocked());
            assert!(!blocked.is_terminal());
        }
    }
}

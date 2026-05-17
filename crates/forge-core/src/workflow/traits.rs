use std::future::Future;
use std::pin::Pin;
use std::str::FromStr;
use std::time::Duration;

use serde::{Serialize, de::DeserializeOwned};

use super::context::WorkflowContext;
use crate::Result;
use crate::metadata::HandlerMetadata;

/// Trait for workflow handlers.
pub trait ForgeWorkflow: crate::__sealed::Sealed + Send + Sync + 'static {
    /// Input type for the workflow.
    type Input: DeserializeOwned + Serialize + Send + Sync;
    /// Output type for the workflow.
    type Output: Serialize + Send;

    /// Get workflow metadata.
    fn info() -> WorkflowInfo;

    /// Unified metadata for uniform consumers (observability, admin, codegen).
    fn metadata() -> HandlerMetadata {
        HandlerMetadata::from(&Self::info())
    }

    /// Execute the workflow.
    fn execute(
        ctx: &WorkflowContext,
        input: Self::Input,
    ) -> Pin<Box<dyn Future<Output = Result<Self::Output>> + Send + '_>>;
}

/// Lifecycle state of a workflow definition version.
///
/// A workflow name can have at most one `Active` version at a time.
/// `Deprecated` versions are kept alive only to drain in-flight runs.
/// `Staging` versions accept no new runs and are skipped during drain.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
#[non_exhaustive]
pub enum WorkflowDefStatus {
    /// This version accepts new runs (at most one per workflow name).
    #[default]
    Active,
    /// Old version kept alive to drain in-flight runs; accepts no new runs.
    Deprecated,
    /// Pre-release version: not yet promoted, not visible to new runs.
    Staging,
}

impl WorkflowDefStatus {
    /// Convert to the string written to `forge_workflow_definitions.status`.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Deprecated => "deprecated",
            Self::Staging => "staging",
        }
    }

    /// True if this version should accept new workflow runs.
    pub fn is_active(self) -> bool {
        matches!(self, Self::Active)
    }

    /// True if this version is deprecated.
    pub fn is_deprecated(self) -> bool {
        matches!(self, Self::Deprecated)
    }
}

/// Workflow metadata.
///
/// Constructed by the `#[workflow]` macro. Adding a field is a breaking change
/// for hand-written `ForgeWorkflow` impls; stage extensions through a builder
/// or major bump.
#[derive(Debug, Clone)]
pub struct WorkflowInfo {
    /// Workflow logical name (stable across versions).
    pub name: &'static str,
    /// User-facing version identifier (e.g. "2026-03", "v2", "signup-fix-1").
    pub version: &'static str,
    /// Derived signature from the persisted contract. Used as the hard runtime safety gate.
    pub signature: &'static str,
    /// Lifecycle status of this version.
    pub status: WorkflowDefStatus,
    /// Default timeout for the entire workflow.
    pub timeout: Duration,
    /// Default timeout for outbound HTTP requests made by the workflow.
    pub http_timeout: Option<Duration>,
    /// Whether the workflow is public (no auth required).
    pub is_public: bool,
    /// Required role for authorization (implies auth required).
    pub required_role: Option<&'static str>,
}

impl WorkflowInfo {
    /// Convenience accessor so existing call sites compile without changes.
    pub fn is_active(&self) -> bool {
        self.status.is_active()
    }

    /// Convenience accessor so existing call sites compile without changes.
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

/// Workflow execution status.
///
/// Workflow lifecycle states. Blocked variants are non-terminal: a deploy
/// with the matching handler/version/signature unblocks the run automatically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkflowStatus {
    /// Workflow created but not yet started.
    Pending,
    /// Workflow is actively running.
    Running,
    /// Workflow is suspended on a durable sleep timer.
    Sleeping,
    /// Workflow is waiting for an external event.
    Waiting,
    /// Workflow completed successfully.
    Completed,
    /// Workflow failed (includes cancelled and compensated runs).
    Failed,
    /// No registered handler for the workflow's pinned version.
    BlockedMissingVersion,
    /// Handler exists but signature doesn't match the run's pinned signature.
    BlockedSignatureMismatch,
    /// Workflow name not found in the registry at all.
    BlockedMissingHandler,
}

impl WorkflowStatus {
    /// Convert to string for database storage.
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::Running => "running",
            Self::Sleeping => "sleeping",
            Self::Waiting => "waiting",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::BlockedMissingVersion => "blocked_missing_version",
            Self::BlockedSignatureMismatch => "blocked_signature_mismatch",
            Self::BlockedMissingHandler => "blocked_missing_handler",
        }
    }

    /// Check if the workflow is terminal (no longer running).
    pub fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed)
    }

    /// Check if the workflow is blocked and waiting for a matching deploy.
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
            "failed"
            | "compensating"
            | "compensated"
            | "retired_unresumable"
            | "cancelled_by_operator" => Ok(Self::Failed),
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
    fn test_workflow_status_legacy_parsing() {
        assert_eq!(
            "created".parse::<WorkflowStatus>(),
            Ok(WorkflowStatus::Pending)
        );
        assert_eq!(
            "compensating".parse::<WorkflowStatus>(),
            Ok(WorkflowStatus::Failed)
        );
        assert_eq!(
            "compensated".parse::<WorkflowStatus>(),
            Ok(WorkflowStatus::Failed)
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
        assert_eq!(
            "cancelled_by_operator".parse::<WorkflowStatus>(),
            Ok(WorkflowStatus::Failed)
        );
        assert_eq!(
            "retired_unresumable".parse::<WorkflowStatus>(),
            Ok(WorkflowStatus::Failed)
        );
    }

    #[test]
    fn test_workflow_status_is_terminal() {
        assert!(!WorkflowStatus::Running.is_terminal());
        assert!(!WorkflowStatus::Waiting.is_terminal());
        assert!(!WorkflowStatus::Sleeping.is_terminal());
        assert!(!WorkflowStatus::Pending.is_terminal());
        assert!(!WorkflowStatus::BlockedMissingVersion.is_terminal());
        assert!(!WorkflowStatus::BlockedSignatureMismatch.is_terminal());
        assert!(!WorkflowStatus::BlockedMissingHandler.is_terminal());
        assert!(WorkflowStatus::Completed.is_terminal());
        assert!(WorkflowStatus::Failed.is_terminal());
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
    fn workflow_status_legacy_aliases_collapse_to_failed() {
        for legacy in [
            "compensating",
            "compensated",
            "retired_unresumable",
            "cancelled_by_operator",
        ] {
            let parsed: WorkflowStatus = legacy.parse().unwrap();
            assert_eq!(
                parsed,
                WorkflowStatus::Failed,
                "{legacy} did not map to Failed"
            );
        }
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

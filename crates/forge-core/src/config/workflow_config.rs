//! Workflow scheduler configuration.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::types::DurationStr;

/// Controls how strictly workflow signatures are validated on resume.
///
/// The workflow signature encodes the persisted contract (step keys, wait keys,
/// timeout, and input/output type names). By default (`Strict`) a mismatch
/// between the signature stored on the run and the signature compiled into the
/// current binary blocks the run with `BlockedSignatureMismatch`.
///
/// During a rolling deploy, all nodes run the same source version but the new
/// binary may carry a different signature if any tracked field changed — even
/// when the change is safe to resume through. Setting `signature_check =
/// "relaxed"` in `[workflow]` tells the runtime to accept any registered
/// handler for the matching `(name, version)` pair and skip the hash
/// comparison, preventing stranded runs during the transition window.
///
/// Switch back to `"strict"` once all nodes are on the new binary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SignatureCheckMode {
    /// Require an exact signature match before resuming a run (default).
    #[default]
    Strict,
    /// Accept any handler registered for the matching `(name, version)` pair,
    /// ignoring the signature hash. Use during rolling deploys.
    Relaxed,
}

/// Workflow scheduler configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WorkflowConfig {
    /// Poll interval for the workflow scheduler (e.g. "1s", "5s"). Wakeups are
    /// NOTIFY-driven; this is the fallback cadence when no notification arrives.
    #[serde(default = "default_poll_interval")]
    pub poll_interval: DurationStr,

    /// Timeout for workflow step execution (e.g. "30m", "1h").
    #[serde(default = "default_step_timeout")]
    pub step_timeout: DurationStr,

    /// Controls signature validation on workflow resume.
    ///
    /// - `"strict"` (default): resume requires the stored signature to match the
    ///   binary's compiled signature. Mismatches block with
    ///   `BlockedSignatureMismatch`.
    /// - `"relaxed"`: only `(name, version)` must match; the signature hash is
    ///   ignored. Use this during rolling deploys and revert to `"strict"` once
    ///   all nodes are on the new binary.
    #[serde(default)]
    pub signature_check: SignatureCheckMode,
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        Self {
            poll_interval: default_poll_interval(),
            step_timeout: default_step_timeout(),
            signature_check: SignatureCheckMode::Strict,
        }
    }
}

fn default_poll_interval() -> DurationStr {
    DurationStr::new(Duration::from_secs(1))
}

fn default_step_timeout() -> DurationStr {
    DurationStr::new(Duration::from_secs(1800))
}

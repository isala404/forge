//! Workflow scheduler configuration.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::types::DurationStr;

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
}

impl Default for WorkflowConfig {
    fn default() -> Self {
        Self {
            poll_interval: default_poll_interval(),
            step_timeout: default_step_timeout(),
        }
    }
}

fn default_poll_interval() -> DurationStr {
    DurationStr::new(Duration::from_secs(1))
}

fn default_step_timeout() -> DurationStr {
    DurationStr::new(Duration::from_secs(1800))
}

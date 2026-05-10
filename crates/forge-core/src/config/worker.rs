//! Worker (job queue) configuration.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::types::DurationStr;

/// Worker configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WorkerConfig {
    /// Maximum concurrent jobs.
    #[serde(default = "default_max_concurrent_jobs")]
    pub max_concurrent_jobs: usize,

    /// Job timeout duration (e.g. "1h", "30m").
    #[serde(default = "default_job_timeout")]
    pub job_timeout: DurationStr,

    /// Poll interval duration (e.g. "100ms", "1s").
    #[serde(default = "default_poll_interval")]
    pub poll_interval: DurationStr,
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            max_concurrent_jobs: default_max_concurrent_jobs(),
            job_timeout: default_job_timeout(),
            poll_interval: default_poll_interval(),
        }
    }
}

fn default_max_concurrent_jobs() -> usize {
    50
}

fn default_job_timeout() -> DurationStr {
    DurationStr::new(Duration::from_secs(3600))
}

fn default_poll_interval() -> DurationStr {
    DurationStr::new(Duration::from_millis(100))
}

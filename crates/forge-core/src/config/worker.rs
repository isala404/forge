//! Worker (job queue) configuration.

use serde::{Deserialize, Serialize};

/// Worker configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WorkerConfig {
    /// Maximum concurrent jobs.
    #[serde(default = "default_max_concurrent_jobs")]
    pub max_concurrent_jobs: usize,

    /// Job timeout duration (e.g. "1h", "30m").
    #[serde(default = "default_job_timeout")]
    pub job_timeout: String,

    /// Poll interval duration (e.g. "100ms", "1s").
    #[serde(default = "default_poll_interval")]
    pub poll_interval: String,
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

impl WorkerConfig {
    /// Job timeout in seconds, parsed from the `job_timeout` string.
    pub fn job_timeout_secs(&self) -> u64 {
        super::parse_duration_secs(&self.job_timeout, 3600)
    }

    /// Poll interval in milliseconds, parsed from the `poll_interval` string.
    pub fn poll_interval_ms(&self) -> u64 {
        super::parse_duration_millis(&self.poll_interval, 100)
    }
}

fn default_max_concurrent_jobs() -> usize {
    50
}

fn default_job_timeout() -> String {
    "1h".to_string()
}

fn default_poll_interval() -> String {
    "100ms".to_string()
}

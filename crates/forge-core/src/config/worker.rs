//! Worker (job queue) configuration.

use std::collections::BTreeMap;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::types::DurationStr;

/// Worker configuration.
///
/// Worker pools are reserved per queue so heavy traffic on one queue cannot
/// starve another. Defaults: `default=8`, `workflows=4`, `cron=2`. Operators
/// override via `[worker.queues.<name>]` in `forge.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct WorkerConfig {
    /// Job timeout duration (e.g. "1h", "30m").
    #[serde(default = "default_job_timeout")]
    pub job_timeout: DurationStr,

    /// Poll interval duration (e.g. "100ms", "1s"). Wakeups are NOTIFY-driven;
    /// this is the fallback cadence when no `forge_jobs_available` arrives.
    #[serde(default = "default_poll_interval")]
    pub poll_interval: DurationStr,

    /// Per-queue worker pool reservations. The `default` queue handles
    /// untagged user jobs; `workflows` handles `$workflow_resume`; `cron`
    /// handles `$cron:*`. Add custom queues by tagging jobs with a worker
    /// capability and configuring a matching entry here.
    #[serde(default = "default_queues")]
    pub queues: BTreeMap<String, QueueWorkerConfig>,
}

/// Per-queue worker pool reservation.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[non_exhaustive]
pub struct QueueWorkerConfig {
    /// Number of concurrent worker tasks for this queue.
    pub workers: usize,
}

impl QueueWorkerConfig {
    /// Construct a queue config with the given worker count.
    pub const fn new(workers: usize) -> Self {
        Self { workers }
    }
}

impl Default for WorkerConfig {
    fn default() -> Self {
        Self {
            job_timeout: default_job_timeout(),
            poll_interval: default_poll_interval(),
            queues: default_queues(),
        }
    }
}

/// Queue name reserved for the `$workflow_resume` job kind.
pub const WORKFLOWS_QUEUE: &str = "workflows";

/// Queue name reserved for `$cron:<name>` jobs.
pub const CRON_QUEUE: &str = "cron";

/// Queue name that drains untagged user jobs.
pub const DEFAULT_QUEUE: &str = "default";

fn default_queues() -> BTreeMap<String, QueueWorkerConfig> {
    let mut q = BTreeMap::new();
    q.insert(DEFAULT_QUEUE.to_string(), QueueWorkerConfig::new(8));
    q.insert(WORKFLOWS_QUEUE.to_string(), QueueWorkerConfig::new(4));
    q.insert(CRON_QUEUE.to_string(), QueueWorkerConfig::new(2));
    q
}

fn default_job_timeout() -> DurationStr {
    DurationStr::new(Duration::from_secs(3600))
}

fn default_poll_interval() -> DurationStr {
    DurationStr::new(Duration::from_millis(100))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_queues_match_plan() {
        let cfg = WorkerConfig::default();
        assert_eq!(
            cfg.queues.get(DEFAULT_QUEUE),
            Some(&QueueWorkerConfig::new(8))
        );
        assert_eq!(
            cfg.queues.get(WORKFLOWS_QUEUE),
            Some(&QueueWorkerConfig::new(4))
        );
        assert_eq!(cfg.queues.get(CRON_QUEUE), Some(&QueueWorkerConfig::new(2)));
    }

    #[test]
    fn deserialize_queue_overrides() {
        let toml_str = r#"
            [queues.default]
            workers = 16

            [queues.media]
            workers = 3
        "#;
        let cfg: WorkerConfig = toml::from_str(toml_str).expect("parse");
        assert_eq!(cfg.queues.get("default"), Some(&QueueWorkerConfig::new(16)));
        assert_eq!(cfg.queues.get("media"), Some(&QueueWorkerConfig::new(3)));
    }
}

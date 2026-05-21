//! Cron scheduler configuration.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::types::DurationStr;

/// Cron scheduler configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct CronConfig {
    /// Poll interval for the cron scheduler tick (e.g. "1s", "5s").
    #[serde(default = "default_poll_interval")]
    pub poll_interval: DurationStr,

    /// Maximum number of catch-up jobs to insert per scheduler tick after
    /// downtime. Prevents burst-inserting thousands of missed jobs.
    #[serde(default = "default_catch_up_limit")]
    pub catch_up_limit: usize,
}

impl Default for CronConfig {
    fn default() -> Self {
        Self {
            poll_interval: default_poll_interval(),
            catch_up_limit: default_catch_up_limit(),
        }
    }
}

fn default_poll_interval() -> DurationStr {
    DurationStr::new(Duration::from_secs(1))
}

fn default_catch_up_limit() -> usize {
    100
}

//! Daemon runner configuration.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::types::DurationStr;

/// Daemon runner configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct DaemonConfig {
    /// How often the daemon runner checks health of running daemons.
    #[serde(default = "default_health_check_interval")]
    pub health_check_interval: DurationStr,

    /// How often leader-elected daemons send heartbeats.
    #[serde(default = "default_heartbeat_interval")]
    pub heartbeat_interval: DurationStr,
}

impl Default for DaemonConfig {
    fn default() -> Self {
        Self {
            health_check_interval: default_health_check_interval(),
            heartbeat_interval: default_heartbeat_interval(),
        }
    }
}

fn default_health_check_interval() -> DurationStr {
    DurationStr::new(Duration::from_secs(30))
}

fn default_heartbeat_interval() -> DurationStr {
    DurationStr::new(Duration::from_secs(10))
}

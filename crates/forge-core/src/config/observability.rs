//! Observability configuration for OTLP telemetry export.

use std::time::Duration;

use serde::{Deserialize, Serialize};

use super::default_true;
use super::types::DurationStr;

/// Observability configuration for OTLP telemetry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ObservabilityConfig {
    #[serde(default)]
    pub enabled: bool,

    #[serde(default = "default_otlp_endpoint")]
    pub otlp_endpoint: String,

    pub service_name: Option<String>,

    #[serde(default = "default_true")]
    pub enable_traces: bool,

    #[serde(default = "default_true")]
    pub enable_metrics: bool,

    #[serde(default = "default_true")]
    pub enable_logs: bool,

    /// Trace sampling ratio (0.0 to 1.0).
    #[serde(default = "default_sampling_ratio")]
    pub sampling_ratio: f64,

    /// Metrics export interval duration (e.g. "15s", "1m"). OTLP collectors typically prefer 15s-60s.
    #[serde(default = "default_metrics_interval")]
    pub metrics_interval: DurationStr,

    #[serde(default = "default_log_level")]
    pub log_level: String,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            otlp_endpoint: default_otlp_endpoint(),
            service_name: None,
            enable_traces: true,
            enable_metrics: true,
            enable_logs: true,
            sampling_ratio: default_sampling_ratio(),
            metrics_interval: default_metrics_interval(),
            log_level: default_log_level(),
        }
    }
}

impl ObservabilityConfig {
    /// Whether OTLP export is active (enabled + at least one signal on).
    pub fn otlp_active(&self) -> bool {
        self.enabled && (self.enable_traces || self.enable_metrics || self.enable_logs)
    }
}

fn default_otlp_endpoint() -> String {
    "http://localhost:4318".to_string()
}

/// Default trace sampling ratio. 100% so every span is visible out of the box.
/// Users can tune down for high-traffic production deployments.
fn default_sampling_ratio() -> f64 {
    1.0
}

fn default_metrics_interval() -> DurationStr {
    DurationStr::new(Duration::from_secs(15))
}

fn default_log_level() -> String {
    "info".to_string()
}

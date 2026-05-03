//! Observability configuration for OTLP telemetry export.

use serde::{Deserialize, Serialize};

/// Observability configuration for OTLP telemetry.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct ObservabilityConfig {
    /// Enable observability (traces, metrics, logs).
    #[serde(default)]
    pub enabled: bool,

    /// OTLP endpoint for telemetry export.
    #[serde(default = "default_otlp_endpoint")]
    pub otlp_endpoint: String,

    /// Service name for telemetry identification.
    pub service_name: Option<String>,

    /// Enable distributed tracing.
    #[serde(default = "default_true")]
    pub enable_traces: bool,

    /// Enable metrics collection.
    #[serde(default = "default_true")]
    pub enable_metrics: bool,

    /// Enable log export via OTLP.
    #[serde(default = "default_true")]
    pub enable_logs: bool,

    /// Trace sampling ratio (0.0 to 1.0).
    #[serde(default = "default_sampling_ratio")]
    pub sampling_ratio: f64,

    /// Metrics export interval duration (e.g. "15s", "1m"). OTLP collectors typically prefer 15s-60s.
    #[serde(default = "default_metrics_interval")]
    pub metrics_interval: String,

    /// Log level for the tracing subscriber (e.g., "debug", "info", "warn").
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

    /// Metrics export interval in seconds, parsed from the `metrics_interval` string.
    pub fn metrics_interval_secs(&self) -> u64 {
        super::parse_duration_secs(&self.metrics_interval, 15)
    }
}

fn default_otlp_endpoint() -> String {
    "http://localhost:4318".to_string()
}

fn default_true() -> bool {
    true
}

/// Default trace sampling ratio. 100% so every span is visible out of the box.
/// Users can tune down for high-traffic production deployments.
fn default_sampling_ratio() -> f64 {
    1.0
}

fn default_metrics_interval() -> String {
    "15s".to_string()
}

fn default_log_level() -> String {
    "info".to_string()
}

use serde::{Deserialize, Serialize};

/// Observability configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservabilityConfig {
    /// Enable metrics collection.
    #[serde(default = "default_true")]
    pub metrics_enabled: bool,

    /// Enable logging.
    #[serde(default = "default_true")]
    pub logging_enabled: bool,

    /// Enable distributed tracing.
    #[serde(default = "default_true")]
    pub tracing_enabled: bool,

    /// Enable built-in dashboard.
    #[serde(default = "default_true")]
    pub dashboard_enabled: bool,

    /// Logging configuration.
    #[serde(default)]
    pub logging: LoggingConfig,

    /// Metrics configuration.
    #[serde(default)]
    pub metrics: MetricsConfig,

    /// Tracing configuration.
    #[serde(default)]
    pub tracing: TracingConfig,

    /// Data retention configuration.
    #[serde(default)]
    pub retention: RetentionConfig,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            metrics_enabled: true,
            logging_enabled: true,
            tracing_enabled: true,
            dashboard_enabled: true,
            logging: LoggingConfig::default(),
            metrics: MetricsConfig::default(),
            tracing: TracingConfig::default(),
            retention: RetentionConfig::default(),
        }
    }
}

fn default_true() -> bool {
    true
}

/// Logging configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoggingConfig {
    /// Log level.
    #[serde(default = "default_log_level")]
    pub level: String,

    /// Slow query threshold in milliseconds.
    #[serde(default = "default_slow_query_threshold")]
    pub slow_query_threshold_ms: u64,

    /// Whether to output JSON format.
    #[serde(default)]
    pub json_format: bool,
}

impl Default for LoggingConfig {
    fn default() -> Self {
        Self {
            level: default_log_level(),
            slow_query_threshold_ms: default_slow_query_threshold(),
            json_format: false,
        }
    }
}

fn default_log_level() -> String {
    "info".to_string()
}

fn default_slow_query_threshold() -> u64 {
    100
}

/// Metrics configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsConfig {
    /// Flush interval in seconds.
    #[serde(default = "default_flush_interval")]
    pub flush_interval_secs: u64,

    /// Enable Prometheus endpoint.
    #[serde(default)]
    pub prometheus_enabled: bool,

    /// Prometheus endpoint path.
    #[serde(default = "default_prometheus_path")]
    pub prometheus_path: String,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            flush_interval_secs: default_flush_interval(),
            prometheus_enabled: false,
            prometheus_path: default_prometheus_path(),
        }
    }
}

fn default_flush_interval() -> u64 {
    10
}

fn default_prometheus_path() -> String {
    "/metrics".to_string()
}

/// Tracing configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TracingConfig {
    /// Sample rate (0.0 to 1.0).
    #[serde(default = "default_sample_rate")]
    pub sample_rate: f64,

    /// OTLP endpoint for exporting traces.
    pub otlp_endpoint: Option<String>,
}

impl Default for TracingConfig {
    fn default() -> Self {
        Self {
            sample_rate: default_sample_rate(),
            otlp_endpoint: None,
        }
    }
}

fn default_sample_rate() -> f64 {
    1.0
}

/// Data retention configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetentionConfig {
    /// Metrics retention in days.
    #[serde(default = "default_metrics_retention")]
    pub metrics_days: u32,

    /// Logs retention in days.
    #[serde(default = "default_logs_retention")]
    pub logs_days: u32,

    /// Traces retention in days.
    #[serde(default = "default_traces_retention")]
    pub traces_days: u32,

    /// Completed jobs retention in days.
    #[serde(default = "default_jobs_retention")]
    pub completed_jobs_days: u32,
}

impl Default for RetentionConfig {
    fn default() -> Self {
        Self {
            metrics_days: default_metrics_retention(),
            logs_days: default_logs_retention(),
            traces_days: default_traces_retention(),
            completed_jobs_days: default_jobs_retention(),
        }
    }
}

fn default_metrics_retention() -> u32 {
    30
}

fn default_logs_retention() -> u32 {
    7
}

fn default_traces_retention() -> u32 {
    7
}

fn default_jobs_retention() -> u32 {
    7
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_observability_config() {
        let config = ObservabilityConfig::default();
        assert!(config.metrics_enabled);
        assert!(config.logging_enabled);
        assert!(config.tracing_enabled);
        assert_eq!(config.logging.level, "info");
    }

    #[test]
    fn test_parse_observability_config() {
        let toml = r#"
            metrics_enabled = true
            logging_enabled = true
            dashboard_enabled = false

            [logging]
            level = "debug"
            slow_query_threshold_ms = 50

            [metrics]
            flush_interval_secs = 5
            prometheus_enabled = true
        "#;

        let config: ObservabilityConfig = toml::from_str(toml).unwrap();
        assert!(!config.dashboard_enabled);
        assert_eq!(config.logging.level, "debug");
        assert!(config.metrics.prometheus_enabled);
    }
}

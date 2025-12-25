use std::time::Duration;

use serde::{Deserialize, Serialize};

use forge_core::LogLevel;

/// Observability configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ObservabilityConfig {
    /// Whether observability is enabled.
    pub enabled: bool,
    /// Separate database URL for observability (optional).
    pub database_url: Option<String>,
    /// Connection pool size.
    pub pool_size: u32,
    /// Connection pool timeout.
    pub pool_timeout: Duration,
    /// Metrics configuration.
    pub metrics: MetricsConfig,
    /// Logs configuration.
    pub logs: LogsConfig,
    /// Traces configuration.
    pub traces: TracesConfig,
    /// Export configuration.
    pub export: ExportConfig,
}

impl Default for ObservabilityConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            database_url: None,
            pool_size: 10,
            pool_timeout: Duration::from_secs(5),
            metrics: MetricsConfig::default(),
            logs: LogsConfig::default(),
            traces: TracesConfig::default(),
            export: ExportConfig::default(),
        }
    }
}

/// Metrics configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsConfig {
    /// Collection interval.
    pub interval: Duration,
    /// Raw data retention.
    pub raw_retention: Duration,
    /// 1-minute aggregate retention.
    pub downsampled_1m: Duration,
    /// 5-minute aggregate retention.
    pub downsampled_5m: Duration,
    /// 1-hour aggregate retention.
    pub downsampled_1h: Duration,
    /// In-memory buffer size before flush.
    pub buffer_size: usize,
    /// Flush interval.
    pub flush_interval: Duration,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            interval: Duration::from_secs(10),
            raw_retention: Duration::from_secs(3600), // 1 hour
            downsampled_1m: Duration::from_secs(86400), // 24 hours
            downsampled_5m: Duration::from_secs(604800), // 7 days
            downsampled_1h: Duration::from_secs(7776000), // 90 days
            buffer_size: 10000,
            flush_interval: Duration::from_secs(10),
        }
    }
}

/// Logs configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogsConfig {
    /// Minimum log level.
    pub level: LogLevel,
    /// Retention duration.
    pub retention: Duration,
    /// Slow query threshold.
    pub slow_query_threshold: Duration,
    /// Async writes.
    pub async_writes: bool,
    /// Buffer size.
    pub buffer_size: usize,
}

impl Default for LogsConfig {
    fn default() -> Self {
        Self {
            level: LogLevel::Info,
            retention: Duration::from_secs(604800), // 7 days
            slow_query_threshold: Duration::from_millis(100),
            async_writes: true,
            buffer_size: 5000,
        }
    }
}

/// Traces configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TracesConfig {
    /// Sample rate (0.0 to 1.0).
    pub sample_rate: f64,
    /// Retention duration.
    pub retention: Duration,
    /// Always trace errors.
    pub always_trace_errors: bool,
}

impl Default for TracesConfig {
    fn default() -> Self {
        Self {
            sample_rate: 1.0,
            retention: Duration::from_secs(86400), // 24 hours
            always_trace_errors: true,
        }
    }
}

/// Export configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ExportConfig {
    /// Export destinations.
    pub destinations: Vec<ExportDestination>,
    /// OTLP configuration.
    pub otlp: Option<OtlpConfig>,
    /// Prometheus configuration.
    pub prometheus: Option<PrometheusConfig>,
}

impl Default for ExportConfig {
    fn default() -> Self {
        Self {
            destinations: vec![ExportDestination::Postgres],
            otlp: None,
            prometheus: None,
        }
    }
}

/// Export destination.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ExportDestination {
    /// PostgreSQL storage.
    Postgres,
    /// OpenTelemetry Protocol.
    Otlp,
    /// Prometheus metrics endpoint.
    Prometheus,
}

/// OTLP export configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OtlpConfig {
    /// OTLP endpoint.
    pub endpoint: String,
    /// Protocol (grpc or http/protobuf).
    pub protocol: String,
    /// Authentication headers.
    pub headers: std::collections::HashMap<String, String>,
    /// Export metrics.
    pub metrics: bool,
    /// Export logs.
    pub logs: bool,
    /// Export traces.
    pub traces: bool,
    /// Trace sample rate for export.
    pub trace_sample_rate: f64,
    /// Always export errors.
    pub always_export_errors: bool,
}

impl Default for OtlpConfig {
    fn default() -> Self {
        Self {
            endpoint: "http://localhost:4317".to_string(),
            protocol: "grpc".to_string(),
            headers: std::collections::HashMap::new(),
            metrics: true,
            logs: true,
            traces: true,
            trace_sample_rate: 1.0,
            always_export_errors: true,
        }
    }
}

/// Prometheus export configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrometheusConfig {
    /// Whether enabled.
    pub enabled: bool,
    /// Metrics endpoint path.
    pub path: String,
}

impl Default for PrometheusConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            path: "/metrics".to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_observability_config_default() {
        let config = ObservabilityConfig::default();
        assert!(config.enabled);
        assert!(config.database_url.is_none());
        assert_eq!(config.pool_size, 10);
    }

    #[test]
    fn test_metrics_config_default() {
        let config = MetricsConfig::default();
        assert_eq!(config.interval, Duration::from_secs(10));
        assert_eq!(config.buffer_size, 10000);
    }

    #[test]
    fn test_logs_config_default() {
        let config = LogsConfig::default();
        assert_eq!(config.level, LogLevel::Info);
        assert!(config.async_writes);
    }

    #[test]
    fn test_traces_config_default() {
        let config = TracesConfig::default();
        assert_eq!(config.sample_rate, 1.0);
        assert!(config.always_trace_errors);
    }

    #[test]
    fn test_export_config_default() {
        let config = ExportConfig::default();
        assert_eq!(config.destinations, vec![ExportDestination::Postgres]);
    }
}

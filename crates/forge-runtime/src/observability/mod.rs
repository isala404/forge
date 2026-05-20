//! Observability: traces, metrics, and structured logging.
//!
//! With the `otel` feature: real OpenTelemetry pipeline (OTLP exporters for
//! traces/metrics/logs, instrumented DB queries, function/job/HTTP metrics).
//!
//! Without `otel`: every recording function is a no-op. Callers can record
//! events unconditionally without compile-time cfg gates everywhere.
//! `tracing-subscriber` log output still works via `init_telemetry`'s
//! fallback path.

#[cfg(feature = "otel")]
mod db;
#[cfg(feature = "otel")]
mod metrics;
#[cfg(feature = "otel")]
mod telemetry;

#[cfg(feature = "otel")]
pub use db::{extract_table_name, instrumented_query, record_pool_metrics, record_query_duration};
#[cfg(feature = "otel")]
pub use metrics::{
    ActiveConnectionsGauge, FnCacheMetrics, FnMetrics, HttpMetrics, JobMetrics,
    NotifyMetrics, SubscriptionMetrics, WorkflowSchedulerMetrics,
    record_fn_cache, record_fn_execution, record_http_request, record_job_execution,
    record_lost_claim, record_notify_payload_bytes, record_subscription_counts,
    record_workflow_scheduler_duration, set_active_connections,
};
#[cfg(feature = "otel")]
pub use telemetry::{
    TelemetryConfig, TelemetryError, build_env_filter, init_telemetry, shutdown_telemetry,
};

// --- No-op stubs when otel is disabled ---

#[cfg(not(feature = "otel"))]
mod stub {
    use forge_core::config::ObservabilityConfig;
    use sqlx::PgPool;
    use std::time::Duration;

    /// Minimal telemetry config that mirrors the real one's surface so
    /// callers don't need cfg gates.
    #[derive(Debug, Clone, Default)]
    pub struct TelemetryConfig {
        pub otlp_endpoint: String,
        pub enable_traces: bool,
        pub enable_metrics: bool,
        pub enable_logs: bool,
        pub sampling_ratio: f64,
    }

    impl TelemetryConfig {
        pub fn from_observability_config(
            _cfg: &ObservabilityConfig,
            _service_name: &str,
            _service_version: &str,
        ) -> Self {
            Self::default()
        }
    }

    #[derive(Debug, thiserror::Error)]
    pub enum TelemetryError {
        #[error("telemetry disabled: build with the `otel` feature to enable OpenTelemetry")]
        Disabled,
    }

    /// When `otel` is off, install a basic `tracing-subscriber` formatter so
    /// log output still reaches stderr. Returns `Ok(false)` to indicate that
    /// no exporters were installed.
    pub fn init_telemetry(
        _cfg: &TelemetryConfig,
        _service_name: &str,
        log_level: &str,
    ) -> Result<bool, TelemetryError> {
        use tracing_subscriber::EnvFilter;
        use tracing_subscriber::fmt;
        use tracing_subscriber::prelude::*;

        let filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(log_level));

        // Idempotent install: ignore "already set" errors so tests and
        // multi-init paths don't panic.
        let _ = tracing_subscriber::registry()
            .with(filter)
            .with(fmt::layer())
            .try_init();

        Ok(false)
    }

    pub fn shutdown_telemetry() {}

    pub fn build_env_filter(_log_level: &str) -> tracing_subscriber::EnvFilter {
        tracing_subscriber::EnvFilter::new("info")
    }

    // --- Recording stubs (compile to nothing) ---

    #[inline]
    pub fn record_pool_metrics(_pool: &PgPool) {}

    #[inline]
    pub fn record_query_duration(_operation: &str, _duration: Duration) {}

    #[inline]
    pub fn record_fn_execution(
        _function: &str,
        _kind: &str,
        _success: bool,
        _cached: bool,
        _duration_secs: f64,
    ) {
    }

    #[inline]
    pub fn record_fn_cache(_function: &str, _hit: bool) {}

    #[inline]
    pub fn record_http_request(_method: &str, _path: &str, _status: u16, _duration_secs: f64) {}

    #[inline]
    pub fn record_job_execution(_job_type: &str, _status: &'static str, _duration_secs: f64) {}

    #[inline]
    pub fn record_lost_claim(_job_type: &str) {}

    #[inline]
    pub fn set_active_connections(_connection_type: &'static str, _delta: i64) {}

    #[inline]
    pub fn record_notify_payload_bytes(_channel: &str, _bytes: usize) {}

    #[inline]
    pub fn record_subscription_counts(_subscribers: usize, _groups: usize, _tables: usize) {}

    #[inline]
    pub fn record_workflow_scheduler_duration(_duration_secs: f64) {}

    pub fn extract_table_name(_sql: &str) -> Option<&str> {
        None
    }

    #[inline]
    pub async fn instrumented_query<F, T, E>(
        _operation: &str,
        _table: Option<&str>,
        f: F,
    ) -> Result<T, E>
    where
        F: std::future::Future<Output = Result<T, E>>,
    {
        f.await
    }
}

#[cfg(not(feature = "otel"))]
pub use stub::{
    TelemetryConfig, TelemetryError, build_env_filter, extract_table_name, init_telemetry,
    instrumented_query, record_fn_cache, record_fn_execution, record_http_request,
    record_job_execution, record_lost_claim, record_notify_payload_bytes, record_pool_metrics,
    record_query_duration, record_subscription_counts, record_workflow_scheduler_duration,
    set_active_connections, shutdown_telemetry,
};

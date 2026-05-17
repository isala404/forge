use opentelemetry::{KeyValue, global, trace::TracerProvider as _};
use opentelemetry_appender_tracing::layer::OpenTelemetryTracingBridge;
use opentelemetry_otlp::{LogExporter, MetricExporter, SpanExporter, WithExportConfig};
use opentelemetry_sdk::{
    Resource,
    logs::LoggerProvider,
    metrics::{MeterProviderBuilder, PeriodicReader, SdkMeterProvider},
    propagation::TraceContextPropagator,
    resource::{EnvResourceDetector, SdkProvidedResourceDetector},
    trace::{RandomIdGenerator, Sampler, TracerProvider},
};
use opentelemetry_semantic_conventions::resource::{SERVICE_NAME, SERVICE_VERSION};

const DEPLOYMENT_ENVIRONMENT_NAME: &str = "deployment.environment.name";
use std::sync::OnceLock;
use thiserror::Error;
use tracing_opentelemetry::OpenTelemetryLayer;
use tracing_subscriber::{
    EnvFilter, Layer, Registry,
    filter::{FilterExt, FilterFn},
    layer::SubscriberExt,
    util::SubscriberInitExt,
};

static TRACER_PROVIDER: OnceLock<TracerProvider> = OnceLock::new();
static METER_PROVIDER: OnceLock<SdkMeterProvider> = OnceLock::new();
static LOGGER_PROVIDER: OnceLock<LoggerProvider> = OnceLock::new();

#[derive(Debug, Error)]
pub enum TelemetryError {
    #[error("failed to initialize tracer: {0}")]
    TracerInit(String),
    #[error("failed to initialize meter: {0}")]
    MeterInit(String),
    #[error("failed to initialize logger: {0}")]
    LoggerInit(String),
    #[error("telemetry already initialized")]
    AlreadyInitialized,
    #[error("tracing subscriber init failed: {0}")]
    SubscriberInit(String),
}

#[derive(Debug, Clone)]
pub struct TelemetryConfig {
    pub otlp_endpoint: String,
    pub service_name: String,
    pub service_version: String,
    pub environment: String,
    pub enable_traces: bool,
    pub enable_metrics: bool,
    pub enable_logs: bool,
    pub sampling_ratio: f64,
    pub metrics_interval_secs: u64,
}

impl Default for TelemetryConfig {
    fn default() -> Self {
        Self {
            otlp_endpoint: "http://localhost:4318".to_string(),
            service_name: "forge-service".to_string(),
            service_version: "0.1.0".to_string(),
            environment: "development".to_string(),
            enable_traces: true,
            enable_metrics: true,
            enable_logs: true,
            sampling_ratio: 1.0,
            metrics_interval_secs: 15,
        }
    }
}

impl TelemetryConfig {
    pub fn new(service_name: impl Into<String>) -> Self {
        Self {
            service_name: service_name.into(),
            ..Default::default()
        }
    }

    /// Create config from ForgeConfig's observability settings.
    pub fn from_observability_config(
        obs: &forge_core::config::ObservabilityConfig,
        project_name: &str,
        project_version: &str,
    ) -> Self {
        // When observability is disabled, turn off OTLP export but keep the
        // fmt subscriber so console logs still work.
        let otlp_enabled = obs.enabled;
        Self {
            otlp_endpoint: obs.otlp_endpoint.clone(),
            service_name: obs
                .service_name
                .clone()
                .unwrap_or_else(|| project_name.to_string()),
            service_version: project_version.to_string(),
            environment: "production".to_string(),
            enable_traces: otlp_enabled && obs.enable_traces,
            enable_metrics: otlp_enabled && obs.enable_metrics,
            enable_logs: otlp_enabled && obs.enable_logs,
            sampling_ratio: obs.sampling_ratio,
            metrics_interval_secs: obs.metrics_interval.as_secs(),
        }
    }

    pub fn with_endpoint(mut self, endpoint: impl Into<String>) -> Self {
        self.otlp_endpoint = endpoint.into();
        self
    }

    pub fn with_version(mut self, version: impl Into<String>) -> Self {
        self.service_version = version.into();
        self
    }

    pub fn with_environment(mut self, env: impl Into<String>) -> Self {
        self.environment = env.into();
        self
    }

    pub fn with_traces(mut self, enabled: bool) -> Self {
        self.enable_traces = enabled;
        self
    }

    pub fn with_metrics(mut self, enabled: bool) -> Self {
        self.enable_metrics = enabled;
        self
    }

    pub fn with_logs(mut self, enabled: bool) -> Self {
        self.enable_logs = enabled;
        self
    }

    pub fn with_sampling_ratio(mut self, ratio: f64) -> Self {
        self.sampling_ratio = ratio.clamp(0.0, 1.0);
        self
    }
}

fn build_resource(config: &TelemetryConfig) -> Resource {
    let base = Resource::from_detectors(
        std::time::Duration::from_secs(5),
        vec![
            Box::new(SdkProvidedResourceDetector),
            Box::new(EnvResourceDetector::new()),
        ],
    );

    let custom = Resource::new(vec![
        KeyValue::new(SERVICE_NAME, config.service_name.clone()),
        KeyValue::new(SERVICE_VERSION, config.service_version.clone()),
        KeyValue::new(DEPLOYMENT_ENVIRONMENT_NAME, config.environment.clone()),
    ]);

    base.merge(&custom)
}

fn init_tracer(config: &TelemetryConfig) -> Result<TracerProvider, TelemetryError> {
    let exporter = SpanExporter::builder()
        .with_http()
        .with_endpoint(&config.otlp_endpoint)
        .build()
        .map_err(|e| TelemetryError::TracerInit(e.to_string()))?;

    let sampler = if config.sampling_ratio >= 1.0 {
        Sampler::AlwaysOn
    } else if config.sampling_ratio <= 0.0 {
        Sampler::AlwaysOff
    } else {
        Sampler::TraceIdRatioBased(config.sampling_ratio)
    };

    let provider = TracerProvider::builder()
        .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
        .with_sampler(sampler)
        .with_id_generator(RandomIdGenerator::default())
        .with_resource(build_resource(config))
        .build();

    Ok(provider)
}

fn init_meter(config: &TelemetryConfig) -> Result<SdkMeterProvider, TelemetryError> {
    let exporter = MetricExporter::builder()
        .with_http()
        .with_endpoint(&config.otlp_endpoint)
        .build()
        .map_err(|e| TelemetryError::MeterInit(e.to_string()))?;

    let reader = PeriodicReader::builder(exporter, opentelemetry_sdk::runtime::Tokio)
        .with_interval(std::time::Duration::from_secs(config.metrics_interval_secs))
        .build();

    let provider = MeterProviderBuilder::default()
        .with_reader(reader)
        .with_resource(build_resource(config))
        .build();

    Ok(provider)
}

fn init_logger(config: &TelemetryConfig) -> Result<LoggerProvider, TelemetryError> {
    let exporter = LogExporter::builder()
        .with_http()
        .with_endpoint(&config.otlp_endpoint)
        .build()
        .map_err(|e| TelemetryError::LoggerInit(e.to_string()))?;

    let provider = LoggerProvider::builder()
        .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
        .with_resource(build_resource(config))
        .build();

    Ok(provider)
}

/// Build an `EnvFilter` for console output.
///
/// Ensures the user's crate is visible at the configured level even when
/// `RUST_LOG` is set to something restrictive (e.g. `warn`).
pub fn build_env_filter(project_name: &str, log_level: &str) -> EnvFilter {
    build_console_filter(project_name, log_level)
}

fn build_console_filter(project_name: &str, log_level: &str) -> EnvFilter {
    let crate_name = project_name.replace('-', "_");

    let base = if let Ok(filter) = EnvFilter::try_from_default_env() {
        filter
    } else {
        EnvFilter::new(log_level)
    };

    let directive = format!("{}={}", crate_name, log_level);
    match directive.parse() {
        Ok(d) => base.add_directive(d),
        Err(_) => base,
    }
}

/// Set up tracing so logs work without any user boilerplate.
/// Returns `Ok(false)` if a subscriber already exists (user configured their own).
pub fn init_telemetry(
    config: &TelemetryConfig,
    project_name: &str,
    log_level: &str,
) -> Result<bool, TelemetryError> {
    global::set_text_map_propagator(TraceContextPropagator::new());

    // Per-layer filters avoid the global EnvFilter + per-layer filter conflict
    // that causes the OTel log bridge to silently drop events.
    let fmt_layer = tracing_subscriber::fmt::layer()
        .with_target(true)
        .with_thread_ids(false)
        .with_file(false)
        .with_line_number(false)
        .with_filter(build_console_filter(project_name, log_level));

    // Build optional trace layer (includes sqlx debug for DB-level spans)
    let otel_trace_layer = if config.enable_traces {
        match init_tracer(config) {
            Ok(tracer_provider) => {
                let tracer = tracer_provider.tracer(config.service_name.clone());

                TRACER_PROVIDER
                    .set(tracer_provider.clone())
                    .map_err(|_| TelemetryError::AlreadyInitialized)?;

                global::set_tracer_provider(tracer_provider);

                Some(
                    OpenTelemetryLayer::new(tracer)
                        .with_filter(build_console_filter(project_name, log_level)),
                )
            }
            Err(e) => {
                eprintln!("WARNING: OTLP trace exporter init failed, traces disabled: {e}");
                None
            }
        }
    } else {
        None
    };

    // Build optional log bridge layer.
    // Uses the OTel filter (includes sqlx debug) plus an anti-recursion guard
    // that blocks events from the OTLP transport stack (hyper, reqwest, h2,
    // etc.) to prevent infinite feedback through the HTTP exporter.
    let otel_log_layer = if config.enable_logs {
        match init_logger(config) {
            Ok(logger_provider) => {
                let env_filter = build_console_filter(project_name, log_level);
                let log_layer = OpenTelemetryTracingBridge::new(&logger_provider).with_filter(
                    env_filter.and(FilterFn::new(|metadata| {
                        let target = metadata.target();
                        !target.starts_with("hyper")
                            && !target.starts_with("reqwest")
                            && !target.starts_with("h2")
                            && !target.starts_with("tonic")
                            && !target.starts_with("tower")
                            && !target.starts_with("opentelemetry")
                    })),
                );

                LOGGER_PROVIDER
                    .set(logger_provider)
                    .map_err(|_| TelemetryError::AlreadyInitialized)?;

                Some(log_layer)
            }
            Err(e) => {
                eprintln!("WARNING: OTLP log exporter init failed, log bridge disabled: {e}");
                None
            }
        }
    } else {
        None
    };

    // No global filter: each layer carries its own per-layer filter.
    if Registry::default()
        .with(fmt_layer)
        .with(otel_trace_layer)
        .with(otel_log_layer)
        .try_init()
        .is_err()
    {
        return Ok(false);
    }

    if config.enable_metrics {
        match init_meter(config) {
            Ok(meter_provider) => {
                METER_PROVIDER
                    .set(meter_provider.clone())
                    .map_err(|_| TelemetryError::AlreadyInitialized)?;

                global::set_meter_provider(meter_provider);
            }
            Err(e) => {
                eprintln!("WARNING: OTLP metric exporter init failed, metrics disabled: {e}");
            }
        }
    }

    tracing::info!(
        service = %config.service_name,
        version = %config.service_version,
        environment = %config.environment,
        traces = config.enable_traces,
        metrics = config.enable_metrics,
        logs = config.enable_logs,
        "telemetry initialized"
    );

    Ok(true)
}

pub fn shutdown_telemetry() {
    tracing::info!("shutting down telemetry");

    if let Some(provider) = TRACER_PROVIDER.get()
        && let Err(e) = provider.shutdown()
    {
        tracing::warn!(error = %e, "failed to shutdown tracer provider");
    }

    if let Some(provider) = METER_PROVIDER.get()
        && let Err(e) = provider.shutdown()
    {
        tracing::warn!(error = %e, "failed to shutdown meter provider");
    }

    if let Some(provider) = LOGGER_PROVIDER.get()
        && let Err(e) = provider.shutdown()
    {
        tracing::warn!(error = %e, "failed to shutdown logger provider");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_builder() {
        let config = TelemetryConfig::new("test-service")
            .with_endpoint("http://otel:4318")
            .with_version("1.0.0")
            .with_environment("production")
            .with_traces(true)
            .with_metrics(false)
            .with_logs(true);

        assert_eq!(config.service_name, "test-service");
        assert_eq!(config.otlp_endpoint, "http://otel:4318");
        assert_eq!(config.service_version, "1.0.0");
        assert_eq!(config.environment, "production");
        assert!(config.enable_traces);
        assert!(!config.enable_metrics);
        assert!(config.enable_logs);
    }

    #[test]
    fn test_default_config() {
        let config = TelemetryConfig::default();

        assert_eq!(config.otlp_endpoint, "http://localhost:4318");
        assert_eq!(config.service_name, "forge-service");
        assert_eq!(config.environment, "development");
        assert!(config.enable_traces);
        assert!(config.enable_metrics);
        assert!(config.enable_logs);
    }

    #[test]
    fn sampling_ratio_clamps_negative_to_zero() {
        let config = TelemetryConfig::default().with_sampling_ratio(-0.5);
        assert_eq!(config.sampling_ratio, 0.0);
    }

    #[test]
    fn sampling_ratio_clamps_above_one_to_one() {
        let config = TelemetryConfig::default().with_sampling_ratio(2.5);
        assert_eq!(config.sampling_ratio, 1.0);
    }

    #[test]
    fn sampling_ratio_preserves_value_in_range() {
        let config = TelemetryConfig::default().with_sampling_ratio(0.25);
        assert_eq!(config.sampling_ratio, 0.25);
    }

    #[test]
    fn from_observability_config_kill_switch_disables_all_signals() {
        // When obs.enabled = false, every enable_* flag in the resolved
        // TelemetryConfig must be false even if the per-signal flags say true.
        // This is the global off switch.
        let mut obs = forge_core::config::ObservabilityConfig::default();
        obs.enabled = false;
        obs.enable_traces = true;
        obs.enable_metrics = true;
        obs.enable_logs = true;

        let config = TelemetryConfig::from_observability_config(&obs, "proj", "1.0.0");
        assert!(!config.enable_traces);
        assert!(!config.enable_metrics);
        assert!(!config.enable_logs);
    }

    #[test]
    fn from_observability_config_respects_per_signal_disable_when_enabled() {
        let mut obs = forge_core::config::ObservabilityConfig::default();
        obs.enabled = true;
        obs.enable_traces = true;
        obs.enable_metrics = false;
        obs.enable_logs = true;

        let config = TelemetryConfig::from_observability_config(&obs, "proj", "1.0.0");
        assert!(config.enable_traces);
        assert!(!config.enable_metrics);
        assert!(config.enable_logs);
    }

    #[test]
    fn from_observability_config_falls_back_to_project_name() {
        let obs = forge_core::config::ObservabilityConfig::default();
        // service_name is None on the default; resolved config should use
        // the project name instead.
        assert!(obs.service_name.is_none());
        let config = TelemetryConfig::from_observability_config(&obs, "my-app", "2.0.0");
        assert_eq!(config.service_name, "my-app");
        assert_eq!(config.service_version, "2.0.0");
    }

    #[test]
    fn from_observability_config_uses_explicit_service_name_when_set() {
        let mut obs = forge_core::config::ObservabilityConfig::default();
        obs.service_name = Some("override-name".to_string());
        let config = TelemetryConfig::from_observability_config(&obs, "ignored", "1.0.0");
        assert_eq!(config.service_name, "override-name");
    }

    #[test]
    fn telemetry_error_display_messages_are_descriptive() {
        // These error strings end up in operator logs at startup; keep their
        // shape stable so log alerting rules don't break silently.
        assert_eq!(
            TelemetryError::TracerInit("connection refused".into()).to_string(),
            "failed to initialize tracer: connection refused"
        );
        assert_eq!(
            TelemetryError::MeterInit("dns".into()).to_string(),
            "failed to initialize meter: dns"
        );
        assert_eq!(
            TelemetryError::LoggerInit("tls".into()).to_string(),
            "failed to initialize logger: tls"
        );
        assert_eq!(
            TelemetryError::AlreadyInitialized.to_string(),
            "telemetry already initialized"
        );
        assert_eq!(
            TelemetryError::SubscriberInit("global set".into()).to_string(),
            "tracing subscriber init failed: global set"
        );
    }

    #[test]
    fn build_env_filter_returns_valid_filter_for_dashed_project() {
        // The filter logic replaces `-` with `_` so a Cargo crate name like
        // "my-app" becomes the runtime crate identifier "my_app" in the
        // tracing directive. This test exercises construction without
        // panicking; the directive's effect on filtering is tracing's own
        // contract.
        let _filter = build_env_filter("my-app", "debug");
        let _filter = build_env_filter("plain", "info");
    }
}

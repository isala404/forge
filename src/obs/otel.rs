use crate::error::{ForgeError, Result};

/// Install a global tracing subscriber that exports spans to the OTLP endpoint
/// at `endpoint` (e.g. `http://localhost:4318/v1/traces`) and also prints them
/// to stderr honoring `RUST_LOG`. Call once at startup, before `Forge::init`.
pub fn install_otlp(endpoint: &str) -> Result<()> {
    use opentelemetry::trace::TracerProvider as _;
    use opentelemetry_otlp::WithExportConfig as _;
    use tracing_subscriber::layer::SubscriberExt as _;
    use tracing_subscriber::util::SubscriberInitExt as _;

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_endpoint(endpoint)
        .build()
        .map_err(|e| ForgeError::config(format!("OTLP exporter setup failed: {e}")))?;

    let provider = opentelemetry_sdk::trace::TracerProvider::builder()
        .with_batch_exporter(exporter, opentelemetry_sdk::runtime::Tokio)
        .build();

    let tracer = provider.tracer("forge");
    let otel_layer = tracing_opentelemetry::layer().with_tracer(tracer);

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(filter)
        .with(tracing_subscriber::fmt::layer())
        .with(otel_layer)
        .try_init()
        .map_err(|e| ForgeError::config(format!("tracing subscriber init failed: {e}")))?;

    opentelemetry::global::set_tracer_provider(provider);
    Ok(())
}

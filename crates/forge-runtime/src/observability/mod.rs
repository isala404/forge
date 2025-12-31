mod alerts;
mod collector;
mod config;
mod partitions;
mod storage;
mod tracing_layer;

pub use alerts::{
    Alert, AlertCondition, AlertEvaluator, AlertRule, AlertSeverity, AlertStatus, AlertStore,
};
pub use collector::{
    LogCollector, MetricsCollector, SystemMetricsCollector, SystemMetricsSnapshot, TraceCollector,
};
pub use config::{LogsConfig, MetricsConfig, ObservabilityConfig, TracesConfig};
pub use partitions::{PartitionConfig, PartitionGranularity, PartitionManager};
pub use storage::{LogStore, MetricsStore, TraceStore, TraceSummary};
pub use tracing_layer::ForgeTracingLayer;

use std::sync::Arc;
use std::time::Duration;

use forge_core::observability::{LogEntry, Metric, Span};
use forge_core::Result;
use tokio::sync::RwLock;

/// Shared observability state for the runtime.
///
/// This struct encapsulates all observability components (collectors and stores)
/// and provides a unified interface for recording and querying observability data.
#[derive(Clone)]
pub struct ObservabilityState {
    /// Metrics collector for buffering metrics.
    pub metrics_collector: Arc<MetricsCollector>,
    /// Log collector for buffering logs.
    pub log_collector: Arc<LogCollector>,
    /// Trace collector for buffering traces.
    pub trace_collector: Arc<TraceCollector>,
    /// System metrics collector.
    pub system_metrics: Arc<SystemMetricsCollector>,
    /// Metrics store for persistence.
    pub metrics_store: Arc<MetricsStore>,
    /// Log store for persistence.
    pub log_store: Arc<LogStore>,
    /// Trace store for persistence.
    pub trace_store: Arc<TraceStore>,
    /// Alert store for persistence.
    pub alert_store: Arc<AlertStore>,
    /// Configuration.
    config: ObservabilityConfig,
    /// Whether observability is enabled.
    enabled: bool,
    /// Shutdown flag.
    shutdown: Arc<RwLock<bool>>,
}

impl ObservabilityState {
    /// Create a new observability state from config and database pool.
    pub fn new(config: ObservabilityConfig, pool: sqlx::PgPool) -> Self {
        let enabled = config.enabled;

        // Create collectors
        let metrics_collector = Arc::new(MetricsCollector::new(config.metrics.clone()));
        let log_collector = Arc::new(LogCollector::new(config.logs.clone()));
        let trace_collector = Arc::new(TraceCollector::new(config.traces.clone()));
        let system_metrics = Arc::new(SystemMetricsCollector::new());

        // Create stores
        let metrics_store = Arc::new(MetricsStore::new(pool.clone()));
        let log_store = Arc::new(LogStore::new(pool.clone()));
        let trace_store = Arc::new(TraceStore::new(pool.clone()));
        let alert_store = Arc::new(AlertStore::new(pool));

        Self {
            metrics_collector,
            log_collector,
            trace_collector,
            system_metrics,
            metrics_store,
            log_store,
            trace_store,
            alert_store,
            config,
            enabled,
            shutdown: Arc::new(RwLock::new(false)),
        }
    }

    /// Check if observability is enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Record a metric.
    pub async fn record_metric(&self, metric: Metric) {
        if self.enabled {
            self.metrics_collector.record(metric).await;
        }
    }

    /// Increment a counter metric.
    pub async fn increment_counter(&self, name: impl Into<String>, value: f64) {
        if self.enabled {
            self.metrics_collector.increment_counter(name, value).await;
        }
    }

    /// Set a gauge metric.
    pub async fn set_gauge(&self, name: impl Into<String>, value: f64) {
        if self.enabled {
            self.metrics_collector.set_gauge(name, value).await;
        }
    }

    /// Record a log entry.
    pub async fn record_log(&self, log: LogEntry) {
        if self.enabled {
            self.log_collector.record(log).await;
        }
    }

    /// Log at info level.
    pub async fn info(&self, message: impl Into<String>) {
        if self.enabled {
            self.log_collector.info(message).await;
        }
    }

    /// Log at warn level.
    pub async fn warn(&self, message: impl Into<String>) {
        if self.enabled {
            self.log_collector.warn(message).await;
        }
    }

    /// Log at error level.
    pub async fn error(&self, message: impl Into<String>) {
        if self.enabled {
            self.log_collector.error(message).await;
        }
    }

    /// Record a span.
    pub async fn record_span(&self, span: Span) {
        if self.enabled {
            self.trace_collector.record(span).await;
        }
    }

    /// Flush all collectors and persist to stores.
    pub async fn flush(&self) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        // Flush metrics
        self.metrics_collector.flush().await;

        // Flush logs
        self.log_collector.flush().await;

        // Flush traces
        self.trace_collector.flush().await;

        Ok(())
    }

    /// Start background flush loops.
    ///
    /// This spawns tasks that periodically flush collectors to stores
    /// and run cleanup based on retention policies.
    pub fn start_background_tasks(&self) -> Vec<tokio::task::JoinHandle<()>> {
        let mut handles = Vec::new();

        if !self.enabled {
            return handles;
        }

        // Metrics flush loop
        {
            let collector = self.metrics_collector.clone();
            let store = self.metrics_store.clone();
            let interval = self.config.metrics.flush_interval;
            let shutdown = self.shutdown.clone();

            handles.push(tokio::spawn(async move {
                let mut ticker = tokio::time::interval(interval);
                loop {
                    ticker.tick().await;

                    if *shutdown.read().await {
                        break;
                    }

                    // Drain collector buffer and persist to store
                    let metrics = collector.drain().await;
                    if !metrics.is_empty() {
                        if let Err(e) = store.store(metrics).await {
                            tracing::warn!("Failed to persist metrics: {}", e);
                        }
                    }
                }
            }));
        }

        // Logs flush loop
        {
            let collector = self.log_collector.clone();
            let store = self.log_store.clone();
            let shutdown = self.shutdown.clone();
            let interval = Duration::from_secs(10); // Logs flush every 10s

            handles.push(tokio::spawn(async move {
                let mut ticker = tokio::time::interval(interval);
                loop {
                    ticker.tick().await;

                    if *shutdown.read().await {
                        break;
                    }

                    // Drain collector buffer and persist to store
                    let logs = collector.drain().await;
                    if !logs.is_empty() {
                        if let Err(e) = store.store(logs).await {
                            tracing::warn!("Failed to persist logs: {}", e);
                        }
                    }
                }
            }));
        }

        // Traces flush loop
        {
            let collector = self.trace_collector.clone();
            let store = self.trace_store.clone();
            let shutdown = self.shutdown.clone();
            let interval = Duration::from_secs(10); // Traces flush every 10s

            handles.push(tokio::spawn(async move {
                let mut ticker = tokio::time::interval(interval);
                loop {
                    ticker.tick().await;

                    if *shutdown.read().await {
                        break;
                    }

                    // Drain collector buffer and persist to store
                    let spans = collector.drain().await;
                    if !spans.is_empty() {
                        if let Err(e) = store.store(spans).await {
                            tracing::warn!("Failed to persist traces: {}", e);
                        }
                    }
                }
            }));
        }

        // System metrics collection loop (every 15 seconds)
        {
            let handle = self
                .system_metrics
                .start(self.metrics_collector.clone(), Duration::from_secs(15));
            handles.push(handle);
        }

        // Cleanup loop (runs less frequently)
        {
            let metrics_store = self.metrics_store.clone();
            let log_store = self.log_store.clone();
            let trace_store = self.trace_store.clone();
            let metrics_retention = self.config.metrics.raw_retention;
            let logs_retention = self.config.logs.retention;
            let traces_retention = self.config.traces.retention;
            let shutdown = self.shutdown.clone();

            handles.push(tokio::spawn(async move {
                let mut ticker = tokio::time::interval(Duration::from_secs(3600)); // Cleanup every hour
                loop {
                    ticker.tick().await;

                    if *shutdown.read().await {
                        break;
                    }

                    // Run cleanup for each store
                    if let Err(e) = metrics_store.cleanup(metrics_retention).await {
                        tracing::warn!("Metrics cleanup error: {}", e);
                    }

                    if let Err(e) = log_store.cleanup(logs_retention).await {
                        tracing::warn!("Logs cleanup error: {}", e);
                    }

                    if let Err(e) = trace_store.cleanup(traces_retention).await {
                        tracing::warn!("Traces cleanup error: {}", e);
                    }
                }
            }));
        }

        handles
    }

    /// Signal shutdown to background tasks.
    pub async fn shutdown(&self) {
        let mut shutdown = self.shutdown.write().await;
        *shutdown = true;

        // Stop system metrics collector
        self.system_metrics.stop().await;

        // Final flush
        let _ = self.flush().await;
    }

    /// Get a tracing layer that forwards logs to the LogCollector.
    ///
    /// Use this to add log collection to your tracing subscriber:
    /// ```ignore
    /// use tracing_subscriber::layer::SubscriberExt;
    /// use tracing_subscriber::util::SubscriberInitExt;
    ///
    /// tracing_subscriber::registry()
    ///     .with(observability.tracing_layer())
    ///     .with(tracing_subscriber::fmt::layer())
    ///     .init();
    /// ```
    pub fn tracing_layer(&self) -> ForgeTracingLayer {
        ForgeTracingLayer::new(self.log_collector.clone())
    }
}

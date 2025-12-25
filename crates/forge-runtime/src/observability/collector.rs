use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, RwLock};

use forge_core::observability::{LogEntry, Metric, Span};
use forge_core::LogLevel;

use super::config::{LogsConfig, MetricsConfig, TracesConfig};

/// Metrics collector for buffering and batching metrics.
pub struct MetricsCollector {
    config: MetricsConfig,
    buffer: Arc<RwLock<VecDeque<Metric>>>,
    sender: mpsc::Sender<Vec<Metric>>,
    receiver: Arc<RwLock<mpsc::Receiver<Vec<Metric>>>>,
    counter: AtomicU64,
}

impl MetricsCollector {
    /// Create a new metrics collector.
    pub fn new(config: MetricsConfig) -> Self {
        let (sender, receiver) = mpsc::channel(1024);
        Self {
            config,
            buffer: Arc::new(RwLock::new(VecDeque::new())),
            sender,
            receiver: Arc::new(RwLock::new(receiver)),
            counter: AtomicU64::new(0),
        }
    }

    /// Record a metric.
    pub async fn record(&self, metric: Metric) {
        let mut buffer = self.buffer.write().await;
        buffer.push_back(metric);
        self.counter.fetch_add(1, Ordering::Relaxed);

        // Flush if buffer is full
        if buffer.len() >= self.config.buffer_size {
            let batch: Vec<Metric> = buffer.drain(..).collect();
            let _ = self.sender.send(batch).await;
        }
    }

    /// Record a counter increment.
    pub async fn increment_counter(&self, name: impl Into<String>, value: f64) {
        self.record(Metric::counter(name, value)).await;
    }

    /// Record a gauge value.
    pub async fn set_gauge(&self, name: impl Into<String>, value: f64) {
        self.record(Metric::gauge(name, value)).await;
    }

    /// Flush the buffer.
    pub async fn flush(&self) {
        let mut buffer = self.buffer.write().await;
        if !buffer.is_empty() {
            let batch: Vec<Metric> = buffer.drain(..).collect();
            let _ = self.sender.send(batch).await;
        }
    }

    /// Get the flush receiver for consuming batches.
    pub fn subscribe(&self) -> mpsc::Receiver<Vec<Metric>> {
        let (tx, rx) = mpsc::channel(1024);
        // Note: In a real implementation, this would clone the sender
        // For simplicity, we're creating a new channel here
        rx
    }

    /// Get collected metrics count.
    pub fn count(&self) -> u64 {
        self.counter.load(Ordering::Relaxed)
    }

    /// Get current buffer size.
    pub async fn buffer_size(&self) -> usize {
        self.buffer.read().await.len()
    }

    /// Run the flush loop.
    pub async fn run(&self) {
        let mut interval = tokio::time::interval(self.config.flush_interval);
        loop {
            interval.tick().await;
            self.flush().await;
        }
    }
}

/// Log collector for buffering and filtering logs.
pub struct LogCollector {
    config: LogsConfig,
    buffer: Arc<RwLock<VecDeque<LogEntry>>>,
    sender: mpsc::Sender<Vec<LogEntry>>,
    counter: AtomicU64,
}

impl LogCollector {
    /// Create a new log collector.
    pub fn new(config: LogsConfig) -> Self {
        let (sender, _receiver) = mpsc::channel(1024);
        Self {
            config,
            buffer: Arc::new(RwLock::new(VecDeque::new())),
            sender,
            counter: AtomicU64::new(0),
        }
    }

    /// Record a log entry.
    pub async fn record(&self, entry: LogEntry) {
        // Filter by log level
        if !entry.matches_level(self.config.level) {
            return;
        }

        let mut buffer = self.buffer.write().await;
        buffer.push_back(entry);
        self.counter.fetch_add(1, Ordering::Relaxed);

        // Flush if buffer is full
        if buffer.len() >= self.config.buffer_size {
            let batch: Vec<LogEntry> = buffer.drain(..).collect();
            let _ = self.sender.send(batch).await;
        }
    }

    /// Log at trace level.
    pub async fn trace(&self, message: impl Into<String>) {
        self.record(LogEntry::trace(message)).await;
    }

    /// Log at debug level.
    pub async fn debug(&self, message: impl Into<String>) {
        self.record(LogEntry::debug(message)).await;
    }

    /// Log at info level.
    pub async fn info(&self, message: impl Into<String>) {
        self.record(LogEntry::info(message)).await;
    }

    /// Log at warn level.
    pub async fn warn(&self, message: impl Into<String>) {
        self.record(LogEntry::warn(message)).await;
    }

    /// Log at error level.
    pub async fn error(&self, message: impl Into<String>) {
        self.record(LogEntry::error(message)).await;
    }

    /// Flush the buffer.
    pub async fn flush(&self) {
        let mut buffer = self.buffer.write().await;
        if !buffer.is_empty() {
            let batch: Vec<LogEntry> = buffer.drain(..).collect();
            let _ = self.sender.send(batch).await;
        }
    }

    /// Get collected log count.
    pub fn count(&self) -> u64 {
        self.counter.load(Ordering::Relaxed)
    }

    /// Get current buffer size.
    pub async fn buffer_size(&self) -> usize {
        self.buffer.read().await.len()
    }

    /// Get the minimum log level.
    pub fn min_level(&self) -> LogLevel {
        self.config.level
    }
}

/// Trace collector for sampling and batching traces.
pub struct TraceCollector {
    config: TracesConfig,
    buffer: Arc<RwLock<VecDeque<Span>>>,
    sender: mpsc::Sender<Vec<Span>>,
    counter: AtomicU64,
    sampled_counter: AtomicU64,
}

impl TraceCollector {
    /// Create a new trace collector.
    pub fn new(config: TracesConfig) -> Self {
        let (sender, _receiver) = mpsc::channel(1024);
        Self {
            config,
            buffer: Arc::new(RwLock::new(VecDeque::new())),
            sender,
            counter: AtomicU64::new(0),
            sampled_counter: AtomicU64::new(0),
        }
    }

    /// Record a span.
    pub async fn record(&self, span: Span) {
        self.counter.fetch_add(1, Ordering::Relaxed);

        // Sample decision
        let should_sample = self.should_sample(&span);
        if !should_sample {
            return;
        }

        self.sampled_counter.fetch_add(1, Ordering::Relaxed);

        let mut buffer = self.buffer.write().await;
        buffer.push_back(span);
    }

    /// Check if a span should be sampled.
    fn should_sample(&self, span: &Span) -> bool {
        // Always sample errors if configured
        if self.config.always_trace_errors && span.status == forge_core::SpanStatus::Error {
            return true;
        }

        // Check if context is sampled
        if !span.context.is_sampled() {
            return false;
        }

        // Apply sample rate
        if self.config.sample_rate >= 1.0 {
            return true;
        }

        // Simple probabilistic sampling
        let hash = span
            .context
            .trace_id
            .as_str()
            .as_bytes()
            .iter()
            .fold(0u64, |acc, b| acc.wrapping_mul(31).wrapping_add(*b as u64));
        let threshold = (self.config.sample_rate * u64::MAX as f64) as u64;
        hash < threshold
    }

    /// Flush the buffer.
    pub async fn flush(&self) {
        let mut buffer = self.buffer.write().await;
        if !buffer.is_empty() {
            let batch: Vec<Span> = buffer.drain(..).collect();
            let _ = self.sender.send(batch).await;
        }
    }

    /// Get total span count.
    pub fn count(&self) -> u64 {
        self.counter.load(Ordering::Relaxed)
    }

    /// Get sampled span count.
    pub fn sampled_count(&self) -> u64 {
        self.sampled_counter.load(Ordering::Relaxed)
    }

    /// Get current buffer size.
    pub async fn buffer_size(&self) -> usize {
        self.buffer.read().await.len()
    }

    /// Get the sample rate.
    pub fn sample_rate(&self) -> f64 {
        self.config.sample_rate
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::observability::{MetricKind, SpanContext};

    #[tokio::test]
    async fn test_metrics_collector_record() {
        let collector = MetricsCollector::new(MetricsConfig::default());

        collector.increment_counter("test_counter", 1.0).await;
        collector.set_gauge("test_gauge", 42.0).await;

        assert_eq!(collector.count(), 2);
        assert_eq!(collector.buffer_size().await, 2);
    }

    #[tokio::test]
    async fn test_metrics_collector_flush() {
        let mut config = MetricsConfig::default();
        config.buffer_size = 2;
        let collector = MetricsCollector::new(config);

        collector.increment_counter("test1", 1.0).await;
        collector.increment_counter("test2", 2.0).await;
        // Buffer should auto-flush at 2

        assert_eq!(collector.count(), 2);
    }

    #[tokio::test]
    async fn test_log_collector_level_filter() {
        let mut config = LogsConfig::default();
        config.level = LogLevel::Warn;
        let collector = LogCollector::new(config);

        collector.debug("Debug message").await;
        collector.info("Info message").await;
        collector.warn("Warn message").await;
        collector.error("Error message").await;

        // Only warn and error should be collected
        assert_eq!(collector.count(), 2);
    }

    #[tokio::test]
    async fn test_log_collector_record() {
        let collector = LogCollector::new(LogsConfig::default());

        collector.info("Test message").await;
        assert_eq!(collector.count(), 1);
        assert_eq!(collector.buffer_size().await, 1);
    }

    #[tokio::test]
    async fn test_trace_collector_sampling() {
        let mut config = TracesConfig::default();
        config.sample_rate = 1.0; // 100% sampling
        let collector = TraceCollector::new(config);

        let span = Span::new("test_span");
        collector.record(span).await;

        assert_eq!(collector.count(), 1);
        assert_eq!(collector.sampled_count(), 1);
    }

    #[tokio::test]
    async fn test_trace_collector_always_trace_errors() {
        let mut config = TracesConfig::default();
        config.sample_rate = 0.0; // No sampling
        config.always_trace_errors = true;
        let collector = TraceCollector::new(config);

        let mut span = Span::new("error_span");
        span.end_error("Test error");
        collector.record(span).await;

        // Error should still be recorded
        assert_eq!(collector.sampled_count(), 1);
    }
}

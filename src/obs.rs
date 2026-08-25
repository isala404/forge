use crate::backend::Primitive;
use crate::error::{ForgeError, Result};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::sync::Mutex;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use tracing::Instrument;

const HISTOGRAM_BUCKETS: [f64; 9] = [
    0.001,
    0.005,
    0.010,
    0.025,
    0.050,
    0.100,
    0.250,
    1.0,
    f64::INFINITY,
];

/// One bounded-cardinality metric in a point-in-time Forge instance snapshot.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct MetricSample {
    pub name: String,
    pub kind: String,
    pub labels: HashMap<String, String>,
    pub value: f64,
    pub count: Option<u64>,
    pub sum: Option<f64>,
}

/// Options for a bounded live dependency probe.
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct ProbeOptions {
    pub deadline: Duration,
    /// Backends that decide readiness. An empty list means every enabled backend.
    pub readiness_backends: Vec<Primitive>,
}

impl Default for ProbeOptions {
    fn default() -> Self {
        Self {
            deadline: Duration::from_secs(2),
            readiness_backends: Vec::new(),
        }
    }
}

impl ProbeOptions {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn with_deadline(mut self, deadline: Duration) -> Self {
        self.deadline = deadline;
        self
    }

    pub fn with_readiness_backends(
        mut self,
        backends: impl IntoIterator<Item = Primitive>,
    ) -> Self {
        self.readiness_backends = backends.into_iter().collect();
        self
    }
}

/// One backend's result from a live probe. Messages are fixed, redacted text.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct BackendHealth {
    pub primitive: Primitive,
    pub provider: String,
    pub status: String,
    pub latency_ms: f64,
    pub error_category: Option<String>,
    pub last_success_ms: Option<f64>,
    pub message: String,
}

/// Process liveness plus dependency readiness from one bounded probe pass.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq)]
pub struct HealthReport {
    pub live: bool,
    pub ready: bool,
    pub checked_at_ms: f64,
    pub duration_ms: f64,
    pub backends: Vec<BackendHealth>,
}

/// One bounded, secret-safe deployment or operator diagnostic.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticCheck {
    pub name: String,
    pub status: String,
    pub message: String,
}

/// Structured diagnostics for admin endpoints, deployment checks, and tests.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsReport {
    pub ready: bool,
    pub checked_at_ms: u64,
    pub checks: Vec<DiagnosticCheck>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
struct MetricKey {
    name: &'static str,
    labels: Vec<(&'static str, &'static str)>,
}

#[derive(Debug, Clone)]
enum MetricValue {
    Counter(f64),
    Gauge(f64),
    Histogram {
        count: u64,
        sum: f64,
        buckets: [u64; HISTOGRAM_BUCKETS.len()],
    },
}

/// Per-handle observation state. Nothing is registered globally and there is no
/// background collector. A poisoned diagnostic lock is recovered because telemetry
/// must never take the application down.
pub(crate) struct Observability {
    metrics: Mutex<BTreeMap<MetricKey, MetricValue>>,
    last_success: Mutex<HashMap<Primitive, SystemTime>>,
}

impl Observability {
    pub(crate) fn new() -> Self {
        Self {
            metrics: Mutex::new(BTreeMap::new()),
            last_success: Mutex::new(HashMap::new()),
        }
    }

    fn metrics(&self) -> std::sync::MutexGuard<'_, BTreeMap<MetricKey, MetricValue>> {
        match self.metrics.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    fn successes(&self) -> std::sync::MutexGuard<'_, HashMap<Primitive, SystemTime>> {
        match self.last_success.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    pub(crate) fn counter(
        &self,
        name: &'static str,
        labels: &[(&'static str, &'static str)],
        value: u64,
    ) {
        let key = metric_key(name, labels);
        let mut metrics = self.metrics();
        match metrics.entry(key).or_insert(MetricValue::Counter(0.0)) {
            MetricValue::Counter(current) => *current += value as f64,
            MetricValue::Gauge(_) | MetricValue::Histogram { .. } => {}
        }
    }

    pub(crate) fn gauge(
        &self,
        name: &'static str,
        labels: &[(&'static str, &'static str)],
        value: f64,
    ) {
        self.metrics()
            .insert(metric_key(name, labels), MetricValue::Gauge(value));
    }

    pub(crate) fn histogram(
        &self,
        name: &'static str,
        labels: &[(&'static str, &'static str)],
        value: f64,
    ) {
        let key = metric_key(name, labels);
        let mut metrics = self.metrics();
        let metric = metrics.entry(key).or_insert(MetricValue::Histogram {
            count: 0,
            sum: 0.0,
            buckets: [0; HISTOGRAM_BUCKETS.len()],
        });
        if let MetricValue::Histogram {
            count,
            sum,
            buckets,
        } = metric
        {
            *count = count.saturating_add(1);
            *sum += value;
            for (index, upper) in HISTOGRAM_BUCKETS.iter().enumerate() {
                if value <= *upper
                    && let Some(bucket) = buckets.get_mut(index)
                {
                    *bucket = bucket.saturating_add(1);
                }
            }
        }
    }

    pub(crate) async fn operation<T, F>(
        &self,
        primitive: &'static str,
        op: &'static str,
        future: F,
    ) -> Result<T>
    where
        F: Future<Output = Result<T>>,
    {
        let started = Instant::now();
        let span = tracing::info_span!(
            "forge.operation",
            forge.primitive = primitive,
            forge.operation = op,
            outcome = tracing::field::Empty,
            error.variant = tracing::field::Empty,
        );
        let result = future.instrument(span.clone()).await;
        let outcome = match &result {
            Ok(_) => "ok",
            Err(error) => error_variant(error),
        };
        span.record("outcome", outcome);
        if result.is_err() {
            span.record("error.variant", outcome);
        }
        self.counter(
            "forge_operations_total",
            &[
                ("primitive", primitive),
                ("operation", op),
                ("outcome", outcome),
            ],
            1,
        );
        self.histogram(
            "forge_operation_duration_seconds",
            &[("primitive", primitive), ("operation", op)],
            started.elapsed().as_secs_f64(),
        );
        result
    }

    pub(crate) fn mark_probe_success(&self, primitive: Primitive, at: SystemTime) {
        self.successes().insert(primitive, at);
    }

    pub(crate) fn last_probe_success(&self, primitive: Primitive) -> Option<SystemTime> {
        self.successes().get(&primitive).copied()
    }

    pub(crate) fn snapshot(&self) -> Vec<MetricSample> {
        self.metrics()
            .iter()
            .map(|(key, value)| {
                let labels = key
                    .labels
                    .iter()
                    .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
                    .collect();
                match value {
                    MetricValue::Counter(value) => MetricSample {
                        name: key.name.to_string(),
                        kind: "counter".to_string(),
                        labels,
                        value: *value,
                        count: None,
                        sum: None,
                    },
                    MetricValue::Gauge(value) => MetricSample {
                        name: key.name.to_string(),
                        kind: "gauge".to_string(),
                        labels,
                        value: *value,
                        count: None,
                        sum: None,
                    },
                    MetricValue::Histogram { count, sum, .. } => MetricSample {
                        name: key.name.to_string(),
                        kind: "histogram".to_string(),
                        labels,
                        value: 0.0,
                        count: Some(*count),
                        sum: Some(*sum),
                    },
                }
            })
            .collect()
    }

    pub(crate) fn render_prometheus(&self) -> String {
        let metrics = self.metrics();
        let mut output = String::new();
        let mut documented = HashSet::new();
        for (key, value) in metrics.iter() {
            if documented.insert(key.name) {
                let kind = match value {
                    MetricValue::Counter(_) => "counter",
                    MetricValue::Gauge(_) => "gauge",
                    MetricValue::Histogram { .. } => "histogram",
                };
                output.push_str("# TYPE ");
                output.push_str(key.name);
                output.push(' ');
                output.push_str(kind);
                output.push('\n');
            }
            match value {
                MetricValue::Counter(value) | MetricValue::Gauge(value) => {
                    render_sample(&mut output, key.name, &key.labels, None, *value);
                }
                MetricValue::Histogram {
                    count,
                    sum,
                    buckets,
                } => {
                    for (upper, bucket) in HISTOGRAM_BUCKETS.iter().zip(buckets) {
                        let bound = if upper.is_infinite() {
                            "+Inf".to_string()
                        } else {
                            upper.to_string()
                        };
                        render_sample(
                            &mut output,
                            &format!("{}_bucket", key.name),
                            &key.labels,
                            Some(("le", &bound)),
                            *bucket as f64,
                        );
                    }
                    render_sample(
                        &mut output,
                        &format!("{}_sum", key.name),
                        &key.labels,
                        None,
                        *sum,
                    );
                    render_sample(
                        &mut output,
                        &format!("{}_count", key.name),
                        &key.labels,
                        None,
                        *count as f64,
                    );
                }
            }
        }
        output
    }
}

fn metric_key(name: &'static str, labels: &[(&'static str, &'static str)]) -> MetricKey {
    let mut labels = labels.to_vec();
    labels.sort_unstable();
    MetricKey { name, labels }
}

fn render_sample(
    output: &mut String,
    name: &str,
    labels: &[(&str, &str)],
    extra: Option<(&str, &str)>,
    value: f64,
) {
    output.push_str(name);
    if !labels.is_empty() || extra.is_some() {
        output.push('{');
        let mut first = true;
        for (label, value) in labels.iter().copied().chain(extra) {
            if !first {
                output.push(',');
            }
            first = false;
            output.push_str(label);
            output.push_str("=\"");
            output.push_str(&prometheus_escape(value));
            output.push('"');
        }
        output.push('}');
    }
    output.push(' ');
    output.push_str(&value.to_string());
    output.push('\n');
}

fn prometheus_escape(value: &str) -> String {
    value
        .replace('\\', "\\\\")
        .replace('\n', "\\n")
        .replace('"', "\\\"")
}

pub(crate) fn unix_ms(time: SystemTime) -> f64 {
    time.duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs_f64()
        * 1000.0
}

/// Stable, secret-free variant label for spans and metrics. Never the message.
pub(crate) fn error_variant(e: &ForgeError) -> &'static str {
    match e {
        ForgeError::Context { source, .. } => error_variant(source),
        ForgeError::Config(_) => "config",
        ForgeError::NotConfigured(_) => "not_configured",
        ForgeError::Unavailable(_) => "unavailable",
        ForgeError::NotFound => "not_found",
        ForgeError::Precondition(_) => "precondition",
        ForgeError::Limit(_) => "limit",
        ForgeError::Invalid(_) => "invalid",
        ForgeError::Backend { .. } => "backend",
    }
}

pub(crate) fn safe_probe_message(error: &ForgeError) -> &'static str {
    match error {
        ForgeError::Context { source, .. } => safe_probe_message(source),
        ForgeError::Config(_) => "backend configuration is invalid",
        ForgeError::NotConfigured(_) => "backend is not configured",
        ForgeError::Unavailable(_) => "backend is temporarily unavailable",
        ForgeError::NotFound => "probe target was not found",
        ForgeError::Precondition(_) => "backend precondition failed",
        ForgeError::Limit(_) => "backend limit prevented the probe",
        ForgeError::Invalid(_) => "probe request was invalid",
        ForgeError::Backend { .. } => "backend operation failed",
    }
}

/// Run `fut` inside `span` and record its outcome.
pub(crate) async fn instrument<T, F>(
    _primitive: &'static str,
    _op: &'static str,
    span: tracing::Span,
    fut: F,
) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    let result = fut.instrument(span.clone()).await;
    let outcome = match &result {
        Ok(_) => "ok",
        Err(e) => error_variant(e),
    };
    span.record("outcome", outcome);
    if result.is_err() {
        span.record("error.variant", outcome);
    }
    result
}

#[cfg(feature = "otel")]
mod otel;
#[cfg(feature = "otel")]
pub use otel::install_otlp;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_variants_have_stable_labels() {
        assert_eq!(error_variant(&ForgeError::NotFound), "not_found");
        assert_eq!(error_variant(&ForgeError::invalid("x")), "invalid");
        assert_eq!(error_variant(&ForgeError::backend("secret")), "backend");
    }

    #[tokio::test]
    async fn instance_metrics_render_without_user_values() {
        let obs = Observability::new();
        let value = obs
            .operation("kv", "get", async { Ok::<_, ForgeError>(7u8) })
            .await
            .expect("operation succeeds");
        assert_eq!(value, 7);
        let text = obs.render_prometheus();
        assert!(text.contains("forge_operations_total"));
        assert!(text.contains("primitive=\"kv\""));
        assert!(text.ends_with('\n'));
        assert!(!text.contains("secret"));
    }

    #[test]
    fn prometheus_labels_are_escaped() {
        assert_eq!(prometheus_escape("a\\b\n\"c"), "a\\\\b\\n\\\"c");
    }
}

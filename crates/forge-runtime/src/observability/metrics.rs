use opentelemetry::{
    KeyValue, global,
    metrics::{Counter, Gauge, Histogram, UpDownCounter},
};
use std::sync::OnceLock;

const METER_NAME: &str = "forge-runtime";

static HTTP_METRICS: OnceLock<HttpMetrics> = OnceLock::new();
static FN_METRICS: OnceLock<FnMetrics> = OnceLock::new();
static FN_CACHE_METRICS: OnceLock<FnCacheMetrics> = OnceLock::new();
static JOB_METRICS: OnceLock<JobMetrics> = OnceLock::new();
static CONNECTIONS_GAUGE: OnceLock<ActiveConnectionsGauge> = OnceLock::new();
static NOTIFY_METRICS: OnceLock<NotifyMetrics> = OnceLock::new();
static SUBSCRIPTION_METRICS: OnceLock<SubscriptionMetrics> = OnceLock::new();
static WORKFLOW_SCHEDULER_METRICS: OnceLock<WorkflowSchedulerMetrics> = OnceLock::new();

pub struct HttpMetrics {
    requests_total: Counter<u64>,
    request_duration: Histogram<f64>,
}

impl HttpMetrics {
    fn new() -> Self {
        let meter = global::meter(METER_NAME);

        let requests_total = meter
            .u64_counter("http_requests_total")
            .with_description("Total number of HTTP requests")
            .with_unit("requests")
            .build();

        let request_duration = meter
            .f64_histogram("http_request_duration_seconds")
            .with_description("HTTP request duration in seconds")
            .with_unit("s")
            .build();

        Self {
            requests_total,
            request_duration,
        }
    }

    pub fn record(&self, method: &str, path: &str, status: u16, duration_secs: f64) {
        let attributes = [
            KeyValue::new("method", method.to_string()),
            KeyValue::new("path", path.to_string()),
            KeyValue::new("status", i64::from(status)),
        ];

        self.requests_total.add(1, &attributes);
        self.request_duration.record(duration_secs, &attributes);
    }
}

pub struct FnMetrics {
    executions_total: Counter<u64>,
    duration: Histogram<f64>,
}

impl FnMetrics {
    fn new() -> Self {
        let meter = global::meter(METER_NAME);

        let executions_total = meter
            .u64_counter("fn.executions_total")
            .with_description("Total function executions")
            .with_unit("executions")
            .build();

        let duration = meter
            .f64_histogram("fn.duration_seconds")
            .with_description("Function execution duration")
            .with_unit("s")
            .build();

        Self {
            executions_total,
            duration,
        }
    }

    pub fn record(
        &self,
        function: &str,
        kind: &str,
        success: bool,
        cached: bool,
        duration_secs: f64,
    ) {
        let status = if success { "ok" } else { "error" };
        let attributes = [
            KeyValue::new("function", function.to_string()),
            KeyValue::new("kind", kind.to_string()),
            KeyValue::new("status", status),
            KeyValue::new("cached", cached),
        ];

        self.executions_total.add(1, &attributes);
        self.duration.record(duration_secs, &attributes);
    }
}

pub struct FnCacheMetrics {
    hits_total: Counter<u64>,
    misses_total: Counter<u64>,
}

impl FnCacheMetrics {
    fn new() -> Self {
        let meter = global::meter(METER_NAME);

        let hits_total = meter
            .u64_counter("fn.cache.hits_total")
            .with_description("Total query cache hits")
            .with_unit("hits")
            .build();

        let misses_total = meter
            .u64_counter("fn.cache.misses_total")
            .with_description("Total query cache misses")
            .with_unit("misses")
            .build();

        Self {
            hits_total,
            misses_total,
        }
    }

    pub fn record(&self, function: &str, hit: bool) {
        let attributes = [KeyValue::new("function", function.to_string())];
        if hit {
            self.hits_total.add(1, &attributes);
        } else {
            self.misses_total.add(1, &attributes);
        }
    }
}

pub struct JobMetrics {
    executions_total: Counter<u64>,
    duration: Histogram<f64>,
    lost_claim_total: Counter<u64>,
}

impl JobMetrics {
    fn new() -> Self {
        let meter = global::meter(METER_NAME);

        let executions_total = meter
            .u64_counter("job_executions_total")
            .with_description("Total number of job executions")
            .with_unit("executions")
            .build();

        let duration = meter
            .f64_histogram("job_duration_seconds")
            .with_description("Job execution duration in seconds")
            .with_unit("s")
            .build();

        let lost_claim_total = meter
            .u64_counter("worker_lost_claim_total")
            .with_description(
                "Number of times a worker lost a job claim to a stale-reclaim race. \
                 Each lost claim consumes one semaphore permit for the duration of \
                 the start() fence check. Sustained elevation indicates stale_threshold \
                 is set too low for the observed heartbeat latency.",
            )
            .with_unit("claims")
            .build();

        Self {
            executions_total,
            duration,
            lost_claim_total,
        }
    }

    pub fn record(&self, job_type: &str, status: &'static str, duration_secs: f64) {
        let attributes = [
            KeyValue::new("job_type", job_type.to_string()),
            KeyValue::new("status", status),
        ];

        self.executions_total.add(1, &attributes);
        self.duration.record(duration_secs, &attributes);
    }

    pub fn record_lost_claim(&self, job_type: &str) {
        self.lost_claim_total
            .add(1, &[KeyValue::new("job_type", job_type.to_string())]);
    }
}

pub struct ActiveConnectionsGauge {
    gauge: UpDownCounter<i64>,
}

impl ActiveConnectionsGauge {
    fn new() -> Self {
        let meter = global::meter(METER_NAME);

        let gauge = meter
            .i64_up_down_counter("active_connections")
            .with_description("Number of active connections")
            .with_unit("connections")
            .build();

        Self { gauge }
    }

    pub fn increment(&self, connection_type: &'static str) {
        self.gauge.add(1, &[KeyValue::new("type", connection_type)]);
    }

    pub fn decrement(&self, connection_type: &'static str) {
        self.gauge
            .add(-1, &[KeyValue::new("type", connection_type)]);
    }

    pub fn set(&self, connection_type: &'static str, delta: i64) {
        self.gauge
            .add(delta, &[KeyValue::new("type", connection_type)]);
    }
}

/// Metrics for PostgreSQL NOTIFY payload sizes.
///
/// The PG NOTIFY payload ceiling is 8 KiB. `NotifyChannel` enforces a 7 KiB
/// soft limit but payload growth is invisible without a metric. Tracking
/// payload bytes makes it easy to spot channels approaching the truncation
/// boundary before they hit it in production.
pub struct NotifyMetrics {
    /// Histogram of serialized payload sizes in bytes, labelled by channel.
    payload_bytes: Histogram<u64>,
}

impl NotifyMetrics {
    fn new() -> Self {
        let meter = global::meter(METER_NAME);

        let payload_bytes = meter
            .u64_histogram("notify.payload_bytes")
            .with_description(
                "Size of PostgreSQL NOTIFY payloads in bytes. \
                 Payloads approaching 7168 bytes (the NotifyChannel soft limit) \
                 risk hitting the 8000-byte PostgreSQL hard ceiling.",
            )
            .with_unit("By")
            .build();

        Self { payload_bytes }
    }

    pub fn record(&self, channel: &str, bytes: usize) {
        self.payload_bytes.record(
            bytes as u64,
            &[KeyValue::new("channel", channel.to_string())],
        );
    }
}

/// Metrics for the realtime subscription manager.
///
/// These gauges are updated on the cleanup interval (every 60 s by default)
/// so they reflect steady-state cardinality rather than per-event churn.
pub struct SubscriptionMetrics {
    /// Total number of active subscribers across all groups.
    subscribers_total: Gauge<i64>,
    /// Number of unique query groups (deduplicated by query+args+auth).
    groups_total: Gauge<i64>,
    /// Number of tables currently indexed by the subscription manager.
    tables_indexed: Gauge<i64>,
}

impl SubscriptionMetrics {
    fn new() -> Self {
        let meter = global::meter(METER_NAME);

        let subscribers_total = meter
            .i64_gauge("subscriptions.subscribers_total")
            .with_description("Total active SSE subscribers across all query groups")
            .build();

        let groups_total = meter
            .i64_gauge("subscriptions.groups_total")
            .with_description(
                "Number of unique query groups (query+args+auth combinations). \
                 Each group re-executes independently on invalidation.",
            )
            .build();

        let tables_indexed = meter
            .i64_gauge("subscriptions.tables_indexed")
            .with_description(
                "Number of tables currently tracked in the subscription inverted index. \
                 A NOTIFY on an un-indexed table is a no-op for the reactor.",
            )
            .build();

        Self {
            subscribers_total,
            groups_total,
            tables_indexed,
        }
    }

    pub fn record(&self, subscribers: usize, groups: usize, tables: usize) {
        self.subscribers_total.record(subscribers as i64, &[]);
        self.groups_total.record(groups as i64, &[]);
        self.tables_indexed.record(tables as i64, &[]);
    }
}

/// Metrics for the workflow scheduler's `process_ready_workflows` loop.
pub struct WorkflowSchedulerMetrics {
    /// How long each `process_ready_workflows` call takes end-to-end.
    processing_duration: Histogram<f64>,
}

impl WorkflowSchedulerMetrics {
    fn new() -> Self {
        let meter = global::meter(METER_NAME);

        let processing_duration = meter
            .f64_histogram("workflow.scheduler.processing_duration_seconds")
            .with_description(
                "Duration of each workflow scheduler processing cycle (cancel scan, \
                 timer wakeups, event wakeups). Sustained high values indicate the \
                 scheduler is falling behind the poll interval.",
            )
            .with_unit("s")
            .build();

        Self {
            processing_duration,
        }
    }

    pub fn record_processing_duration(&self, duration_secs: f64) {
        self.processing_duration.record(duration_secs, &[]);
    }
}

fn http_metrics() -> &'static HttpMetrics {
    HTTP_METRICS.get_or_init(HttpMetrics::new)
}

fn fn_metrics() -> &'static FnMetrics {
    FN_METRICS.get_or_init(FnMetrics::new)
}

fn fn_cache_metrics() -> &'static FnCacheMetrics {
    FN_CACHE_METRICS.get_or_init(FnCacheMetrics::new)
}

fn job_metrics() -> &'static JobMetrics {
    JOB_METRICS.get_or_init(JobMetrics::new)
}

fn connections_gauge() -> &'static ActiveConnectionsGauge {
    CONNECTIONS_GAUGE.get_or_init(ActiveConnectionsGauge::new)
}

fn notify_metrics() -> &'static NotifyMetrics {
    NOTIFY_METRICS.get_or_init(NotifyMetrics::new)
}

fn subscription_metrics() -> &'static SubscriptionMetrics {
    SUBSCRIPTION_METRICS.get_or_init(SubscriptionMetrics::new)
}

fn workflow_scheduler_metrics() -> &'static WorkflowSchedulerMetrics {
    WORKFLOW_SCHEDULER_METRICS.get_or_init(WorkflowSchedulerMetrics::new)
}

pub fn record_http_request(method: &str, path: &str, status: u16, duration_secs: f64) {
    http_metrics().record(method, path, status, duration_secs);
}

pub fn record_fn_execution(
    function: &str,
    kind: &str,
    success: bool,
    cached: bool,
    duration_secs: f64,
) {
    fn_metrics().record(function, kind, success, cached, duration_secs);
}

pub fn record_fn_cache(function: &str, hit: bool) {
    fn_cache_metrics().record(function, hit);
}

pub fn record_job_execution(job_type: &str, status: &'static str, duration_secs: f64) {
    job_metrics().record(job_type, status, duration_secs);
}

pub fn record_lost_claim(job_type: &str) {
    job_metrics().record_lost_claim(job_type);
}

pub fn set_active_connections(connection_type: &'static str, delta: i64) {
    connections_gauge().set(connection_type, delta);
}

pub fn record_notify_payload_bytes(channel: &str, bytes: usize) {
    notify_metrics().record(channel, bytes);
}

/// Call periodically rather than on every subscribe/unsubscribe to avoid per-event overhead.
pub fn record_subscription_counts(subscribers: usize, groups: usize, tables: usize) {
    subscription_metrics().record(subscribers, groups, tables);
}

pub fn record_workflow_scheduler_duration(duration_secs: f64) {
    workflow_scheduler_metrics().record_processing_duration(duration_secs);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_http_metrics_creation() {
        let _metrics = HttpMetrics::new();
    }

    #[test]
    fn test_job_metrics_creation() {
        let _metrics = JobMetrics::new();
    }

    #[test]
    fn test_connections_gauge_creation() {
        let _gauge = ActiveConnectionsGauge::new();
    }

    #[test]
    fn test_notify_metrics_creation() {
        let _metrics = NotifyMetrics::new();
    }

    #[test]
    fn test_subscription_metrics_creation() {
        let _metrics = SubscriptionMetrics::new();
    }

    #[test]
    fn test_workflow_scheduler_metrics_creation() {
        let _metrics = WorkflowSchedulerMetrics::new();
    }
}

# FORGE Observability System

FORGE includes a built-in observability stack for metrics, logs, and traces. All observability data is stored in PostgreSQL and queryable via the dashboard or REST API.

## Architecture Overview

```
+-------------------+     +------------------+     +-------------------+
|   Application     | --> |   Collectors     | --> |   Storage         |
|   (Metrics/Logs/  |     | (Buffer + Batch) |     | (PostgreSQL)      |
|    Traces)        |     +------------------+     +-------------------+
+-------------------+            |                         |
                                 v                         v
                        +------------------+     +-------------------+
                        | Background Flush |     |   Dashboard API   |
                        | (Configurable)   |     | (REST + Charts)   |
                        +------------------+     +-------------------+
```

## Metrics

### MetricKind

Four metric types are supported:

| Kind | Description | Example |
|------|-------------|---------|
| `Counter` | Monotonically increasing value | `http_requests_total` |
| `Gauge` | Value that can increase or decrease | `active_connections` |
| `Histogram` | Distribution with configurable buckets | `request_duration_seconds` |
| `Summary` | Distribution with quantiles | `response_size_bytes` |

### MetricValue

```rust
pub enum MetricValue {
    Value(f64),
    Histogram { buckets: Vec<(f64, u64)>, count: u64, sum: f64 },
    Summary { quantiles: Vec<(f64, f64)>, count: u64, sum: f64 },
}
```

### Metric Struct

```rust
pub struct Metric {
    pub name: String,
    pub kind: MetricKind,
    pub labels: HashMap<String, String>,
    pub value: MetricValue,
    pub timestamp: DateTime<Utc>,
    pub description: Option<String>,
}
```

Create metrics using the builder pattern:

```rust
use forge_core::observability::Metric;

// Counter
Metric::counter("http_requests_total", 1.0)
    .with_label("method", "GET")
    .with_label("status", "200");

// Gauge
Metric::gauge("active_connections", 42.0);

// Histogram
Metric::histogram("request_duration", buckets, 100, 45.5);
```

### MetricsCollector

Buffers metrics in memory and flushes to storage in batches:

```rust
pub struct MetricsCollector {
    config: MetricsConfig,
    buffer: Arc<RwLock<VecDeque<Metric>>>,
    // ...
}
```

Key methods:

| Method | Description |
|--------|-------------|
| `record(metric)` | Add a metric to the buffer |
| `increment_counter(name, value)` | Convenience for counter metrics |
| `set_gauge(name, value)` | Convenience for gauge metrics |
| `flush()` | Force flush buffer to storage |
| `drain()` | Drain buffer for persistence |
| `run()` | Start background flush loop |

Configuration:

```rust
pub struct MetricsConfig {
    pub interval: Duration,          // Collection interval (default: 10s)
    pub raw_retention: Duration,     // Raw data retention (default: 1h)
    pub downsampled_1m: Duration,    // 1-min aggregate retention (default: 24h)
    pub downsampled_5m: Duration,    // 5-min aggregate retention (default: 7d)
    pub downsampled_1h: Duration,    // 1-hour aggregate retention (default: 90d)
    pub buffer_size: usize,          // Buffer size before auto-flush (default: 10000)
    pub flush_interval: Duration,    // Flush interval (default: 10s)
}
```

### SystemMetricsCollector

Collects system metrics using the `sysinfo` crate:

| Metric | Description |
|--------|-------------|
| `forge_system_cpu_usage_percent` | Overall CPU usage |
| `forge_system_cpu_core_usage_percent` | Per-core CPU usage (label: `core`) |
| `forge_system_memory_total_bytes` | Total memory |
| `forge_system_memory_used_bytes` | Used memory |
| `forge_system_memory_usage_percent` | Memory usage percentage |
| `forge_system_swap_total_bytes` | Total swap |
| `forge_system_swap_used_bytes` | Used swap |
| `forge_system_disk_total_bytes` | Disk total (label: `mount`) |
| `forge_system_disk_used_bytes` | Disk used (label: `mount`) |
| `forge_system_disk_usage_percent` | Disk usage percentage |
| `forge_system_load_1m` | 1-minute load average (Unix only) |
| `forge_system_load_5m` | 5-minute load average (Unix only) |
| `forge_system_load_15m` | 15-minute load average (Unix only) |

---

## Logs

### LogLevel

Ordered log levels with filtering support:

```rust
pub enum LogLevel {
    Trace,  // Most verbose
    Debug,
    Info,   // Default
    Warn,
    Error,  // Least verbose
}
```

Log level ordering: `Trace < Debug < Info < Warn < Error`

### LogEntry

```rust
pub struct LogEntry {
    pub level: LogLevel,
    pub message: String,
    pub target: Option<String>,           // Module path
    pub fields: HashMap<String, serde_json::Value>,
    pub timestamp: DateTime<Utc>,
    pub trace_id: Option<String>,         // Trace correlation
    pub span_id: Option<String>,          // Span correlation
    pub node_id: Option<uuid::Uuid>,      // Source node
}
```

Create log entries using convenience methods:

```rust
use forge_core::observability::LogEntry;

LogEntry::info("Request processed")
    .with_target("forge::gateway")
    .with_field("duration_ms", 42)
    .with_field("status", 200)
    .with_trace_id("abc123");
```

### LogCollector

Buffers and filters logs:

```rust
impl LogCollector {
    pub async fn record(&self, entry: LogEntry);
    pub async fn trace(&self, message: impl Into<String>);
    pub async fn debug(&self, message: impl Into<String>);
    pub async fn info(&self, message: impl Into<String>);
    pub async fn warn(&self, message: impl Into<String>);
    pub async fn error(&self, message: impl Into<String>);
    pub async fn drain(&self) -> Vec<LogEntry>;
}
```

Configuration:

```rust
pub struct LogsConfig {
    pub level: LogLevel,                  // Minimum level (default: Info)
    pub retention: Duration,              // Retention duration (default: 7d)
    pub slow_query_threshold: Duration,   // Slow query logging (default: 100ms)
    pub async_writes: bool,               // Async writes (default: true)
    pub buffer_size: usize,               // Buffer size (default: 5000)
}
```

---

## Traces

### TraceId and SpanId

```rust
pub struct TraceId(String);  // 32-char hex, no dashes
pub struct SpanId(String);   // 16-char hex
```

### SpanContext

Propagation context for distributed tracing:

```rust
pub struct SpanContext {
    pub trace_id: TraceId,
    pub span_id: SpanId,
    pub parent_span_id: Option<SpanId>,
    pub trace_flags: u8,  // 0x01 = sampled
}
```

W3C Trace Context support:

```rust
// Create traceparent header
let header = ctx.to_traceparent();
// "00-{trace_id}-{span_id}-{flags}"

// Parse from header
let ctx = SpanContext::from_traceparent("00-abc123...-def456...-01");
```

### SpanKind

```rust
pub enum SpanKind {
    Internal,   // Default - internal operation
    Server,     // Server handling a request
    Client,     // Client making a request
    Producer,   // Producer sending a message
    Consumer,   // Consumer receiving a message
}
```

### SpanStatus

```rust
pub enum SpanStatus {
    Unset,  // Default
    Ok,     // Completed successfully
    Error,  // Failed with error
}
```

### Span

```rust
pub struct Span {
    pub context: SpanContext,
    pub name: String,
    pub kind: SpanKind,
    pub status: SpanStatus,
    pub status_message: Option<String>,
    pub start_time: DateTime<Utc>,
    pub end_time: Option<DateTime<Utc>>,
    pub attributes: HashMap<String, serde_json::Value>,
    pub events: Vec<SpanEvent>,
    pub node_id: Option<uuid::Uuid>,
}
```

Create and manage spans:

```rust
use forge_core::observability::Span;

// Root span
let mut span = Span::new("http_request")
    .with_kind(SpanKind::Server)
    .with_attribute("http.method", "GET")
    .with_attribute("http.url", "/api/users");

// Child span
let child = span.child("database_query");

// Add events
span.add_event("started processing");

// Complete span
span.end_ok();
// or
span.end_error("Something went wrong");

// Get duration
if let Some(ms) = span.duration_ms() {
    println!("Duration: {}ms", ms);
}
```

### TraceCollector

Samples and buffers traces:

```rust
impl TraceCollector {
    pub async fn record(&self, span: Span);
    pub async fn drain(&self) -> Vec<Span>;
    pub fn sample_rate(&self) -> f64;
}
```

Configuration:

```rust
pub struct TracesConfig {
    pub sample_rate: f64,         // Sample rate 0.0-1.0 (default: 1.0)
    pub retention: Duration,      // Retention duration (default: 24h)
    pub always_trace_errors: bool, // Always sample errors (default: true)
}
```

Sampling behavior:
- If `always_trace_errors` is true, error spans are always recorded
- Otherwise, spans are sampled based on `sample_rate`
- Sampling is deterministic based on trace ID hash

---

## Alerts

### AlertSeverity

```rust
pub enum AlertSeverity {
    Info,      // Informational
    Warning,   // Warning
    Critical,  // Critical/Error
}
```

Ordering: `Info < Warning < Critical`

### AlertStatus

```rust
pub enum AlertStatus {
    Inactive,  // Not triggered
    Pending,   // Condition met, waiting for duration
    Firing,    // Alert is active
    Resolved,  // Alert was resolved
}
```

### AlertCondition

```rust
pub struct AlertCondition {
    pub expression: String,           // e.g., "rate(errors[5m]) > 0.05"
    pub for_duration: Duration,       // Duration before firing
}
```

### AlertState

Tracks alert lifecycle:

```rust
pub struct AlertState {
    pub status: AlertStatus,
    pub pending_since: Option<DateTime<Utc>>,
    pub firing_since: Option<DateTime<Utc>>,
    pub resolved_at: Option<DateTime<Utc>>,
    pub last_evaluation: Option<DateTime<Utc>>,
    pub last_value: Option<f64>,
}
```

### Alert

```rust
pub struct Alert {
    pub name: String,
    pub condition: AlertCondition,
    pub severity: AlertSeverity,
    pub notify: Vec<String>,          // Notification channels
    pub description: Option<String>,
    pub state: AlertState,
}
```

Create alerts:

```rust
use forge_core::observability::{Alert, AlertCondition, AlertSeverity};
use std::time::Duration;

Alert::new(
    "high_error_rate",
    AlertCondition::new("rate(errors[5m]) > 0.05", Duration::from_secs(300)),
    AlertSeverity::Critical,
)
.with_notify("slack:#alerts")
.with_description("Error rate exceeds 5%");
```

---

## Storage

All observability data is persisted to PostgreSQL using batch inserts with the UNNEST pattern for efficiency.

### MetricsStore

```rust
impl MetricsStore {
    pub async fn store(&self, metrics: Vec<Metric>);
    pub async fn query(&self, name: &str, from: DateTime<Utc>, to: DateTime<Utc>) -> Vec<Metric>;
    pub async fn list_latest(&self) -> Vec<Metric>;
    pub async fn cleanup(&self, retention: Duration) -> u64;
}
```

### LogStore

```rust
impl LogStore {
    pub async fn store(&self, logs: Vec<LogEntry>);
    pub async fn query(&self, level: Option<LogLevel>, from: Option<DateTime<Utc>>, to: Option<DateTime<Utc>>, limit: usize) -> Vec<LogEntry>;
    pub async fn search(&self, query: &str, limit: usize) -> Vec<LogEntry>;
    pub async fn cleanup(&self, retention: Duration) -> u64;
}
```

### TraceStore

```rust
impl TraceStore {
    pub async fn store(&self, spans: Vec<Span>);
    pub async fn get_trace(&self, trace_id: &str) -> Vec<Span>;
    pub async fn query(&self, from: DateTime<Utc>, to: DateTime<Utc>, limit: usize) -> Vec<String>;
    pub async fn list_recent(&self, limit: usize) -> Vec<TraceSummary>;
    pub async fn find_errors(&self, limit: usize) -> Vec<String>;
    pub async fn cleanup(&self, retention: Duration) -> u64;
}
```

### Batch Insert Pattern

All stores use PostgreSQL UNNEST for efficient batch inserts:

```sql
INSERT INTO forge_metrics (name, kind, value, labels, timestamp)
SELECT * FROM UNNEST($1::TEXT[], $2::TEXT[], $3::FLOAT8[], $4::JSONB[], $5::TIMESTAMPTZ[])
```

Benefits:
- Single round-trip for entire batch
- Avoids parameter limit issues (batched in chunks of 1000)
- Efficient for high-throughput scenarios

---

## Database Tables

### forge_metrics

```sql
CREATE TABLE forge_metrics (
    id BIGSERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    value FLOAT8 NOT NULL,
    labels JSONB NOT NULL DEFAULT '{}',
    timestamp TIMESTAMPTZ NOT NULL
);
```

### forge_logs

```sql
CREATE TABLE forge_logs (
    id BIGSERIAL PRIMARY KEY,
    level TEXT NOT NULL,
    message TEXT NOT NULL,
    target TEXT,
    fields JSONB NOT NULL DEFAULT '{}',
    trace_id TEXT,
    span_id TEXT,
    timestamp TIMESTAMPTZ NOT NULL
);
```

### forge_traces

```sql
CREATE TABLE forge_traces (
    id UUID PRIMARY KEY,
    trace_id TEXT NOT NULL,
    span_id TEXT NOT NULL,
    parent_span_id TEXT,
    name TEXT NOT NULL,
    kind TEXT NOT NULL,
    status TEXT NOT NULL,
    attributes JSONB NOT NULL DEFAULT '{}',
    events JSONB NOT NULL DEFAULT '[]',
    started_at TIMESTAMPTZ NOT NULL,
    ended_at TIMESTAMPTZ,
    duration_ms INT
);
```

### forge_alerts and forge_alert_rules

```sql
CREATE TABLE forge_alert_rules (
    id UUID PRIMARY KEY,
    name TEXT NOT NULL,
    description TEXT,
    metric_name TEXT NOT NULL,
    condition TEXT NOT NULL,
    threshold FLOAT8 NOT NULL,
    duration_seconds INT NOT NULL DEFAULT 0,
    severity TEXT NOT NULL DEFAULT 'warning',
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    labels JSONB NOT NULL DEFAULT '{}',
    notification_channels JSONB NOT NULL DEFAULT '[]',
    cooldown_seconds INT NOT NULL DEFAULT 300,
    created_at TIMESTAMPTZ NOT NULL,
    updated_at TIMESTAMPTZ NOT NULL
);

CREATE TABLE forge_alerts (
    id UUID PRIMARY KEY,
    rule_id UUID NOT NULL REFERENCES forge_alert_rules(id),
    rule_name TEXT NOT NULL,
    metric_value FLOAT8 NOT NULL,
    threshold FLOAT8 NOT NULL,
    severity TEXT NOT NULL,
    status TEXT NOT NULL,
    triggered_at TIMESTAMPTZ NOT NULL,
    resolved_at TIMESTAMPTZ,
    acknowledged_at TIMESTAMPTZ,
    acknowledged_by TEXT
);
```

---

## Export Configuration

### ExportDestination

```rust
pub enum ExportDestination {
    Postgres,    // Built-in PostgreSQL storage
    Otlp,        // OpenTelemetry Protocol
    Prometheus,  // Prometheus metrics endpoint
}
```

### OTLP Export

```rust
pub struct OtlpConfig {
    pub endpoint: String,              // e.g., "http://localhost:4317"
    pub protocol: String,              // "grpc" or "http/protobuf"
    pub headers: HashMap<String, String>,
    pub metrics: bool,
    pub logs: bool,
    pub traces: bool,
    pub trace_sample_rate: f64,
    pub always_export_errors: bool,
}
```

### Prometheus Export

```rust
pub struct PrometheusConfig {
    pub enabled: bool,
    pub path: String,  // e.g., "/metrics"
}
```

---

## Configuration Example

```toml
[observability]
enabled = true
database_url = "postgres://localhost/forge_observability"  # Optional separate DB
pool_size = 10
pool_timeout = "5s"

[observability.metrics]
interval = "10s"
raw_retention = "1h"
downsampled_1m = "24h"
downsampled_5m = "7d"
downsampled_1h = "90d"
buffer_size = 10000
flush_interval = "10s"

[observability.logs]
level = "info"
retention = "7d"
slow_query_threshold = "100ms"
async_writes = true
buffer_size = 5000

[observability.traces]
sample_rate = 1.0
retention = "24h"
always_trace_errors = true

[observability.export]
destinations = ["postgres"]

[observability.export.otlp]
endpoint = "http://localhost:4317"
protocol = "grpc"
metrics = true
logs = true
traces = true

[observability.export.prometheus]
enabled = true
path = "/metrics"
```

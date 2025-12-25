# Observability

> *You can't fix what you can't see*

---

## Overview

FORGE has **built-in observability**—no Prometheus, Grafana, or Jaeger required. Everything is stored in PostgreSQL and viewable in the embedded dashboard.

The three pillars:
- **Metrics** — Quantitative measurements (latency, throughput, errors)
- **Logs** — Discrete events with context
- **Traces** — Request flow across the system

---

## Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                     OBSERVABILITY ARCHITECTURE                               │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│   Application Code                                                           │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │  #[forge::query]                                                     │   │
│   │  pub async fn get_projects(...) {                                    │   │
│   │      ctx.log.info("Fetching projects", json!({...}));               │   │
│   │      // Metrics, traces auto-collected                               │   │
│   │  }                                                                   │   │
│   └─────────────────────────────────────────────────────────────────────┘   │
│                                       │                                      │
│                                       ▼                                      │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │                    Observability Collector                           │   │
│   │                                                                      │   │
│   │   ┌───────────┐    ┌───────────┐    ┌───────────┐                   │   │
│   │   │  Metrics  │    │   Logs    │    │  Traces   │                   │   │
│   │   │ Collector │    │ Collector │    │ Collector │                   │   │
│   │   └─────┬─────┘    └─────┬─────┘    └─────┬─────┘                   │   │
│   │         │                │                │                          │   │
│   │         └────────────────┼────────────────┘                          │   │
│   │                          │                                           │   │
│   │                    Buffer & Batch                                    │   │
│   │                          │                                           │   │
│   └──────────────────────────┼──────────────────────────────────────────┘   │
│                              │                                               │
│                              ▼                                               │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │                        PostgreSQL                                    │   │
│   │                                                                      │   │
│   │   forge_metrics    forge_logs    forge_traces                        │   │
│   │   (time-series)    (structured)  (spans)                             │   │
│   └─────────────────────────────────────────────────────────────────────┘   │
│                              │                                               │
│                              ▼                                               │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │                     Built-in Dashboard                               │   │
│   │                   http://localhost:8080/_dashboard                   │   │
│   └─────────────────────────────────────────────────────────────────────┘   │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## What's Collected Automatically

### Metrics (No Code Required)

| Metric | Description |
|--------|-------------|
| `forge_http_requests_total` | HTTP requests by method, path, status |
| `forge_http_request_duration_seconds` | Request latency histogram |
| `forge_function_calls_total` | Function calls by name, type |
| `forge_function_duration_seconds` | Function execution time |
| `forge_jobs_dispatched_total` | Jobs dispatched by type |
| `forge_jobs_completed_total` | Jobs completed |
| `forge_jobs_failed_total` | Jobs failed |
| `forge_db_queries_total` | Database queries |
| `forge_db_query_duration_seconds` | Query latency |
| `forge_websocket_connections` | Active WebSocket connections |
| `forge_subscriptions_active` | Active subscriptions |

### Logs (Automatic + Custom)

Automatic logs:
- Request start/end
- Function execution
- Database queries (slow queries)
- Job lifecycle
- Errors and panics

### Traces (Automatic)

Every request gets a trace with spans for:
- HTTP handling
- Authentication
- Function execution
- Database queries
- External HTTP calls
- Job dispatch

---

## Configuration

```toml
# forge.toml

[observability]
# Enable/disable
enabled = true

# IMPORTANT: Separate database for observability (recommended for production)
# This prevents observability writes from competing with user transactions for IOPS.
# If not specified, defaults to the main application database.
database_url = "${OBSERVABILITY_DATABASE_URL}"

# Connection pool for observability database (separate from main pool)
pool_size = 10
pool_timeout = "5s"

[observability.metrics]
# Collection interval
interval = "10s"

# Retention
raw_retention = "1h"      # Full resolution
downsampled_1m = "24h"    # 1-minute aggregates
downsampled_5m = "7d"     # 5-minute aggregates
downsampled_1h = "90d"    # 1-hour aggregates

# Buffer settings to reduce write pressure
buffer_size = 10000       # In-memory buffer before flush
flush_interval = "10s"    # How often to flush to database

[observability.logs]
# Minimum level
level = "info"  # debug, info, warn, error

# Retention
retention = "7d"

# Slow query threshold
slow_query_threshold = "100ms"

# Async logging to prevent blocking
async_writes = true
buffer_size = 5000

[observability.traces]
# Sample rate (1.0 = 100%)
sample_rate = 1.0

# Retention
retention = "24h"

# Always trace errors
always_trace_errors = true
```

### Avoiding the IOPS Death Spiral

**The Problem:** When your app gets busy, observability writes (metrics, logs) compete with user transactions for database IOPS. This slows down the database, which generates more latency alerts, creating a feedback loop that blinds you exactly when you need visibility most.

**The Solution:** Use a separate database for observability data:

```toml
# forge.toml

[database]
url = "postgres://user:pass@primary-db:5432/myapp"

[observability]
# Point observability to a separate database instance
database_url = "postgres://user:pass@observability-db:5432/observability"
```

**Benefits:**
- User transactions and observability writes don't compete for IOPS
- You can use cheaper storage for observability (it's append-heavy, read-infrequent)
- Observability database can be sized independently
- Main database vacuum pressure is reduced

**If you don't specify `database_url`:** FORGE defaults to using the main application database. This is fine for development and small deployments, but for production workloads, a separate database is strongly recommended.

---

## Accessing Observability Data

### Dashboard

```
http://localhost:8080/_dashboard
```

Features:
- Real-time metrics graphs
- Log search and filtering
- Trace exploration
- System health overview
- Alert management

### SQL Queries

```sql
-- Recent error rate
SELECT 
    date_trunc('minute', time) as minute,
    sum(value) FILTER (WHERE labels->>'status' >= '500') / sum(value) as error_rate
FROM forge_metrics
WHERE name = 'forge_http_requests_total'
  AND time > NOW() - INTERVAL '1 hour'
GROUP BY 1
ORDER BY 1;

-- Slow functions
SELECT 
    labels->>'function' as function,
    avg(value) as avg_duration_ms,
    max(value) as max_duration_ms
FROM forge_metrics
WHERE name = 'forge_function_duration_seconds'
  AND time > NOW() - INTERVAL '1 hour'
GROUP BY 1
ORDER BY 2 DESC
LIMIT 10;
```

### API

```bash
# Metrics
curl http://localhost:8080/_api/metrics?name=forge_http_requests_total&period=1h

# Logs
curl http://localhost:8080/_api/logs?level=error&limit=100

# Traces
curl http://localhost:8080/_api/traces/abc123
```

---

## Pluggable Export

FORGE writes to PostgreSQL by default, but can simultaneously export to external systems. This is **additive**—you keep the built-in dashboard AND get data in your existing tools.

### Export Options

```toml
# forge.toml

[observability.export]
# Export destinations (can enable multiple)
destinations = ["postgres", "otlp"]  # Default: ["postgres"]
```

| Destination | Use Case |
|-------------|----------|
| `postgres` | Built-in dashboard, simple setup |
| `otlp` | Datadog, Honeycomb, Grafana Cloud, Jaeger, etc. |
| `prometheus` | Existing Prometheus/Grafana stack |

### OpenTelemetry (OTLP) Export

OTLP is the standard protocol supported by most observability platforms:

```toml
# forge.toml

[observability.export]
destinations = ["postgres", "otlp"]

[observability.export.otlp]
endpoint = "http://otel-collector:4317"
protocol = "grpc"  # or "http/protobuf"

# Optional: headers for authentication
headers = { "api-key" = "${OBSERVABILITY_API_KEY}" }

# What to export
metrics = true
logs = true
traces = true
```

**Direct to vendor:**

```toml
# Datadog
[observability.export.otlp]
endpoint = "https://otel.datadoghq.com:4317"
headers = { "DD-API-KEY" = "${DD_API_KEY}" }

# Honeycomb
[observability.export.otlp]
endpoint = "https://api.honeycomb.io:443"
headers = { "x-honeycomb-team" = "${HONEYCOMB_API_KEY}" }

# Grafana Cloud
[observability.export.otlp]
endpoint = "https://otlp-gateway-prod-us-central-0.grafana.net/otlp"
headers = { "Authorization" = "Basic ${GRAFANA_OTLP_TOKEN}" }
```

### Prometheus Metrics Endpoint

Expose a `/metrics` endpoint for Prometheus to scrape:

```toml
[observability.export]
destinations = ["postgres", "prometheus"]

[observability.export.prometheus]
enabled = true
path = "/metrics"
```

```yaml
# prometheus.yml
scrape_configs:
  - job_name: 'forge'
    static_configs:
      - targets: ['forge:8080']
    metrics_path: '/metrics'
```

### Why Keep PostgreSQL AND Export?

1. **Dashboard always works** — Even if external service is down
2. **Lower latency queries** — Dashboard queries local PostgreSQL
3. **No vendor lock-in** — Switch vendors without losing observability
4. **Cost control** — Store raw data locally, sample what you export

### Selective Export

Don't export everything to external services (expensive!):

```toml
[observability.export.otlp]
endpoint = "https://api.honeycomb.io:443"

# Only export traces (most valuable for debugging)
metrics = false
logs = false
traces = true

# Sample traces to reduce cost
trace_sample_rate = 0.1  # 10% of traces

# Always export errors
always_export_errors = true
```

### Migration Path

Start with PostgreSQL. Add OTLP later when you need it:

```toml
# Day 1: Just PostgreSQL (built-in)
[observability]
enabled = true

# Day 100: Add OTLP export
[observability.export]
destinations = ["postgres", "otlp"]

[observability.export.otlp]
endpoint = "..."
```

No migration needed. Just add config and restart.

---

## Alerting

Built-in alerting without external tools:

```toml
# forge.toml

[[alerts]]
name = "high_error_rate"
condition = "rate(forge_http_requests_total{status=~'5..'}[5m]) / rate(forge_http_requests_total[5m]) > 0.05"
for = "5m"
severity = "critical"
notify = ["slack:#alerts", "pagerduty"]

[[alerts]]
name = "slow_queries"
condition = "avg(forge_db_query_duration_seconds) > 0.5"
for = "10m"
severity = "warning"
notify = ["slack:#ops"]

[[alerts]]
name = "job_queue_backup"
condition = "forge_jobs_pending > 1000"
for = "15m"
severity = "warning"
notify = ["slack:#ops"]

[alerts.notifications.slack]
webhook_url = "${SLACK_WEBHOOK_URL}"

[alerts.notifications.pagerduty]
routing_key = "${PAGERDUTY_KEY}"
```

---

## Data Retention

FORGE automatically manages retention:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                     METRICS RETENTION                                        │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│   Time ──────────────────────────────────────────────────────────────────►  │
│                                                                              │
│   ◄─── 1 hour ───►◄──── 24 hours ────►◄──── 7 days ────►◄─── 90 days ───►  │
│                                                                              │
│   ┌──────────────┐┌──────────────────┐┌────────────────┐┌────────────────┐  │
│   │  Raw (10s)   ││  1-min aggregate ││ 5-min aggregate││1-hour aggregate│  │
│   │              ││                  ││                ││                │  │
│   │  Full detail ││  count, sum,     ││  count, sum,   ││  count, sum,   │  │
│   │              ││  min, max, avg   ││  min, max, avg ││  min, max, avg │  │
│   └──────────────┘└──────────────────┘└────────────────┘└────────────────┘  │
│                                                                              │
│   Automatic downsampling + cleanup via background job                        │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Performance Impact

Observability is designed to be lightweight:

| Operation | Overhead |
|-----------|----------|
| Metric increment | ~100ns (in-memory counter) |
| Log write | ~1μs (buffered) |
| Trace span | ~500ns (sampled) |
| DB flush | Batched every 10s |

Tips for high-throughput:
- Reduce log level to `warn` in production
- Sample traces (e.g., 10% with `sample_rate = 0.1`)
- Increase flush interval

---

## Related Documentation

- [Metrics](METRICS.md) — Detailed metrics guide
- [Logging](LOGGING.md) — Structured logging
- [Tracing](TRACING.md) — Distributed tracing
- [Dashboard](DASHBOARD.md) — Built-in UI

# Metrics

> *Measure everything that matters*

---

## Overview

FORGE collects metrics automatically and stores them in PostgreSQL. No external time-series database required.

---

## Built-in Metrics

### HTTP Metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `forge_http_requests_total` | Counter | method, path, status | Total HTTP requests |
| `forge_http_request_duration_seconds` | Histogram | method, path | Request latency |
| `forge_http_request_size_bytes` | Histogram | method, path | Request body size |
| `forge_http_response_size_bytes` | Histogram | method, path | Response body size |

### Function Metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `forge_function_calls_total` | Counter | name, type, status | Function invocations |
| `forge_function_duration_seconds` | Histogram | name, type | Execution time |
| `forge_function_errors_total` | Counter | name, type, error | Errors by type |

### Database Metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `forge_db_queries_total` | Counter | operation | Query count |
| `forge_db_query_duration_seconds` | Histogram | operation | Query latency |
| `forge_db_connections_active` | Gauge | | Active connections |
| `forge_db_connections_idle` | Gauge | | Idle connections |

### Job Metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `forge_jobs_dispatched_total` | Counter | type | Jobs created |
| `forge_jobs_completed_total` | Counter | type | Jobs finished |
| `forge_jobs_failed_total` | Counter | type | Jobs failed |
| `forge_jobs_duration_seconds` | Histogram | type | Job execution time |
| `forge_jobs_pending` | Gauge | capability | Queue depth |
| `forge_jobs_retry_total` | Counter | type | Retry count |

### Cluster Metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `forge_cluster_nodes_total` | Gauge | status | Node count |
| `forge_cluster_leader` | Gauge | role | 1 if this node is leader |
| `forge_mesh_rpc_duration_seconds` | Histogram | peer, method | Inter-node latency |

### WebSocket Metrics

| Metric | Type | Labels | Description |
|--------|------|--------|-------------|
| `forge_websocket_connections` | Gauge | | Active connections |
| `forge_subscriptions_active` | Gauge | | Active subscriptions |
| `forge_subscription_updates_total` | Counter | query | Updates sent |

---

## Custom Metrics

Add your own metrics in function code:

```rust
use forge::metrics::{counter, gauge, histogram};

#[forge::mutation]
pub async fn process_order(ctx: &MutationContext, input: OrderInput) -> Result<Order> {
    // Increment counter
    counter!("orders_processed_total", 1, "type" => input.order_type);
    
    // Set gauge
    gauge!("order_value_dollars", input.total.to_f64());
    
    // Record histogram
    let start = Instant::now();
    let result = do_processing(&input).await?;
    histogram!("order_processing_seconds", start.elapsed().as_secs_f64());
    
    Ok(result)
}
```

### Metric Types

```rust
// Counter: Only goes up
counter!("events_total", 1);
counter!("bytes_processed", bytes.len() as u64);

// Gauge: Can go up or down
gauge!("queue_depth", queue.len() as f64);
gauge!("temperature_celsius", sensor.read());

// Histogram: Distribution of values
histogram!("request_duration_seconds", duration.as_secs_f64());
histogram!("response_size_bytes", response.len() as f64);
```

### Labels

```rust
// Add labels for dimensions
counter!("http_requests", 1, 
    "method" => "POST",
    "path" => "/api/orders",
    "status" => "200"
);

// Dynamic labels
counter!("user_actions", 1,
    "action" => action_type,
    "user_tier" => user.tier.to_string()
);
```

---

## Storage

Metrics are stored in PostgreSQL with automatic downsampling:

```sql
-- Raw metrics (high resolution, short retention)
CREATE TABLE forge_metrics (
    time TIMESTAMPTZ NOT NULL,
    name VARCHAR(255) NOT NULL,
    labels JSONB NOT NULL DEFAULT '{}',
    value DOUBLE PRECISION NOT NULL,
    node_id UUID
) PARTITION BY RANGE (time);

-- 1-minute aggregates
CREATE TABLE forge_metrics_1m (
    time TIMESTAMPTZ NOT NULL,
    name VARCHAR(255) NOT NULL,
    labels JSONB NOT NULL DEFAULT '{}',
    count INTEGER NOT NULL,
    sum DOUBLE PRECISION NOT NULL,
    min DOUBLE PRECISION NOT NULL,
    max DOUBLE PRECISION NOT NULL,
    PRIMARY KEY (time, name, labels)
);
```

### Downsampling Process

```
┌─────────────────────────────────────────────────────────────────┐
│                    DOWNSAMPLING                                  │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│   Raw Data (10s resolution)                                      │
│   ┌─────────────────────────────────────────────────────────┐   │
│   │ 10:00:00  value=1.2                                      │   │
│   │ 10:00:10  value=1.5                                      │   │
│   │ 10:00:20  value=1.3                                      │   │
│   │ 10:00:30  value=1.8                                      │   │
│   │ 10:00:40  value=1.1                                      │   │
│   │ 10:00:50  value=1.4                                      │   │
│   └─────────────────────────────────────────────────────────┘   │
│                              │                                   │
│                              ▼ Aggregate to 1-minute             │
│   ┌─────────────────────────────────────────────────────────┐   │
│   │ 10:00:00  count=6, sum=8.3, min=1.1, max=1.8, avg=1.38  │   │
│   └─────────────────────────────────────────────────────────┘   │
│                              │                                   │
│                              ▼ Delete raw data older than 1h     │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## Querying Metrics

### Dashboard

The built-in dashboard provides:
- Pre-built graphs for common metrics
- Custom query builder
- Time range selection
- Auto-refresh

### SQL

```sql
-- Request rate over time
SELECT 
    date_trunc('minute', time) as minute,
    sum(value) as requests
FROM forge_metrics
WHERE name = 'forge_http_requests_total'
  AND time > NOW() - INTERVAL '1 hour'
GROUP BY 1
ORDER BY 1;

-- P95 latency by endpoint
SELECT 
    labels->>'path' as path,
    percentile_cont(0.95) WITHIN GROUP (ORDER BY value) as p95
FROM forge_metrics
WHERE name = 'forge_http_request_duration_seconds'
  AND time > NOW() - INTERVAL '1 hour'
GROUP BY 1
ORDER BY 2 DESC;

-- Error rate
SELECT 
    sum(value) FILTER (WHERE labels->>'status' >= '500') / 
    NULLIF(sum(value), 0) as error_rate
FROM forge_metrics
WHERE name = 'forge_http_requests_total'
  AND time > NOW() - INTERVAL '5 minutes';
```

### API

```bash
# Get metric values
curl "http://localhost:8080/_api/metrics?name=forge_http_requests_total&period=1h"

# Aggregated
curl "http://localhost:8080/_api/metrics/aggregate?name=forge_function_duration_seconds&agg=p95&period=24h"
```

---

## Prometheus Export

If you need Prometheus compatibility:

```toml
# forge.toml
[observability.export]
prometheus_enabled = true
prometheus_path = "/metrics"
```

```bash
curl http://localhost:8080/metrics

# Output:
# HELP forge_http_requests_total Total HTTP requests
# TYPE forge_http_requests_total counter
# forge_http_requests_total{method="GET",path="/api/users",status="200"} 1523
# forge_http_requests_total{method="POST",path="/api/orders",status="201"} 892
```

---

## Related Documentation

- [Observability](OBSERVABILITY.md) — Overview
- [Dashboard](DASHBOARD.md) — Viewing metrics
- [Logging](LOGGING.md) — Structured logs

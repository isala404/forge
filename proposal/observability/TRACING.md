# Distributed Tracing

> *Follow requests across the system*

---

## Overview

FORGE automatically traces requests as they flow through the system—across nodes, functions, database queries, and external services.

---

## Trace Structure

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                     TRACE: abc-123-def-456                                   │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │ Span: http.request                                    [0ms - 150ms] │    │
│  │ Service: gateway                                                     │    │
│  │ Tags: {method: POST, path: /api/orders, status: 201}                │    │
│  │                                                                      │    │
│  │  ┌─────────────────────────────────────────────────────────────┐    │    │
│  │  │ Span: auth.verify                             [5ms - 15ms]  │    │    │
│  │  │ Tags: {user_id: user-123}                                   │    │    │
│  │  └─────────────────────────────────────────────────────────────┘    │    │
│  │                                                                      │    │
│  │  ┌─────────────────────────────────────────────────────────────┐    │    │
│  │  │ Span: function.execute (create_order)        [20ms - 140ms] │    │    │
│  │  │ Service: function-executor                                   │    │    │
│  │  │                                                              │    │    │
│  │  │  ┌─────────────────────────────────────────────────────┐    │    │    │
│  │  │  │ Span: db.query                       [25ms - 30ms]  │    │    │    │
│  │  │  │ Tags: {query: "SELECT * FROM users..."}             │    │    │    │
│  │  │  └─────────────────────────────────────────────────────┘    │    │    │
│  │  │                                                              │    │    │
│  │  │  ┌─────────────────────────────────────────────────────┐    │    │    │
│  │  │  │ Span: http.client (stripe)           [35ms - 100ms] │    │    │    │
│  │  │  │ Tags: {url: "api.stripe.com/charges"}               │    │    │    │
│  │  │  └─────────────────────────────────────────────────────┘    │    │    │
│  │  │                                                              │    │    │
│  │  │  ┌─────────────────────────────────────────────────────┐    │    │    │
│  │  │  │ Span: db.query                      [105ms - 120ms] │    │    │    │
│  │  │  │ Tags: {query: "INSERT INTO orders..."}              │    │    │    │
│  │  │  └─────────────────────────────────────────────────────┘    │    │    │
│  │  │                                                              │    │    │
│  │  │  ┌─────────────────────────────────────────────────────┐    │    │    │
│  │  │  │ Span: job.dispatch                  [125ms - 128ms] │    │    │    │
│  │  │  │ Tags: {job_type: "send_confirmation"}               │    │    │    │
│  │  │  └─────────────────────────────────────────────────────┘    │    │    │
│  │  │                                                              │    │    │
│  │  └─────────────────────────────────────────────────────────────┘    │    │
│  │                                                                      │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Automatic Tracing

FORGE traces these operations automatically:

| Operation | Span Name | Tags |
|-----------|-----------|------|
| HTTP request | `http.request` | method, path, status |
| Function call | `function.execute` | name, type |
| Database query | `db.query` | operation, table |
| External HTTP | `http.client` | url, method, status |
| Job dispatch | `job.dispatch` | job_type |
| Job execution | `job.execute` | job_type, job_id |
| gRPC call | `grpc.call` | service, method |

---

## Custom Spans

Add custom spans for detailed tracing:

```rust
use forge::tracing::{span, Instrument};

#[forge::action]
pub async fn complex_operation(ctx: &ActionContext, input: Input) -> Result<Output> {
    // Create a custom span
    let span = span!("process_items");
    
    async {
        for item in input.items {
            // Nested span
            let item_span = span!("process_item", item_id = %item.id);
            
            async {
                validate_item(&item).await?;
                transform_item(&item).await?;
                save_item(&item).await?;
                Ok(())
            }
            .instrument(item_span)
            .await?;
        }
        Ok(Output { ... })
    }
    .instrument(span)
    .await
}
```

### Adding Tags to Current Span

```rust
use forge::tracing::current_span;

#[forge::mutation]
pub async fn create_order(ctx: &MutationContext, input: OrderInput) -> Result<Order> {
    // Add tags to current span
    current_span().record("order_total", input.total);
    current_span().record("item_count", input.items.len());
    
    // ... rest of function
}
```

---

## Trace Context Propagation

### Cross-Node Propagation

Traces automatically propagate across nodes via gRPC metadata:

```
Node A (Gateway)              Node B (Function Executor)
┌─────────────┐              ┌─────────────┐
│ Span: http  │              │             │
│             │              │             │
│  trace_id ─────────────────► trace_id   │
│  span_id  ─────────────────► parent_id  │
│             │   gRPC       │             │
│             │              │ Span: func  │
└─────────────┘              └─────────────┘
```

### External Service Propagation

FORGE's HTTP client propagates trace context:

```rust
#[forge::action]
pub async fn call_external_api(ctx: &ActionContext) -> Result<Response> {
    // Trace context automatically added to headers:
    // traceparent: 00-abc123-def456-01
    // tracestate: forge=...
    
    let response = ctx.http
        .get("https://api.external.com/data")
        .send()
        .await?;
    
    Ok(response)
}
```

---

## Trace Storage

```sql
CREATE TABLE forge_traces (
    trace_id VARCHAR(32) NOT NULL,
    span_id VARCHAR(16) NOT NULL,
    parent_span_id VARCHAR(16),
    
    operation_name VARCHAR(255) NOT NULL,
    service_name VARCHAR(100) NOT NULL,
    node_id UUID,
    
    start_time TIMESTAMPTZ NOT NULL,
    duration_ms INTEGER NOT NULL,
    
    status VARCHAR(20),  -- ok, error
    status_message TEXT,
    
    tags JSONB DEFAULT '{}',
    logs JSONB DEFAULT '[]',
    
    PRIMARY KEY (trace_id, span_id)
) PARTITION BY RANGE (start_time);
```

---

## Viewing Traces

### Dashboard

The trace viewer shows:
- Timeline visualization
- Span hierarchy
- Tag inspection
- Log correlation
- Error highlighting

### SQL

```sql
-- Get all spans for a trace
SELECT 
    span_id,
    parent_span_id,
    operation_name,
    start_time,
    duration_ms,
    status,
    tags
FROM forge_traces
WHERE trace_id = 'abc-123'
ORDER BY start_time;

-- Find slow traces
SELECT 
    trace_id,
    max(duration_ms) as total_duration
FROM forge_traces
WHERE start_time > NOW() - INTERVAL '1 hour'
  AND operation_name = 'http.request'
GROUP BY trace_id
HAVING max(duration_ms) > 1000
ORDER BY total_duration DESC;

-- Trace with errors
SELECT DISTINCT trace_id
FROM forge_traces
WHERE status = 'error'
  AND start_time > NOW() - INTERVAL '1 hour';
```

### API

```bash
# Get trace
curl "http://localhost:8080/_api/traces/abc-123"

# Search traces
curl "http://localhost:8080/_api/traces?operation=create_order&min_duration=1000&limit=50"
```

---

## Sampling

For high-traffic systems, sample traces:

```toml
# forge.toml

[observability.traces]
# Sample rate (0.0 - 1.0)
sample_rate = 0.1  # 10% of requests

# Always trace these
always_sample = [
    "errors",        # All errors
    "slow_requests", # Requests > 1s
]

# Slow request threshold
slow_threshold = "1s"
```

---

## Export to External Systems

```toml
# forge.toml

[observability.export]
# Jaeger
jaeger_endpoint = "http://jaeger:14268/api/traces"

# Zipkin
zipkin_endpoint = "http://zipkin:9411/api/v2/spans"

# OTLP (OpenTelemetry)
otlp_endpoint = "http://otel-collector:4317"
otlp_protocol = "grpc"
```

---

## Related Documentation

- [Observability](OBSERVABILITY.md) — Overview
- [Data Flow](../architecture/DATA_FLOW.md) — Request flow
- [Meshing](../cluster/MESHING.md) — Cross-node communication

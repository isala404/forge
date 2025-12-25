# Logging

> *Structured logs that tell the story*

---

## Overview

FORGE provides structured JSON logging with automatic context enrichment. Logs are stored in PostgreSQL and queryable via SQL.

---

## Log Levels

| Level | Use Case |
|-------|----------|
| `error` | Failures requiring attention |
| `warn` | Unexpected but handled situations |
| `info` | Important business events |
| `debug` | Detailed debugging info |
| `trace` | Very verbose, development only |

---

## Logging in Functions

```rust
use forge::prelude::*;

#[forge::mutation]
pub async fn process_payment(ctx: &MutationContext, input: PaymentInput) -> Result<Payment> {
    ctx.log.info("Processing payment", json!({
        "user_id": input.user_id,
        "amount": input.amount,
        "currency": input.currency,
    }));
    
    let result = stripe::charge(&input).await;
    
    match &result {
        Ok(charge) => {
            ctx.log.info("Payment successful", json!({
                "charge_id": charge.id,
                "amount": charge.amount,
            }));
        }
        Err(e) => {
            ctx.log.error("Payment failed", json!({
                "error": e.to_string(),
                "error_code": e.code(),
            }));
        }
    }
    
    result
}
```

---

## Automatic Context

Every log entry automatically includes:

```json
{
    "timestamp": "2024-01-15T10:30:00.123Z",
    "level": "info",
    "message": "Processing payment",
    
    // Automatic context
    "node_id": "abc-123",
    "trace_id": "def-456",
    "span_id": "ghi-789",
    "function_name": "process_payment",
    "function_type": "mutation",
    "user_id": "user-123",
    
    // Your fields
    "fields": {
        "amount": 99.99,
        "currency": "USD"
    }
}
```

---

## Log Storage

```sql
CREATE TABLE forge_logs (
    id BIGSERIAL,
    timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    level VARCHAR(20) NOT NULL,
    message TEXT NOT NULL,
    
    -- Automatic context
    node_id UUID,
    trace_id VARCHAR(32),
    span_id VARCHAR(16),
    function_name VARCHAR(255),
    function_type VARCHAR(50),
    user_id UUID,
    
    -- Custom fields
    fields JSONB DEFAULT '{}'
) PARTITION BY RANGE (timestamp);

-- Indexes for common queries
CREATE INDEX idx_logs_timestamp ON forge_logs(timestamp DESC);
CREATE INDEX idx_logs_level ON forge_logs(level, timestamp DESC);
CREATE INDEX idx_logs_trace ON forge_logs(trace_id) WHERE trace_id IS NOT NULL;
CREATE INDEX idx_logs_user ON forge_logs(user_id) WHERE user_id IS NOT NULL;
CREATE INDEX idx_logs_fields ON forge_logs USING GIN(fields);
```

---

## Querying Logs

### Dashboard

Full-text search with filters:
- Time range
- Log level
- Function name
- User ID
- Trace ID
- Custom field values

### SQL

```sql
-- Recent errors
SELECT timestamp, message, fields
FROM forge_logs
WHERE level = 'error'
  AND timestamp > NOW() - INTERVAL '1 hour'
ORDER BY timestamp DESC
LIMIT 100;

-- Logs for a specific trace
SELECT timestamp, level, message, function_name
FROM forge_logs
WHERE trace_id = 'abc-123'
ORDER BY timestamp;

-- Search in custom fields
SELECT *
FROM forge_logs
WHERE fields->>'order_id' = 'order-456';

-- Full-text search
SELECT *
FROM forge_logs
WHERE message ILIKE '%payment%'
  AND timestamp > NOW() - INTERVAL '24 hours'
ORDER BY timestamp DESC;
```

### API

```bash
# Recent errors
curl "http://localhost:8080/_api/logs?level=error&limit=100"

# By trace
curl "http://localhost:8080/_api/logs?trace_id=abc-123"

# Search
curl "http://localhost:8080/_api/logs?search=payment&period=24h"
```

---

## Configuration

```toml
# forge.toml

[observability.logs]
# Minimum level
level = "info"

# Also log to stdout (useful for containers)
stdout = true
stdout_format = "json"  # or "pretty"

# Retention
retention = "7d"

# Slow query logging
slow_query_threshold = "100ms"
log_query_params = false  # Security: don't log query parameters
```

---

## Sensitive Data

Avoid logging sensitive information:

```rust
// ❌ Bad: Logs sensitive data
ctx.log.info("User login", json!({
    "email": user.email,
    "password": input.password,  // NEVER log passwords!
}));

// ✅ Good: Redact sensitive fields
ctx.log.info("User login", json!({
    "user_id": user.id,
    "email_domain": user.email.split('@').last(),
}));
```

### Automatic Redaction

```toml
# forge.toml

[observability.logs.redaction]
# Fields to redact (replaced with "[REDACTED]")
fields = ["password", "api_key", "secret", "token", "ssn", "credit_card"]

# Patterns to redact
patterns = [
    "\\b\\d{4}[- ]?\\d{4}[- ]?\\d{4}[- ]?\\d{4}\\b",  # Credit card
    "\\b\\d{3}-\\d{2}-\\d{4}\\b",  # SSN
]
```

---

## Log Aggregation Export

Export to external log systems:

```toml
# forge.toml

[observability.export]
# Loki
loki_url = "http://loki:3100/loki/api/v1/push"

# Elasticsearch
elasticsearch_url = "http://elasticsearch:9200"
elasticsearch_index = "forge-logs"

# Generic HTTP (for Datadog, etc.)
http_endpoint = "https://logs.example.com/v1/logs"
http_headers = { "DD-API-KEY" = "${DATADOG_API_KEY}" }
```

---

## Related Documentation

- [Observability](OBSERVABILITY.md) — Overview
- [Tracing](TRACING.md) — Distributed tracing
- [Security](../reference/SECURITY.md) — Data protection

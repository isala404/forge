# Dashboard

> *See everything, from anywhere*

---

## Overview

FORGE includes a built-in observability dashboard—no Grafana or external tools needed. Access it at:

```
http://localhost:8080/_dashboard
```

---

## Features

### System Overview

- **Cluster health**: Node status, roles, connections
- **Request metrics**: Throughput, latency, error rate
- **Resource usage**: CPU, memory, connections

### Metrics Explorer

- Pre-built dashboards for common metrics
- Custom query builder
- Time range selection
- Auto-refresh
- Graph visualizations

### Log Viewer

- Real-time log streaming
- Full-text search
- Filter by level, function, user
- Trace correlation

### Trace Explorer

- Search traces by operation, duration, status
- Timeline visualization
- Span hierarchy
- Tag inspection

### Job Monitor

- Queue depth by capability
- Job throughput
- Failed jobs and dead letter queue
- Retry status

### Cron Status

- Next run times
- Execution history
- Success/failure tracking

---

## Screenshots

```
┌─────────────────────────────────────────────────────────────────────────────┐
│  FORGE Dashboard                                           [cluster: prod]  │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  ┌─────────────────────┐ ┌─────────────────────┐ ┌─────────────────────┐   │
│  │ Requests/sec        │ │ Error Rate          │ │ P95 Latency         │   │
│  │                     │ │                     │ │                     │   │
│  │      ▂▅▇█▅▃▄▆      │ │        ▁▁           │ │      ▄▅▃▂▄▆▄      │   │
│  │                     │ │                     │ │                     │   │
│  │      1,234 req/s    │ │       0.02%         │ │        45ms         │   │
│  └─────────────────────┘ └─────────────────────┘ └─────────────────────┘   │
│                                                                              │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │  Cluster Nodes                                                       │   │
│  │                                                                      │   │
│  │  ● forge-1 (leader)    G F W S   Load: 45%   Uptime: 3d 14h        │   │
│  │  ● forge-2             G F W     Load: 62%   Uptime: 3d 14h        │   │
│  │  ● forge-3             W (media) Load: 28%   Uptime: 1d 2h         │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                              │
│  ┌─────────────────────────────────────────────────────────────────────┐   │
│  │  Recent Logs                                              [Live ●]  │   │
│  │                                                                      │   │
│  │  10:30:01 INFO  create_order    Order created: ord_123             │   │
│  │  10:30:02 INFO  process_payment Payment successful: $99.00         │   │
│  │  10:30:03 WARN  sync_inventory  Stock low: SKU-456 (qty: 5)        │   │
│  │  10:30:05 ERROR send_email      SMTP timeout                        │   │
│  └─────────────────────────────────────────────────────────────────────┘   │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Configuration

```toml
# forge.toml

[dashboard]
# Enable/disable dashboard
enabled = true

# Custom path (default: /_dashboard)
path = "/_dashboard"

# Require authentication
require_auth = true

# Allowed roles (if auth enabled)
allowed_roles = ["admin", "developer"]

# Custom branding
title = "My App Dashboard"
logo_url = "/static/logo.png"
```

---

## Authentication

### Development Mode

```toml
[dashboard]
require_auth = false  # Open access in dev
```

### Production

```toml
[dashboard]
require_auth = true

# Use same auth as your app
auth_provider = "app"  # Uses your app's auth

# Or basic auth
auth_provider = "basic"
basic_auth_user = "${DASHBOARD_USER}"
basic_auth_password = "${DASHBOARD_PASSWORD}"

# Or OAuth
auth_provider = "oauth"
oauth_provider = "google"
oauth_allowed_domains = ["mycompany.com"]
```

---

## API Access

The dashboard has an API for programmatic access:

```bash
# Metrics
GET /_api/metrics
GET /_api/metrics?name=forge_http_requests_total&period=1h

# Logs
GET /_api/logs
GET /_api/logs?level=error&limit=100
GET /_api/logs?trace_id=abc-123

# Traces
GET /_api/traces/{trace_id}
GET /_api/traces?operation=create_order&min_duration=1000

# Cluster
GET /_api/cluster/nodes
GET /_api/cluster/health

# Jobs
GET /_api/jobs/queue
GET /_api/jobs/dead-letter
POST /_api/jobs/{job_id}/retry
```

---

## Custom Dashboards

Create custom dashboard pages:

```rust
// Register custom dashboard section
forge::dashboard::register(DashboardSection {
    name: "Business Metrics",
    path: "/business",
    component: include_str!("../dashboard/business.svelte"),
});
```

```svelte
<!-- dashboard/business.svelte -->
<script>
  import { query } from '$lib/forge/dashboard';
  
  const revenue = query('SELECT sum(amount) FROM orders WHERE date > now() - interval "7 days"');
  const orders = query('SELECT count(*) FROM orders WHERE date > now() - interval "24 hours"');
</script>

<div class="grid grid-cols-2 gap-4">
  <MetricCard title="7-Day Revenue" value={$revenue} format="currency" />
  <MetricCard title="Orders Today" value={$orders} />
</div>
```

---

## Related Documentation

- [Observability](OBSERVABILITY.md) — Overview
- [Metrics](METRICS.md) — Metrics reference
- [Logging](LOGGING.md) — Log access
- [Tracing](TRACING.md) — Trace explorer

# FORGE Dashboard

FORGE includes a built-in web dashboard for monitoring and managing your application. The dashboard provides real-time visibility into metrics, logs, traces, jobs, workflows, crons, and cluster status.

## Accessing the Dashboard

The dashboard is available at `/_dashboard/` on your FORGE server:

```
http://localhost:3000/_dashboard/
```

The REST API is available at `/_api/`:

```
http://localhost:3000/_api/
```

## Configuration

```rust
pub struct DashboardConfig {
    pub enabled: bool,              // Enable dashboard (default: true)
    pub path_prefix: String,        // Dashboard path (default: "/_dashboard")
    pub api_prefix: String,         // API path (default: "/_api")
    pub require_auth: bool,         // Require authentication (default: false)
    pub admin_users: Vec<String>,   // Allowed admin user IDs
}
```

---

## Dashboard Pages

### Overview (`/_dashboard/`)

The main dashboard showing system health at a glance:

**Stats Cards:**
- Requests per second
- P99 latency (milliseconds)
- Error rate (percentage)
- Active connections

**Charts:**
- Request rate over time
- Response time distribution

**Panels:**
- Active alerts
- Recent logs
- Cluster nodes

### Metrics (`/_dashboard/metrics`)

Browse and search all collected metrics:

**Features:**
- Search metrics by name
- Filter by type (counter, gauge, histogram)
- Click any metric to view detail chart
- Time-series visualization with Chart.js

**Controls:**
- Metric search input
- Type filter dropdown
- Time range selector

### Logs (`/_dashboard/logs`)

View and search application logs:

**Columns:**
- Time
- Level (trace, debug, info, warn, error)
- Message
- Trace ID (for correlation)

**Features:**
- Full-text search
- Level filtering
- Live streaming mode (SSE-based)
- Pagination

### Traces (`/_dashboard/traces`)

Distributed tracing view:

**List View Columns:**
- Trace ID
- Root span name
- Service
- Duration
- Span count
- Status (ok/error)
- Start time

**Features:**
- Search by trace ID, service, or operation
- Filter by minimum duration
- Show errors only toggle

### Trace Detail (`/_dashboard/traces/{trace_id}`)

Detailed view of a single trace:

**Components:**
- Trace header with ID and summary
- Waterfall visualization with timeline ruler
- Span tree showing parent-child relationships
- Span details panel (on selection)
- Tabs for attributes, events, and logs

**Waterfall Features:**
- Timeline visualization
- Span duration bars
- Parent-child relationships
- Click to select span

### Alerts (`/_dashboard/alerts`)

Manage alerts and alert rules:

**Summary Stats:**
- Critical count
- Warning count
- Info count

**Tabs:**
- Active Alerts - currently firing
- Alert History - past alerts
- Alert Rules - configured rules

**Alert Actions:**
- Acknowledge alert
- Resolve alert
- Create/edit/delete rules

### Jobs (`/_dashboard/jobs`)

Monitor background job queue:

**Stats:**
- Pending jobs
- Running jobs
- Completed jobs
- Failed jobs

**Tabs:**
- Queue - pending jobs
- Running - in-progress jobs
- History - completed jobs
- Dead Letter - failed after max retries

**Job List Columns:**
- Job ID
- Type
- Priority
- Status
- Progress (percentage and message)
- Attempts
- Created time
- Error message

**Job Detail Modal:**
- Full progress bar
- Input/output JSON
- Error details
- Timing information

### Workflows (`/_dashboard/workflows`)

Monitor workflow executions:

**Stats:**
- Running workflows
- Completed workflows
- Waiting workflows
- Failed workflows

**Workflow List Columns:**
- Run ID
- Workflow name
- Version
- Status
- Current step
- Started time
- Error message

**Workflow Detail Modal:**
- Step-by-step progress
- Status icons for each step
- Step timing
- Input/output data
- Error details

### Crons (`/_dashboard/crons`)

Manage scheduled tasks:

**Stats:**
- Active crons
- Paused crons
- Success rate (24h)
- Next scheduled run

**Cron List Columns:**
- Name
- Schedule (cron expression)
- Status
- Last run
- Last result
- Next run
- Average duration
- Actions (trigger, pause, resume)

**Additional Panels:**
- Recent executions chart
- Execution history table

### Cluster (`/_dashboard/cluster`)

View cluster health and node status:

**Health Indicator:**
- Status (healthy/degraded/unhealthy)
- Node count
- Leader information

**Node Cards:**
- Node ID
- Hostname
- Roles
- Status
- Last heartbeat
- Version
- Start time

**Leadership Table:**
- Role (scheduler, metrics_aggregator, log_compactor)
- Leader node ID

---

## REST API Endpoints

All API responses follow this format:

```json
{
  "success": true,
  "data": { ... },
  "error": null
}
```

### Metrics API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/_api/metrics` | GET | List all metrics with latest values |
| `/_api/metrics/{name}` | GET | Get specific metric |
| `/_api/metrics/series` | GET | Get time-series data for charts |

Query parameters:
- `start` - Start time (ISO 8601)
- `end` - End time (ISO 8601)
- `period` - Period shorthand: `1h`, `24h`, `7d`, `30d`

### Logs API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/_api/logs` | GET | List recent logs |
| `/_api/logs/search` | GET | Search logs by message |

Query parameters:
- `level` - Log level filter
- `q` - Search query
- `start`, `end` - Time range
- `limit` - Max results (default: 100, max: 1000)

### Traces API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/_api/traces` | GET | List recent traces |
| `/_api/traces/{trace_id}` | GET | Get trace with all spans |

Query parameters:
- `service` - Service filter
- `operation` - Operation filter
- `min_duration` - Minimum duration in ms
- `errors_only` - Boolean, show only errors
- `start`, `end` - Time range
- `limit` - Max results

### Alerts API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/_api/alerts` | GET | List all alerts |
| `/_api/alerts/active` | GET | Get currently firing alerts |
| `/_api/alerts/{id}/acknowledge` | POST | Acknowledge an alert |
| `/_api/alerts/{id}/resolve` | POST | Resolve an alert |
| `/_api/alerts/rules` | GET | List alert rules |
| `/_api/alerts/rules` | POST | Create alert rule |
| `/_api/alerts/rules/{id}` | GET | Get alert rule |
| `/_api/alerts/rules/{id}` | PUT | Update alert rule |
| `/_api/alerts/rules/{id}` | DELETE | Delete alert rule |

### Jobs API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/_api/jobs` | GET | List jobs (paginated) |
| `/_api/jobs/stats` | GET | Get job statistics |
| `/_api/jobs/registered` | GET | List registered job types |
| `/_api/jobs/{id}` | GET | Get job details |
| `/_api/jobs/{job_type}/dispatch` | POST | Dispatch a job |

Job dispatch request:
```json
{
  "args": { ... }
}
```

Job dispatch response:
```json
{
  "success": true,
  "data": {
    "job_id": "uuid"
  }
}
```

### Workflows API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/_api/workflows` | GET | List workflow runs |
| `/_api/workflows/stats` | GET | Get workflow statistics |
| `/_api/workflows/registered` | GET | List registered workflows |
| `/_api/workflows/{id}` | GET | Get workflow with steps |
| `/_api/workflows/{workflow_name}/start` | POST | Start a workflow |

Workflow start request:
```json
{
  "input": { ... }
}
```

Workflow start response:
```json
{
  "success": true,
  "data": {
    "workflow_id": "uuid"
  }
}
```

### Crons API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/_api/crons` | GET | List all crons |
| `/_api/crons/stats` | GET | Get cron statistics |
| `/_api/crons/history` | GET | Get execution history |
| `/_api/crons/registered` | GET | List registered crons |
| `/_api/crons/{name}/trigger` | POST | Manually trigger a cron |
| `/_api/crons/{name}/pause` | POST | Pause a cron |
| `/_api/crons/{name}/resume` | POST | Resume a cron |

### Cluster API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/_api/cluster/nodes` | GET | List cluster nodes |
| `/_api/cluster/health` | GET | Get cluster health status |

### System API

| Endpoint | Method | Description |
|----------|--------|-------------|
| `/_api/system/info` | GET | Get system information |
| `/_api/system/stats` | GET | Get system statistics |

System info response:
```json
{
  "version": "0.1.0",
  "rust_version": "1.85",
  "started_at": "2024-01-01T00:00:00Z",
  "uptime_seconds": 3600
}
```

System stats response:
```json
{
  "http_requests_total": 1000,
  "http_requests_per_second": 10.5,
  "p99_latency_ms": 42.0,
  "function_calls_total": 500,
  "active_connections": 5,
  "active_subscriptions": 10,
  "jobs_pending": 3,
  "memory_used_mb": 256,
  "cpu_usage_percent": 15.5
}
```

---

## Response Types

### MetricSummary

```json
{
  "name": "http_requests_total",
  "kind": "counter",
  "description": null,
  "current_value": 1234.0,
  "labels": { "method": "GET" },
  "last_updated": "2024-01-01T12:00:00Z"
}
```

### LogEntry

```json
{
  "id": "123",
  "timestamp": "2024-01-01T12:00:00Z",
  "level": "info",
  "message": "Request processed",
  "fields": { "duration_ms": 42 },
  "trace_id": "abc123",
  "span_id": "def456"
}
```

### TraceSummary

```json
{
  "trace_id": "abc123",
  "root_span_name": "HTTP GET /api/users",
  "service": "api-gateway",
  "duration_ms": 150,
  "span_count": 5,
  "error": false,
  "started_at": "2024-01-01T12:00:00Z"
}
```

### TraceDetail

```json
{
  "trace_id": "abc123",
  "spans": [
    {
      "span_id": "span1",
      "parent_span_id": null,
      "name": "HTTP GET /api/users",
      "service": "api-gateway",
      "kind": "server",
      "status": "ok",
      "start_time": "2024-01-01T12:00:00Z",
      "end_time": "2024-01-01T12:00:00.150Z",
      "duration_ms": 150,
      "attributes": { "http.method": "GET" },
      "events": []
    }
  ]
}
```

### JobDetail

```json
{
  "id": "uuid",
  "job_type": "send_email",
  "status": "running",
  "priority": 50,
  "attempts": 1,
  "max_attempts": 3,
  "progress_percent": 50,
  "progress_message": "Sending to recipients...",
  "input": { "to": "user@example.com" },
  "output": null,
  "scheduled_at": "2024-01-01T12:00:00Z",
  "created_at": "2024-01-01T12:00:00Z",
  "started_at": "2024-01-01T12:00:01Z",
  "completed_at": null,
  "last_error": null
}
```

### WorkflowDetail

```json
{
  "id": "uuid",
  "workflow_name": "user_onboarding",
  "version": "1",
  "status": "running",
  "input": { "user_id": "123" },
  "output": null,
  "current_step": "send_welcome_email",
  "steps": [
    {
      "name": "create_profile",
      "status": "completed",
      "result": { "profile_id": "456" },
      "started_at": "2024-01-01T12:00:00Z",
      "completed_at": "2024-01-01T12:00:01Z",
      "error": null
    },
    {
      "name": "send_welcome_email",
      "status": "running",
      "result": null,
      "started_at": "2024-01-01T12:00:01Z",
      "completed_at": null,
      "error": null
    }
  ],
  "started_at": "2024-01-01T12:00:00Z",
  "completed_at": null,
  "error": null
}
```

### JobStats

```json
{
  "pending": 10,
  "running": 3,
  "completed": 1000,
  "failed": 5,
  "retrying": 2,
  "dead_letter": 1
}
```

### WorkflowStats

```json
{
  "running": 5,
  "completed": 500,
  "waiting": 2,
  "failed": 3,
  "compensating": 0
}
```

### ClusterHealth

```json
{
  "status": "healthy",
  "node_count": 3,
  "healthy_nodes": 3,
  "leader_node": "uuid",
  "leaders": {
    "scheduler": "uuid",
    "metrics_aggregator": "uuid"
  }
}
```

---

## Static Assets

The dashboard serves static assets from `/_dashboard/assets/`:

| Asset | Path | Description |
|-------|------|-------------|
| CSS | `/_dashboard/assets/styles.css` | Dark theme, responsive layout |
| Main JS | `/_dashboard/assets/main.js` | Page interactivity, data fetching |
| Chart.js | `/_dashboard/assets/chart.js` | CDN loader with fallback |

### Chart.js Integration

Charts are loaded from CDN with automatic fallback:

```javascript
// Primary CDN
<script src="https://cdn.jsdelivr.net/npm/chart.js@4.4.0"></script>

// Fallback for offline/firewall environments
<script>
  if (typeof Chart === 'undefined') {
    // Load from alternative source
  }
</script>
```

### Auto-Refresh

All dashboard pages auto-refresh every 5 seconds when visible:

```javascript
setInterval(loadPageSpecificData, 5000);
```

Pages load data from the REST API and update the DOM dynamically.

---

## Pagination

List endpoints support pagination:

| Parameter | Type | Default | Description |
|-----------|------|---------|-------------|
| `page` | int | 1 | Page number (1-indexed) |
| `limit` | int | 50 | Items per page (max: 1000) |

Example:
```
GET /_api/jobs?page=2&limit=20
```

---

## Time Ranges

Time-based queries support:

**Explicit range:**
- `start` - ISO 8601 datetime
- `end` - ISO 8601 datetime

**Period shorthand:**
- `period=1h` - Last hour (default)
- `period=24h` - Last 24 hours
- `period=7d` - Last 7 days
- `period=30d` - Last 30 days

The UI time range selector offers:
- Last 5 minutes
- Last 15 minutes
- Last hour
- Last 6 hours
- Last 24 hours
- Last 7 days

---

## Real-Time Features

### Job/Workflow Progress

The dashboard supports real-time progress tracking:

1. Click a job or workflow row to open detail modal
2. Modal shows live progress bar
3. Updates via periodic polling (500ms for in-progress items)

### Live Log Streaming

The logs page supports SSE-based live streaming:

1. Click "Live Stream" button
2. Logs appear in real-time as they're generated
3. Click "Stop Stream" to pause

### WebSocket Subscriptions

For programmatic real-time updates, use WebSocket subscriptions:

```javascript
// Subscribe to job updates
ws.send(JSON.stringify({
  type: 'subscribe_job',
  job_id: 'uuid',
  client_sub_id: 'my-sub-1'
}));

// Subscribe to workflow updates
ws.send(JSON.stringify({
  type: 'subscribe_workflow',
  workflow_id: 'uuid',
  client_sub_id: 'my-sub-2'
}));
```

---

## Security

### Authentication

When `require_auth` is enabled:

1. Dashboard checks for valid JWT token
2. User ID must be in `admin_users` list
3. Unauthenticated requests redirect to login

### CORS

The API router includes permissive CORS for development:

```rust
CorsLayer::new()
    .allow_origin(Any)
    .allow_methods(Any)
    .allow_headers(Any)
```

Configure stricter CORS for production environments.

# PostgreSQL Schema

This document describes all `forge_*` system tables created by FORGE. These tables are automatically managed by the FORGE runtime and should not be modified directly.

---

## Overview

FORGE uses PostgreSQL as its sole database and coordination layer. All system state is stored in tables prefixed with `forge_`:

- **Cluster** - Node registry and leader election
- **Jobs** - Background job queue with priority and retry
- **Cron** - Scheduled task execution history
- **Workflows** - Durable multi-step workflow state
- **Realtime** - WebSocket sessions and subscriptions
- **Observability** - Metrics, logs, and distributed traces
- **Alerts** - Alert rules and active alerts

---

## Cluster Tables

### forge_nodes

Tracks all nodes in the FORGE cluster. Each node registers itself on startup and sends periodic heartbeats.

```sql
CREATE TABLE forge_nodes (
    id UUID PRIMARY KEY,
    hostname VARCHAR(255) NOT NULL,
    ip_address VARCHAR(64) NOT NULL,
    http_port INTEGER NOT NULL,
    grpc_port INTEGER NOT NULL,
    roles TEXT[] NOT NULL DEFAULT '{}',
    worker_capabilities TEXT[] NOT NULL DEFAULT '{}',
    status VARCHAR(32) NOT NULL DEFAULT 'starting',
    version VARCHAR(64),
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_heartbeat TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

| Column | Description |
|--------|-------------|
| `id` | Unique node identifier (UUID) |
| `hostname` | Machine hostname |
| `ip_address` | Node's IP address |
| `http_port` | HTTP gateway port |
| `grpc_port` | gRPC mesh port |
| `roles` | Array of roles: `gateway`, `function`, `worker`, `scheduler` |
| `worker_capabilities` | Job capabilities this worker can handle |
| `status` | Node status: `starting`, `active`, `draining`, `dead` |
| `version` | FORGE version running on this node |
| `started_at` | When the node started |
| `last_heartbeat` | Last heartbeat timestamp (used for dead node detection) |

### forge_leaders

Tracks leader election state for exclusive roles. Uses lease-based leadership with expiration.

```sql
CREATE TABLE forge_leaders (
    role VARCHAR(64) PRIMARY KEY,
    node_id UUID NOT NULL,
    acquired_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    lease_until TIMESTAMPTZ NOT NULL
);
```

| Column | Description |
|--------|-------------|
| `role` | Leader role name (e.g., `scheduler`, `metrics_aggregator`, `log_compactor`) |
| `node_id` | UUID of the node holding leadership |
| `acquired_at` | When leadership was acquired |
| `lease_until` | Lease expiration time (must be refreshed before expiry) |

Leadership is acquired using PostgreSQL advisory locks (`pg_try_advisory_lock`).

---

## Job Queue Tables

### forge_jobs

The background job queue. Uses PostgreSQL's `SKIP LOCKED` pattern for atomic job claiming.

```sql
CREATE TABLE forge_jobs (
    id UUID PRIMARY KEY,
    job_type VARCHAR(255) NOT NULL,
    input JSONB NOT NULL DEFAULT '{}',
    output JSONB,
    status VARCHAR(32) NOT NULL DEFAULT 'pending',
    priority INTEGER NOT NULL DEFAULT 50,
    attempts INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 3,
    last_error TEXT,
    progress_percent INTEGER DEFAULT 0,
    progress_message TEXT,
    worker_capability VARCHAR(255),
    worker_id UUID,
    idempotency_key VARCHAR(255),
    scheduled_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    claimed_at TIMESTAMPTZ,
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    failed_at TIMESTAMPTZ,
    last_heartbeat TIMESTAMPTZ
);
```

| Column | Description |
|--------|-------------|
| `id` | Unique job identifier |
| `job_type` | Job handler name (e.g., `ExportUsersJob`) |
| `input` | Job arguments as JSON |
| `output` | Job result as JSON (set on completion) |
| `status` | Job status (see below) |
| `priority` | Priority 0-100 (higher = more urgent). Default 50 |
| `attempts` | Number of execution attempts |
| `max_attempts` | Maximum retry attempts before dead letter |
| `last_error` | Error message from last failed attempt |
| `progress_percent` | Job progress 0-100 (for UI display) |
| `progress_message` | Current progress message |
| `worker_capability` | Required worker capability (for routing) |
| `worker_id` | UUID of worker currently processing this job |
| `idempotency_key` | Optional key for preventing duplicate jobs |
| `scheduled_at` | When the job should be picked up |
| `created_at` | When the job was created |
| `claimed_at` | When a worker claimed the job |
| `started_at` | When execution started |
| `completed_at` | When the job completed successfully |
| `failed_at` | When the job failed permanently |
| `last_heartbeat` | Last heartbeat from worker (for stale job detection) |

**Job Status Values:**
- `pending` - Waiting to be claimed
- `claimed` - Claimed by a worker, about to start
- `running` - Currently executing
- `completed` - Finished successfully
- `retry` - Failed, will be retried
- `failed` - Failed permanently (exhausted retries)
- `dead_letter` - Moved to dead letter queue

**Indexes:**
```sql
CREATE INDEX idx_forge_jobs_status_scheduled
    ON forge_jobs(status, scheduled_at)
    WHERE status = 'pending';

CREATE INDEX idx_forge_jobs_idempotency
    ON forge_jobs(idempotency_key)
    WHERE idempotency_key IS NOT NULL;
```

---

## Cron Tables

### forge_cron_runs

Tracks execution history for scheduled cron tasks. Uses a unique constraint on `(cron_name, scheduled_time)` to prevent duplicate runs.

```sql
CREATE TABLE forge_cron_runs (
    id UUID PRIMARY KEY,
    cron_name VARCHAR(255) NOT NULL,
    scheduled_time TIMESTAMPTZ NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'pending',
    node_id UUID,
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    error TEXT,
    UNIQUE(cron_name, scheduled_time)
);
```

| Column | Description |
|--------|-------------|
| `id` | Unique run identifier |
| `cron_name` | Name of the cron handler |
| `scheduled_time` | The scheduled execution time |
| `status` | Run status: `pending`, `running`, `completed`, `failed` |
| `node_id` | UUID of node that executed this run |
| `started_at` | When execution started |
| `completed_at` | When execution completed |
| `error` | Error message if failed |

**Index:**
```sql
CREATE INDEX idx_forge_cron_runs_name_time
    ON forge_cron_runs(cron_name, scheduled_time DESC);
```

The unique constraint ensures exactly-once execution even with multiple scheduler nodes.

---

## Workflow Tables

### forge_workflow_runs

Tracks durable workflow execution state. Workflows can span multiple steps, survive restarts, and support compensation (rollback).

```sql
CREATE TABLE forge_workflow_runs (
    id UUID PRIMARY KEY,
    workflow_name VARCHAR(255) NOT NULL,
    version VARCHAR(64),
    input JSONB NOT NULL DEFAULT '{}',
    output JSONB,
    status VARCHAR(32) NOT NULL DEFAULT 'created',
    current_step VARCHAR(255),
    step_results JSONB DEFAULT '{}',
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    error TEXT,
    trace_id VARCHAR(64)
);
```

| Column | Description |
|--------|-------------|
| `id` | Unique workflow run identifier |
| `workflow_name` | Workflow handler name |
| `version` | Workflow version (for versioned deployments) |
| `input` | Workflow input as JSON |
| `output` | Final workflow output as JSON |
| `status` | Workflow status (see below) |
| `current_step` | Name of currently executing step |
| `step_results` | Map of step name to result JSON |
| `started_at` | When the workflow started |
| `completed_at` | When the workflow completed |
| `error` | Error message if failed |
| `trace_id` | Distributed tracing trace ID |

**Workflow Status Values:**
- `created` - Workflow created, not yet started
- `running` - Currently executing steps
- `waiting` - Waiting for external event or timer
- `completed` - All steps completed successfully
- `compensating` - Running compensation handlers (rollback)
- `compensated` - Compensation completed
- `failed` - Failed without compensation

**Index:**
```sql
CREATE INDEX idx_forge_workflow_runs_status
    ON forge_workflow_runs(status);
```

### forge_workflow_steps

Tracks individual step state within a workflow run.

```sql
CREATE TABLE forge_workflow_steps (
    id UUID PRIMARY KEY,
    workflow_run_id UUID NOT NULL REFERENCES forge_workflow_runs(id) ON DELETE CASCADE,
    step_name VARCHAR(255) NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'pending',
    input JSONB,
    result JSONB,
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    error TEXT,
    UNIQUE(workflow_run_id, step_name)
);
```

| Column | Description |
|--------|-------------|
| `id` | Unique step identifier |
| `workflow_run_id` | Parent workflow run (foreign key) |
| `step_name` | Step name within the workflow |
| `status` | Step status: `pending`, `running`, `completed`, `failed`, `compensated`, `skipped` |
| `input` | Step input as JSON |
| `result` | Step result as JSON |
| `started_at` | When step execution started |
| `completed_at` | When step completed |
| `error` | Error message if failed |

Steps are cascaded on workflow deletion.

---

## Realtime Tables

### forge_sessions

Tracks active WebSocket connections for real-time subscriptions.

```sql
CREATE TABLE forge_sessions (
    id UUID PRIMARY KEY,
    node_id UUID NOT NULL,
    user_id VARCHAR(255),
    connected_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_activity TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    status VARCHAR(32) NOT NULL DEFAULT 'connected'
);
```

| Column | Description |
|--------|-------------|
| `id` | Session identifier |
| `node_id` | Node handling this WebSocket connection |
| `user_id` | Authenticated user ID (if any) |
| `connected_at` | When the connection was established |
| `last_activity` | Last message received from client |
| `status` | Session status: `connecting`, `connected`, `reconnecting`, `disconnected` |

**Index:**
```sql
CREATE INDEX idx_forge_sessions_node
    ON forge_sessions(node_id);
```

### forge_subscriptions

Tracks active query subscriptions for real-time updates.

```sql
CREATE TABLE forge_subscriptions (
    id UUID PRIMARY KEY,
    session_id UUID NOT NULL REFERENCES forge_sessions(id) ON DELETE CASCADE,
    query_name VARCHAR(255) NOT NULL,
    query_hash VARCHAR(64) NOT NULL,
    args JSONB DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

| Column | Description |
|--------|-------------|
| `id` | Subscription identifier |
| `session_id` | Parent session (foreign key) |
| `query_name` | Name of the subscribed query function |
| `query_hash` | Hash of query + args for deduplication |
| `args` | Query arguments as JSON |
| `created_at` | When the subscription was created |

**Indexes:**
```sql
CREATE INDEX idx_forge_subscriptions_session
    ON forge_subscriptions(session_id);

CREATE INDEX idx_forge_subscriptions_query_hash
    ON forge_subscriptions(query_hash);
```

---

## Observability Tables

### forge_metrics

Stores time-series metrics collected from the application.

```sql
CREATE TABLE forge_metrics (
    id BIGSERIAL PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    kind VARCHAR(32) NOT NULL,
    value DOUBLE PRECISION NOT NULL,
    labels JSONB DEFAULT '{}',
    timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

| Column | Description |
|--------|-------------|
| `id` | Auto-incrementing ID |
| `name` | Metric name (e.g., `http_requests_total`) |
| `kind` | Metric type: `counter`, `gauge`, `histogram`, `summary` |
| `value` | Metric value |
| `labels` | Dimension labels as JSON |
| `timestamp` | When the metric was recorded |

**Index:**
```sql
CREATE INDEX idx_forge_metrics_name_time
    ON forge_metrics(name, timestamp DESC);
```

### forge_logs

Stores structured log entries.

```sql
CREATE TABLE forge_logs (
    id BIGSERIAL PRIMARY KEY,
    level VARCHAR(16) NOT NULL,
    message TEXT NOT NULL,
    target VARCHAR(255),
    fields JSONB DEFAULT '{}',
    trace_id VARCHAR(64),
    span_id VARCHAR(32),
    timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

| Column | Description |
|--------|-------------|
| `id` | Auto-incrementing ID |
| `level` | Log level: `trace`, `debug`, `info`, `warn`, `error` |
| `message` | Log message text |
| `target` | Logger target/module name |
| `fields` | Structured fields as JSON |
| `trace_id` | Associated trace ID (for correlation) |
| `span_id` | Associated span ID |
| `timestamp` | When the log was recorded |

**Indexes:**
```sql
CREATE INDEX idx_forge_logs_level_time
    ON forge_logs(level, timestamp DESC);

CREATE INDEX idx_forge_logs_trace_id
    ON forge_logs(trace_id)
    WHERE trace_id IS NOT NULL;
```

### forge_traces

Stores distributed trace spans for request tracing.

```sql
CREATE TABLE forge_traces (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    trace_id VARCHAR(64) NOT NULL,
    span_id VARCHAR(32) NOT NULL,
    parent_span_id VARCHAR(32),
    name VARCHAR(255) NOT NULL,
    kind VARCHAR(32) NOT NULL DEFAULT 'internal',
    status VARCHAR(32) NOT NULL DEFAULT 'unset',
    attributes JSONB DEFAULT '{}',
    events JSONB DEFAULT '[]',
    started_at TIMESTAMPTZ NOT NULL,
    ended_at TIMESTAMPTZ,
    duration_ms INTEGER
);
```

| Column | Description |
|--------|-------------|
| `id` | Row identifier |
| `trace_id` | Trace ID (groups related spans) |
| `span_id` | Unique span identifier |
| `parent_span_id` | Parent span ID (for hierarchy) |
| `name` | Span/operation name |
| `kind` | Span kind: `internal`, `server`, `client`, `producer`, `consumer` |
| `status` | Span status: `unset`, `ok`, `error` |
| `attributes` | Span attributes as JSON |
| `events` | Span events as JSON array |
| `started_at` | When the span started |
| `ended_at` | When the span ended |
| `duration_ms` | Duration in milliseconds |

**Indexes:**
```sql
CREATE INDEX idx_forge_traces_trace_id
    ON forge_traces(trace_id);

CREATE INDEX idx_forge_traces_started_at
    ON forge_traces(started_at DESC);
```

---

## Alert Tables

### forge_alert_rules

Defines alert rules that trigger when metrics cross thresholds.

```sql
CREATE TABLE forge_alert_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(255) NOT NULL UNIQUE,
    description TEXT,
    metric_name VARCHAR(255) NOT NULL,
    condition VARCHAR(32) NOT NULL,
    threshold DOUBLE PRECISION NOT NULL,
    duration_seconds INTEGER NOT NULL DEFAULT 0,
    severity VARCHAR(32) NOT NULL DEFAULT 'warning',
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    labels JSONB DEFAULT '{}',
    notification_channels TEXT[] DEFAULT '{}',
    cooldown_seconds INTEGER NOT NULL DEFAULT 300,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

| Column | Description |
|--------|-------------|
| `id` | Rule identifier |
| `name` | Unique rule name |
| `description` | Human-readable description |
| `metric_name` | Metric to monitor |
| `condition` | Comparison: `gt`, `gte`, `lt`, `lte`, `eq`, `ne` |
| `threshold` | Threshold value |
| `duration_seconds` | Condition must be true for this duration |
| `severity` | Alert severity: `info`, `warning`, `critical` |
| `enabled` | Whether the rule is active |
| `labels` | Labels to match on the metric |
| `notification_channels` | Channels to notify: `email`, `slack`, `webhook` |
| `cooldown_seconds` | Wait time before re-alerting |
| `created_at` | When the rule was created |
| `updated_at` | When the rule was last modified |

### forge_alerts

Tracks active and resolved alerts.

```sql
CREATE TABLE forge_alerts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    rule_id UUID NOT NULL REFERENCES forge_alert_rules(id) ON DELETE CASCADE,
    rule_name VARCHAR(255) NOT NULL,
    metric_value DOUBLE PRECISION NOT NULL,
    threshold DOUBLE PRECISION NOT NULL,
    severity VARCHAR(32) NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'firing',
    triggered_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    resolved_at TIMESTAMPTZ,
    acknowledged_at TIMESTAMPTZ,
    acknowledged_by VARCHAR(255),
    labels JSONB DEFAULT '{}',
    annotations JSONB DEFAULT '{}'
);
```

| Column | Description |
|--------|-------------|
| `id` | Alert instance identifier |
| `rule_id` | Source alert rule (foreign key) |
| `rule_name` | Rule name (denormalized for queries) |
| `metric_value` | Metric value that triggered the alert |
| `threshold` | Threshold that was crossed |
| `severity` | Alert severity |
| `status` | Alert status: `firing`, `resolved` |
| `triggered_at` | When the alert fired |
| `resolved_at` | When the alert was resolved |
| `acknowledged_at` | When someone acknowledged the alert |
| `acknowledged_by` | Who acknowledged the alert |
| `labels` | Context labels |
| `annotations` | Additional annotations |

**Indexes:**
```sql
CREATE INDEX idx_forge_alert_rules_enabled
    ON forge_alert_rules(enabled)
    WHERE enabled = TRUE;

CREATE INDEX idx_forge_alerts_status
    ON forge_alerts(status)
    WHERE status = 'firing';

CREATE INDEX idx_forge_alerts_rule_id
    ON forge_alerts(rule_id);

CREATE INDEX idx_forge_alerts_triggered_at
    ON forge_alerts(triggered_at DESC);
```

---

## Migration Tracking Table

### forge_migrations

Tracks which migrations have been applied to the database.

```sql
CREATE TABLE forge_migrations (
    id SERIAL PRIMARY KEY,
    name VARCHAR(255) UNIQUE NOT NULL,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    down_sql TEXT
);
```

| Column | Description |
|--------|-------------|
| `id` | Auto-incrementing ID |
| `name` | Migration name (e.g., `0001_create_users`) |
| `applied_at` | When the migration was applied |
| `down_sql` | Rollback SQL (stored for down migrations) |

---

## Reactivity Functions

FORGE creates helper functions for enabling real-time reactivity on user tables.

### forge_notify_change()

Trigger function that sends `NOTIFY` on table changes.

```sql
CREATE OR REPLACE FUNCTION forge_notify_change() RETURNS TRIGGER AS $$
DECLARE
    row_id TEXT;
    payload TEXT;
BEGIN
    IF TG_OP = 'DELETE' THEN
        row_id := COALESCE(OLD.id::TEXT, '');
    ELSE
        row_id := COALESCE(NEW.id::TEXT, '');
    END IF;

    payload := TG_TABLE_NAME || ':' || TG_OP || ':' || row_id;
    PERFORM pg_notify('forge_changes', payload);

    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    ELSE
        RETURN NEW;
    END IF;
END;
$$ LANGUAGE plpgsql;
```

Payload format: `table_name:operation:row_id` (e.g., `users:INSERT:abc-123`)

### forge_enable_reactivity()

Enables real-time updates for a table.

```sql
SELECT forge_enable_reactivity('users');
```

Creates a trigger that sends `NOTIFY` on INSERT, UPDATE, and DELETE operations.

### forge_disable_reactivity()

Disables real-time updates for a table.

```sql
SELECT forge_disable_reactivity('users');
```

Removes the notification trigger from the table.

---

## Default Reactivity

The following system tables have reactivity enabled by default:

- `forge_jobs` - For real-time job progress tracking
- `forge_workflow_runs` - For real-time workflow status updates
- `forge_workflow_steps` - For real-time step progress updates

User tables must explicitly enable reactivity:

```sql
-- In your migration
SELECT forge_enable_reactivity('your_table');
```

---

## Related Documentation

- [MIGRATIONS.md](MIGRATIONS.md) - Migration system documentation

# PostgreSQL Schema

> *The complete database schema*

---

## Overview

FORGE stores everything in PostgreSQL:

- **Application data** — Your models
- **Job queue** — Background jobs
- **Cluster state** — Node registry, leaders
- **Observability** — Metrics, logs, traces
- **Sessions** — WebSocket connections, subscriptions

---

## System Tables

### Node Registry

```sql
-- Cluster membership
CREATE TABLE forge_nodes (
    id UUID PRIMARY KEY,
    hostname VARCHAR(255) NOT NULL,
    ip_address INET NOT NULL,
    grpc_port INTEGER NOT NULL DEFAULT 9000,
    http_port INTEGER NOT NULL DEFAULT 8080,
    
    -- Capabilities
    roles TEXT[] NOT NULL DEFAULT ARRAY['gateway', 'function', 'worker', 'scheduler'],
    worker_capabilities TEXT[] DEFAULT ARRAY['general'],
    
    -- Status
    status VARCHAR(50) NOT NULL DEFAULT 'joining',
    last_heartbeat TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    
    -- Load metrics
    current_connections INTEGER DEFAULT 0,
    current_jobs INTEGER DEFAULT 0,
    max_concurrent_jobs INTEGER DEFAULT 100,
    cpu_usage FLOAT DEFAULT 0,
    memory_usage FLOAT DEFAULT 0,
    
    -- Metadata
    version VARCHAR(50),
    started_at TIMESTAMPTZ DEFAULT NOW(),
    
    CONSTRAINT valid_status CHECK (status IN ('joining', 'active', 'draining', 'dead'))
);

CREATE INDEX idx_forge_nodes_status ON forge_nodes(status);
CREATE INDEX idx_forge_nodes_heartbeat ON forge_nodes(last_heartbeat);
CREATE INDEX idx_forge_nodes_capabilities ON forge_nodes USING GIN(worker_capabilities);
```

### Leader Election

```sql
-- Leader roles
CREATE TABLE forge_leaders (
    role VARCHAR(100) PRIMARY KEY,
    node_id UUID REFERENCES forge_nodes(id) ON DELETE SET NULL,
    acquired_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    lease_until TIMESTAMPTZ NOT NULL,
    
    CONSTRAINT valid_lease CHECK (lease_until > acquired_at)
);

CREATE INDEX idx_forge_leaders_node ON forge_leaders(node_id);
CREATE INDEX idx_forge_leaders_lease ON forge_leaders(lease_until);
```

---

## Job Queue Tables

### Jobs

```sql
CREATE TABLE forge_jobs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    
    -- Job definition
    job_type VARCHAR(255) NOT NULL,
    input JSONB NOT NULL,
    
    -- Routing
    worker_capability VARCHAR(100) DEFAULT 'general',
    priority INTEGER DEFAULT 0,  -- Higher = more urgent
    
    -- Status
    status VARCHAR(50) NOT NULL DEFAULT 'pending',
    worker_id UUID REFERENCES forge_nodes(id) ON DELETE SET NULL,
    
    -- Retry logic
    attempts INTEGER DEFAULT 0,
    max_retries INTEGER DEFAULT 3,
    last_error TEXT,
    
    -- Timing
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    scheduled_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    claimed_at TIMESTAMPTZ,
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    failed_at TIMESTAMPTZ,
    
    -- Timeout
    timeout_seconds INTEGER DEFAULT 3600,
    
    -- Output
    output JSONB,
    
    -- Tracing
    trace_id VARCHAR(32),
    parent_job_id UUID REFERENCES forge_jobs(id),
    
    -- Idempotency
    idempotency_key VARCHAR(255),
    
    CONSTRAINT valid_status CHECK (status IN (
        'pending', 'claimed', 'running', 'completed', 
        'failed', 'retry', 'dead_letter', 'cancelled'
    ))
);

-- Critical: Index for efficient job claiming
CREATE INDEX idx_forge_jobs_claimable ON forge_jobs(
    priority DESC, 
    scheduled_at ASC
) WHERE status = 'pending';

-- For capability-based routing
CREATE INDEX idx_forge_jobs_capability ON forge_jobs(worker_capability, status);

-- For worker's jobs
CREATE INDEX idx_forge_jobs_worker ON forge_jobs(worker_id) WHERE worker_id IS NOT NULL;

-- For idempotency checks
CREATE UNIQUE INDEX idx_forge_jobs_idempotency ON forge_jobs(idempotency_key) 
    WHERE idempotency_key IS NOT NULL;

-- For job hierarchy (parent-child)
CREATE INDEX idx_forge_jobs_parent ON forge_jobs(parent_job_id);

-- For cleanup
CREATE INDEX idx_forge_jobs_completed ON forge_jobs(completed_at) 
    WHERE status = 'completed';
```

### Cron Runs

```sql
CREATE TABLE forge_cron_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    cron_name VARCHAR(255) NOT NULL,
    
    -- Scheduling
    scheduled_time TIMESTAMPTZ NOT NULL,
    timezone VARCHAR(100) DEFAULT 'UTC',
    
    -- Execution
    status VARCHAR(50) NOT NULL DEFAULT 'pending',
    node_id UUID REFERENCES forge_nodes(id),
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    
    -- Result
    error TEXT,
    
    -- Prevent duplicate runs
    UNIQUE(cron_name, scheduled_time)
);

CREATE INDEX idx_forge_cron_runs_pending ON forge_cron_runs(scheduled_time) 
    WHERE status = 'pending';
CREATE INDEX idx_forge_cron_runs_name ON forge_cron_runs(cron_name, scheduled_time DESC);
```

### Workflow State

```sql
CREATE TABLE forge_workflow_runs (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workflow_name VARCHAR(255) NOT NULL,
    
    -- Input/Output
    input JSONB NOT NULL,
    output JSONB,
    
    -- Status
    status VARCHAR(50) NOT NULL DEFAULT 'running',
    current_step VARCHAR(255),
    
    -- Step results (for resume)
    step_results JSONB DEFAULT '{}',
    
    -- Timing
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    
    -- Error handling
    error TEXT,
    compensation_status VARCHAR(50),
    
    -- Tracing
    trace_id VARCHAR(32),
    
    CONSTRAINT valid_status CHECK (status IN (
        'running', 'waiting', 'completed', 'failed', 
        'compensating', 'compensated'
    ))
);

CREATE INDEX idx_forge_workflows_status ON forge_workflow_runs(status);
CREATE INDEX idx_forge_workflows_waiting ON forge_workflow_runs(id) WHERE status = 'waiting';

-- Workflow steps
CREATE TABLE forge_workflow_steps (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    workflow_run_id UUID NOT NULL REFERENCES forge_workflow_runs(id) ON DELETE CASCADE,
    
    step_name VARCHAR(255) NOT NULL,
    status VARCHAR(50) NOT NULL DEFAULT 'pending',
    
    -- Result
    result JSONB,
    error TEXT,
    
    -- Timing
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    
    UNIQUE(workflow_run_id, step_name)
);

CREATE INDEX idx_forge_workflow_steps_run ON forge_workflow_steps(workflow_run_id);
```

---

## Change Tracking Tables

### Event Log

```sql
CREATE TABLE forge_events (
    id BIGSERIAL PRIMARY KEY,
    
    -- What changed
    table_name VARCHAR(255) NOT NULL,
    operation VARCHAR(10) NOT NULL,  -- INSERT, UPDATE, DELETE
    row_id UUID,  -- If applicable
    
    -- Change data
    old_data JSONB,
    new_data JSONB,
    changed_columns TEXT[],
    
    -- Context
    transaction_id BIGINT DEFAULT txid_current(),
    timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    
    -- Optional: who made the change
    user_id UUID,
    function_name VARCHAR(255)
);

-- Time-based partitioning for efficient cleanup
-- (Implemented via pg_partman or manual partitioning)

CREATE INDEX idx_forge_events_table ON forge_events(table_name, timestamp DESC);
CREATE INDEX idx_forge_events_timestamp ON forge_events(timestamp DESC);
```

### Change Notification Function

```sql
-- Trigger function for change notifications
CREATE OR REPLACE FUNCTION forge_notify_change() RETURNS TRIGGER AS $$
DECLARE
    payload JSONB;
BEGIN
    payload = jsonb_build_object(
        'table', TG_TABLE_NAME,
        'op', TG_OP,
        'timestamp', NOW()
    );
    
    IF TG_OP = 'DELETE' THEN
        payload = payload || jsonb_build_object('id', OLD.id);
    ELSE
        payload = payload || jsonb_build_object('id', NEW.id);
    END IF;
    
    -- Notify listeners
    PERFORM pg_notify('forge_changes', payload::text);
    
    -- Also log to events table (optional, for replay)
    INSERT INTO forge_events (table_name, operation, row_id, old_data, new_data)
    VALUES (
        TG_TABLE_NAME,
        TG_OP,
        COALESCE(NEW.id, OLD.id),
        CASE WHEN TG_OP IN ('UPDATE', 'DELETE') THEN to_jsonb(OLD) END,
        CASE WHEN TG_OP IN ('INSERT', 'UPDATE') THEN to_jsonb(NEW) END
    );
    
    RETURN COALESCE(NEW, OLD);
END;
$$ LANGUAGE plpgsql;

-- Apply to user tables (generated by FORGE)
-- CREATE TRIGGER users_notify_changes
--     AFTER INSERT OR UPDATE OR DELETE ON users
--     FOR EACH ROW EXECUTE FUNCTION forge_notify_change();
```

---

## Observability Tables

### Metrics

```sql
CREATE TABLE forge_metrics (
    time TIMESTAMPTZ NOT NULL,
    name VARCHAR(255) NOT NULL,
    
    -- Labels (dimensions)
    labels JSONB NOT NULL DEFAULT '{}',
    
    -- Value
    value DOUBLE PRECISION NOT NULL,
    
    -- Source
    node_id UUID
) PARTITION BY RANGE (time);

-- Create partitions (daily)
CREATE TABLE forge_metrics_default PARTITION OF forge_metrics DEFAULT;

-- Indexes
CREATE INDEX idx_forge_metrics_time_name ON forge_metrics(time DESC, name);
CREATE INDEX idx_forge_metrics_labels ON forge_metrics USING GIN(labels);

-- Aggregated metrics (for longer retention)
CREATE TABLE forge_metrics_1m (
    time TIMESTAMPTZ NOT NULL,
    name VARCHAR(255) NOT NULL,
    labels JSONB NOT NULL DEFAULT '{}',
    
    -- Aggregates
    count INTEGER NOT NULL,
    sum DOUBLE PRECISION NOT NULL,
    min DOUBLE PRECISION NOT NULL,
    max DOUBLE PRECISION NOT NULL,
    avg DOUBLE PRECISION NOT NULL,
    
    PRIMARY KEY (time, name, labels)
);
```

### Logs

```sql
CREATE TABLE forge_logs (
    id BIGSERIAL,
    timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    
    -- Log level
    level VARCHAR(20) NOT NULL,
    
    -- Message
    message TEXT NOT NULL,
    
    -- Context
    node_id UUID,
    trace_id VARCHAR(32),
    span_id VARCHAR(16),
    
    -- Structured data
    function_name VARCHAR(255),
    function_type VARCHAR(50),
    user_id UUID,
    
    -- Additional fields
    fields JSONB DEFAULT '{}'
) PARTITION BY RANGE (timestamp);

-- Partitions created automatically
CREATE TABLE forge_logs_default PARTITION OF forge_logs DEFAULT;

-- Indexes
CREATE INDEX idx_forge_logs_timestamp ON forge_logs(timestamp DESC);
CREATE INDEX idx_forge_logs_level ON forge_logs(level, timestamp DESC);
CREATE INDEX idx_forge_logs_trace ON forge_logs(trace_id) WHERE trace_id IS NOT NULL;
CREATE INDEX idx_forge_logs_function ON forge_logs(function_name, timestamp DESC);
CREATE INDEX idx_forge_logs_fields ON forge_logs USING GIN(fields);
```

### Traces

```sql
CREATE TABLE forge_traces (
    trace_id VARCHAR(32) NOT NULL,
    span_id VARCHAR(16) NOT NULL,
    parent_span_id VARCHAR(16),
    
    -- Span info
    operation_name VARCHAR(255) NOT NULL,
    service_name VARCHAR(100) NOT NULL,
    node_id UUID,
    
    -- Timing
    start_time TIMESTAMPTZ NOT NULL,
    duration_ms INTEGER NOT NULL,
    
    -- Status
    status VARCHAR(20),  -- ok, error
    status_message TEXT,
    
    -- Data
    tags JSONB DEFAULT '{}',
    logs JSONB DEFAULT '[]',
    
    PRIMARY KEY (trace_id, span_id)
) PARTITION BY RANGE (start_time);

CREATE TABLE forge_traces_default PARTITION OF forge_traces DEFAULT;

CREATE INDEX idx_forge_traces_time ON forge_traces(start_time DESC);
CREATE INDEX idx_forge_traces_operation ON forge_traces(operation_name, start_time DESC);
CREATE INDEX idx_forge_traces_service ON forge_traces(service_name, start_time DESC);
CREATE INDEX idx_forge_traces_tags ON forge_traces USING GIN(tags);
```

---

## Session Tables

### WebSocket Sessions

```sql
CREATE TABLE forge_sessions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    
    -- Connection info
    node_id UUID NOT NULL REFERENCES forge_nodes(id) ON DELETE CASCADE,
    client_ip INET,
    user_agent TEXT,
    
    -- Auth
    user_id UUID,
    auth_token_hash VARCHAR(64),
    
    -- Status
    connected_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_activity TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    
    -- Subscriptions stored separately
    subscription_count INTEGER DEFAULT 0
);

CREATE INDEX idx_forge_sessions_node ON forge_sessions(node_id);
CREATE INDEX idx_forge_sessions_user ON forge_sessions(user_id) WHERE user_id IS NOT NULL;
```

### Subscriptions

```sql
CREATE TABLE forge_subscriptions (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    session_id UUID NOT NULL REFERENCES forge_sessions(id) ON DELETE CASCADE,
    
    -- Query info
    query_name VARCHAR(255) NOT NULL,
    query_args JSONB NOT NULL,
    query_hash VARCHAR(64) NOT NULL,  -- For deduplication
    
    -- Read set (for invalidation)
    read_tables TEXT[],
    read_rows JSONB,  -- {table: [ids]}
    
    -- State
    last_result_hash VARCHAR(64),
    last_updated TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    
    -- Prevent duplicate subscriptions per session
    UNIQUE(session_id, query_hash)
);

CREATE INDEX idx_forge_subscriptions_session ON forge_subscriptions(session_id);
CREATE INDEX idx_forge_subscriptions_tables ON forge_subscriptions USING GIN(read_tables);
```

---

## Scaling PostgreSQL

### Connection Pooling

```toml
# forge.toml

[database]
url = "postgres://user:pass@localhost/forge"
pool_size = 50
pool_timeout = "30s"

# Or use PgBouncer
# url = "postgres://user:pass@pgbouncer:6432/forge"
```

### Table Partitioning

```sql
-- Partition large tables by time

-- Logs: daily partitions
CREATE TABLE forge_logs_2024_01_15 PARTITION OF forge_logs
    FOR VALUES FROM ('2024-01-15') TO ('2024-01-16');

-- Auto-create partitions with pg_partman
SELECT partman.create_parent(
    p_parent_table := 'public.forge_logs',
    p_control := 'timestamp',
    p_type := 'native',
    p_interval := 'daily',
    p_premake := 7
);
```

### Read Replicas

```toml
# forge.toml

[database]
# Primary for writes
primary_url = "postgres://user:pass@primary:5432/forge"

# Replicas for reads
replica_urls = [
    "postgres://user:pass@replica1:5432/forge",
    "postgres://user:pass@replica2:5432/forge",
]

# Route queries to replicas
read_from_replica = true
```

### Indexes Strategy

```sql
-- Only index what you query

-- Good: Specific, used in WHERE clauses
CREATE INDEX idx_projects_owner_status ON projects(owner_id, status)
    WHERE deleted_at IS NULL;

-- Bad: Too broad, rarely used
CREATE INDEX idx_projects_all_columns ON projects(id, owner_id, name, status, created_at);

-- Use partial indexes
CREATE INDEX idx_jobs_pending ON forge_jobs(priority DESC, scheduled_at)
    WHERE status = 'pending';

-- Use covering indexes for common queries
CREATE INDEX idx_users_email_covering ON users(email) INCLUDE (name, created_at);
```

---

## Data Retention

```sql
-- Automatic cleanup of old data

-- Delete old completed jobs
DELETE FROM forge_jobs 
WHERE status = 'completed' 
AND completed_at < NOW() - INTERVAL '7 days';

-- Delete old logs
DROP TABLE IF EXISTS forge_logs_2024_01_01;  -- Drop old partitions

-- Aggregate and delete old metrics
INSERT INTO forge_metrics_1h
SELECT 
    date_trunc('hour', time) as time,
    name,
    labels,
    count(*),
    sum(value),
    min(value),
    max(value),
    avg(value)
FROM forge_metrics
WHERE time < NOW() - INTERVAL '1 hour'
GROUP BY 1, 2, 3;

DELETE FROM forge_metrics WHERE time < NOW() - INTERVAL '1 hour';
```

---

## Related Documentation

- [Job Queue](JOB_QUEUE.md) — SKIP LOCKED pattern
- [Change Tracking](CHANGE_TRACKING.md) — LISTEN/NOTIFY
- [Migrations](MIGRATIONS.md) — Schema management

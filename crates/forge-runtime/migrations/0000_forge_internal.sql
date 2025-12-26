-- FORGE Internal Schema v1
-- This migration creates all system tables required by the FORGE runtime.
-- It is applied automatically before any user migrations.

-- Cluster: Node registry
CREATE TABLE IF NOT EXISTS forge_nodes (
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

-- Cluster: Leader election
CREATE TABLE IF NOT EXISTS forge_leaders (
    role VARCHAR(64) PRIMARY KEY,
    node_id UUID NOT NULL,
    acquired_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    lease_until TIMESTAMPTZ NOT NULL
);

-- Jobs: Background job queue
CREATE TABLE IF NOT EXISTS forge_jobs (
    id UUID PRIMARY KEY,
    job_type VARCHAR(255) NOT NULL,
    input JSONB NOT NULL DEFAULT '{}',
    output JSONB,
    status VARCHAR(32) NOT NULL DEFAULT 'pending',
    priority INTEGER NOT NULL DEFAULT 50,
    attempts INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 3,
    last_error TEXT,
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

CREATE INDEX IF NOT EXISTS idx_forge_jobs_status_scheduled
    ON forge_jobs(status, scheduled_at)
    WHERE status = 'pending';

CREATE INDEX IF NOT EXISTS idx_forge_jobs_idempotency
    ON forge_jobs(idempotency_key)
    WHERE idempotency_key IS NOT NULL;

-- Cron: Execution history
CREATE TABLE IF NOT EXISTS forge_cron_runs (
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

CREATE INDEX IF NOT EXISTS idx_forge_cron_runs_name_time
    ON forge_cron_runs(cron_name, scheduled_time DESC);

-- Workflows: Run state
CREATE TABLE IF NOT EXISTS forge_workflow_runs (
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

CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_status
    ON forge_workflow_runs(status);

-- Workflows: Step state
CREATE TABLE IF NOT EXISTS forge_workflow_steps (
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

-- Observability: Metrics
CREATE TABLE IF NOT EXISTS forge_metrics (
    id BIGSERIAL PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    kind VARCHAR(32) NOT NULL,
    value DOUBLE PRECISION NOT NULL,
    labels JSONB DEFAULT '{}',
    timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_forge_metrics_name_time
    ON forge_metrics(name, timestamp DESC);

-- Observability: Logs
CREATE TABLE IF NOT EXISTS forge_logs (
    id BIGSERIAL PRIMARY KEY,
    level VARCHAR(16) NOT NULL,
    message TEXT NOT NULL,
    target VARCHAR(255),
    fields JSONB DEFAULT '{}',
    trace_id VARCHAR(64),
    span_id VARCHAR(32),
    timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_forge_logs_level_time
    ON forge_logs(level, timestamp DESC);

CREATE INDEX IF NOT EXISTS idx_forge_logs_trace_id
    ON forge_logs(trace_id)
    WHERE trace_id IS NOT NULL;

-- Observability: Traces
CREATE TABLE IF NOT EXISTS forge_traces (
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

CREATE INDEX IF NOT EXISTS idx_forge_traces_trace_id
    ON forge_traces(trace_id);

CREATE INDEX IF NOT EXISTS idx_forge_traces_started_at
    ON forge_traces(started_at DESC);

-- Realtime: Sessions
CREATE TABLE IF NOT EXISTS forge_sessions (
    id UUID PRIMARY KEY,
    node_id UUID NOT NULL,
    user_id VARCHAR(255),
    connected_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_activity TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    status VARCHAR(32) NOT NULL DEFAULT 'connected'
);

CREATE INDEX IF NOT EXISTS idx_forge_sessions_node
    ON forge_sessions(node_id);

-- Realtime: Subscriptions
CREATE TABLE IF NOT EXISTS forge_subscriptions (
    id UUID PRIMARY KEY,
    session_id UUID NOT NULL REFERENCES forge_sessions(id) ON DELETE CASCADE,
    query_name VARCHAR(255) NOT NULL,
    query_hash VARCHAR(64) NOT NULL,
    args JSONB DEFAULT '{}',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_forge_subscriptions_session
    ON forge_subscriptions(session_id);

CREATE INDEX IF NOT EXISTS idx_forge_subscriptions_query_hash
    ON forge_subscriptions(query_hash);

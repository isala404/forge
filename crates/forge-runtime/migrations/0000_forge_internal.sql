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
    version INTEGER DEFAULT 1,
    input JSONB NOT NULL DEFAULT '{}',
    output JSONB,
    status VARCHAR(32) NOT NULL DEFAULT 'created',
    current_step VARCHAR(255),
    step_results JSONB DEFAULT '{}',
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    error TEXT,
    trace_id VARCHAR(64),
    -- Durable workflow support
    suspended_at TIMESTAMPTZ,
    wake_at TIMESTAMPTZ,
    waiting_for_event TEXT,
    event_timeout_at TIMESTAMPTZ,
    tenant_id UUID
);

CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_status
    ON forge_workflow_runs(status);

CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_wake
    ON forge_workflow_runs(wake_at)
    WHERE status = 'waiting' AND wake_at IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_tenant
    ON forge_workflow_runs(tenant_id)
    WHERE tenant_id IS NOT NULL;

-- Workflows: Event storage for durable workflows
CREATE TABLE IF NOT EXISTS forge_workflow_events (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    event_name TEXT NOT NULL,
    correlation_id TEXT NOT NULL,
    payload JSONB,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    consumed_at TIMESTAMPTZ,
    consumed_by UUID REFERENCES forge_workflow_runs(id)
);

CREATE INDEX IF NOT EXISTS idx_forge_workflow_events_lookup
    ON forge_workflow_events(event_name, correlation_id)
    WHERE consumed_at IS NULL;

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

-- Rate Limiting: Token bucket storage
CREATE TABLE IF NOT EXISTS forge_rate_limits (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    bucket_key TEXT NOT NULL,
    tokens DOUBLE PRECISION NOT NULL,
    last_refill TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    max_tokens INTEGER NOT NULL,
    refill_rate DOUBLE PRECISION NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_forge_rate_limits_bucket
    ON forge_rate_limits(bucket_key);

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

-- Realtime: Change notification function
-- This function sends a NOTIFY on the forge_changes channel when data changes.
-- Format: table_name:operation:row_id
CREATE OR REPLACE FUNCTION forge_notify_change() RETURNS TRIGGER AS $$
DECLARE
    row_id TEXT;
    payload TEXT;
BEGIN
    -- Get the row ID (assumes 'id' column exists, falls back to empty)
    IF TG_OP = 'DELETE' THEN
        row_id := COALESCE(OLD.id::TEXT, '');
    ELSE
        row_id := COALESCE(NEW.id::TEXT, '');
    END IF;

    -- Build payload: table:operation:row_id
    payload := TG_TABLE_NAME || ':' || TG_OP || ':' || row_id;

    -- Send notification
    PERFORM pg_notify('forge_changes', payload);

    -- Return appropriate row
    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    ELSE
        RETURN NEW;
    END IF;
END;
$$ LANGUAGE plpgsql;

-- Helper function to enable reactivity on a table
-- Usage: SELECT forge_enable_reactivity('my_table');
CREATE OR REPLACE FUNCTION forge_enable_reactivity(table_name TEXT) RETURNS VOID AS $$
DECLARE
    trigger_name TEXT;
BEGIN
    trigger_name := 'forge_notify_' || table_name;

    -- Drop existing trigger if any
    EXECUTE format('DROP TRIGGER IF EXISTS %I ON %I', trigger_name, table_name);

    -- Create new trigger
    EXECUTE format('
        CREATE TRIGGER %I
        AFTER INSERT OR UPDATE OR DELETE ON %I
        FOR EACH ROW EXECUTE FUNCTION forge_notify_change()
    ', trigger_name, table_name);
END;
$$ LANGUAGE plpgsql;

-- Helper function to disable reactivity on a table
CREATE OR REPLACE FUNCTION forge_disable_reactivity(table_name TEXT) RETURNS VOID AS $$
DECLARE
    trigger_name TEXT;
BEGIN
    trigger_name := 'forge_notify_' || table_name;
    EXECUTE format('DROP TRIGGER IF EXISTS %I ON %I', trigger_name, table_name);
END;
$$ LANGUAGE plpgsql;

-- Observability: Alert Rules
CREATE TABLE IF NOT EXISTS forge_alert_rules (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(255) NOT NULL UNIQUE,
    description TEXT,
    metric_name VARCHAR(255) NOT NULL,
    condition VARCHAR(32) NOT NULL,  -- 'gt', 'gte', 'lt', 'lte', 'eq', 'ne'
    threshold DOUBLE PRECISION NOT NULL,
    duration_seconds INTEGER NOT NULL DEFAULT 0,  -- Condition must be true for this long
    severity VARCHAR(32) NOT NULL DEFAULT 'warning',  -- 'info', 'warning', 'critical'
    enabled BOOLEAN NOT NULL DEFAULT TRUE,
    labels JSONB DEFAULT '{}',  -- Labels to match on metric
    notification_channels TEXT[] DEFAULT '{}',  -- ['email', 'slack', 'webhook']
    cooldown_seconds INTEGER NOT NULL DEFAULT 300,  -- Wait this long before re-alerting
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_forge_alert_rules_enabled
    ON forge_alert_rules(enabled)
    WHERE enabled = TRUE;

-- Observability: Active Alerts
CREATE TABLE IF NOT EXISTS forge_alerts (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    rule_id UUID NOT NULL REFERENCES forge_alert_rules(id) ON DELETE CASCADE,
    rule_name VARCHAR(255) NOT NULL,
    metric_value DOUBLE PRECISION NOT NULL,
    threshold DOUBLE PRECISION NOT NULL,
    severity VARCHAR(32) NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'firing',  -- 'firing', 'resolved'
    triggered_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    resolved_at TIMESTAMPTZ,
    acknowledged_at TIMESTAMPTZ,
    acknowledged_by VARCHAR(255),
    labels JSONB DEFAULT '{}',
    annotations JSONB DEFAULT '{}'
);

CREATE INDEX IF NOT EXISTS idx_forge_alerts_status
    ON forge_alerts(status)
    WHERE status = 'firing';

CREATE INDEX IF NOT EXISTS idx_forge_alerts_rule_id
    ON forge_alerts(rule_id);

CREATE INDEX IF NOT EXISTS idx_forge_alerts_triggered_at
    ON forge_alerts(triggered_at DESC);

-- Enable reactivity on job/workflow tables for WebSocket subscriptions
SELECT forge_enable_reactivity('forge_jobs');
SELECT forge_enable_reactivity('forge_workflow_runs');
SELECT forge_enable_reactivity('forge_workflow_steps');

-- TODO(pre-1.0): Split per subsystem (cluster, jobs, workflows, signals)
-- FORGE Internal Schema v1
-- This migration creates all system tables required by the FORGE runtime.
-- It is applied automatically before any user migrations.

-- Cluster: Node registry (UNLOGGED: transient state rebuilt on startup)
CREATE UNLOGGED TABLE IF NOT EXISTS forge_nodes (
    id UUID PRIMARY KEY,
    hostname VARCHAR(255) NOT NULL,
    ip_address VARCHAR(64) NOT NULL,
    http_port INTEGER NOT NULL,
    grpc_port INTEGER NOT NULL,
    roles TEXT[] NOT NULL DEFAULT '{}',
    worker_capabilities TEXT[] NOT NULL DEFAULT '{}',
    status VARCHAR(32) NOT NULL DEFAULT 'starting',
    version VARCHAR(64),
    current_connections INTEGER NOT NULL DEFAULT 0,
    current_jobs INTEGER NOT NULL DEFAULT 0,
    cpu_usage DOUBLE PRECISION,
    memory_usage DOUBLE PRECISION,
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_heartbeat TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_forge_nodes_status_heartbeat
    ON forge_nodes(status, last_heartbeat)
    WHERE status = 'active';

-- Cluster: Leader election (UNLOGGED: transient state rebuilt on startup).
-- The PG advisory lock is the only source of truth for who holds leadership;
-- this table just records visibility metadata (current node, lease window).
CREATE UNLOGGED TABLE IF NOT EXISTS forge_leaders (
    role VARCHAR(64) PRIMARY KEY,
    node_id UUID NOT NULL,
    acquired_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    lease_until TIMESTAMPTZ NOT NULL
);

-- Jobs: Background job queue
CREATE TABLE IF NOT EXISTS forge_jobs (
    id UUID PRIMARY KEY,
    job_type VARCHAR(255) NOT NULL,
    queue VARCHAR(64) NOT NULL DEFAULT 'default',
    kind VARCHAR(32) NOT NULL DEFAULT 'normal',
    input JSONB NOT NULL DEFAULT '{}',
    output JSONB,
    job_context JSONB NOT NULL DEFAULT '{}',
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
    owner_subject TEXT,
    scheduled_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    claimed_at TIMESTAMPTZ,
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    failed_at TIMESTAMPTZ,
    cancel_requested_at TIMESTAMPTZ,
    cancelled_at TIMESTAMPTZ,
    cancel_reason TEXT,
    last_heartbeat TIMESTAMPTZ,
    expires_at TIMESTAMPTZ,
    -- Forward-compat slot. Future-versioned fields land here without ALTER TABLE.
    metadata JSONB NOT NULL DEFAULT '{}'
);

CREATE INDEX IF NOT EXISTS idx_forge_jobs_status_scheduled
    ON forge_jobs(status, scheduled_at)
    WHERE status = 'pending';

CREATE INDEX IF NOT EXISTS idx_forge_jobs_queue_status_scheduled
    ON forge_jobs(queue, status, scheduled_at)
    WHERE status = 'pending';

CREATE UNIQUE INDEX IF NOT EXISTS idx_forge_jobs_idempotency
    ON forge_jobs(idempotency_key)
    WHERE idempotency_key IS NOT NULL
      AND status NOT IN ('completed', 'failed', 'dead_letter', 'cancelled');

CREATE INDEX IF NOT EXISTS idx_forge_jobs_owner_subject
    ON forge_jobs(owner_subject)
    WHERE owner_subject IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_forge_jobs_expires
    ON forge_jobs(expires_at)
    WHERE expires_at IS NOT NULL;

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
    owner_subject TEXT,
    UNIQUE(cron_name, scheduled_time)
);

CREATE INDEX IF NOT EXISTS idx_forge_cron_runs_name_time
    ON forge_cron_runs(cron_name, scheduled_time DESC);

-- Workflows: Definition registry (upserted on startup)
CREATE TABLE IF NOT EXISTS forge_workflow_definitions (
    workflow_name VARCHAR(255) NOT NULL,
    workflow_version VARCHAR(255) NOT NULL,
    workflow_signature VARCHAR(64) NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (workflow_name, workflow_version)
);

-- Workflows: Run state
CREATE TABLE IF NOT EXISTS forge_workflow_runs (
    id UUID PRIMARY KEY,
    workflow_name VARCHAR(255) NOT NULL,
    workflow_version VARCHAR(255) NOT NULL,
    workflow_signature VARCHAR(64) NOT NULL,
    owner_subject TEXT,
    input JSONB NOT NULL DEFAULT '{}',
    output JSONB,
    status VARCHAR(32) NOT NULL DEFAULT 'created',
    blocking_reason TEXT,
    resolution_reason TEXT,
    current_step VARCHAR(255),
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    error TEXT,
    trace_id VARCHAR(64),
    -- Durable workflow support
    suspended_at TIMESTAMPTZ,
    wake_at TIMESTAMPTZ,
    waiting_for_event TEXT,
    event_timeout_at TIMESTAMPTZ,
    tenant_id UUID,
    -- Compensation metadata for crash-safe saga compensation
    compensation_state JSONB,
    -- User-defined key-value state that persists across suspension points
    saved_state JSONB DEFAULT '{}',
    -- Operator cancel signal. The scheduler picks up rows where
    -- cancel_requested_at IS NOT NULL via the forge_workflow_runs_cancel_notify
    -- trigger and runs compensation immediately, bypassing any wake_at timer.
    cancel_requested_at TIMESTAMPTZ,
    cancel_reason TEXT,
    -- Forward-compat slot. Future-versioned fields land here without ALTER TABLE.
    metadata JSONB NOT NULL DEFAULT '{}'
);

CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_cancel_requested
    ON forge_workflow_runs(cancel_requested_at)
    WHERE cancel_requested_at IS NOT NULL
      AND status IN ('pending', 'running', 'sleeping', 'waiting');

CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_status
    ON forge_workflow_runs(status);

CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_wake
    ON forge_workflow_runs(wake_at)
    WHERE status = 'waiting' AND wake_at IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_tenant
    ON forge_workflow_runs(tenant_id)
    WHERE tenant_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_owner_subject
    ON forge_workflow_runs(owner_subject)
    WHERE owner_subject IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_name_version
    ON forge_workflow_runs(workflow_name, workflow_version)
    WHERE status NOT IN ('completed', 'failed', 'compensated', 'retired_unresumable', 'cancelled_by_operator');

-- Workflows: Event storage for durable workflows
CREATE TABLE IF NOT EXISTS forge_workflow_events (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
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

-- Admin audit log. One row per privileged action taken via /_api/admin/*.
-- Append-only by convention; cleanup is the operator's responsibility.
CREATE TABLE IF NOT EXISTS forge_admin_audit (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    occurred_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    actor_subject TEXT,
    actor_roles TEXT[] NOT NULL DEFAULT '{}',
    action VARCHAR(64) NOT NULL,
    target_type VARCHAR(32) NOT NULL,
    target_id TEXT,
    reason TEXT,
    request_id VARCHAR(64),
    trace_id VARCHAR(64),
    details JSONB NOT NULL DEFAULT '{}'
);

CREATE INDEX IF NOT EXISTS idx_forge_admin_audit_occurred_at
    ON forge_admin_audit(occurred_at DESC);

CREATE INDEX IF NOT EXISTS idx_forge_admin_audit_actor_subject
    ON forge_admin_audit(actor_subject)
    WHERE actor_subject IS NOT NULL;

-- Operator pause state for queues. A queue listed here is paused; workers
-- targeting that capability skip the `claim` SQL when their capability shows
-- up in this table. Kept tiny so operator pauses survive node restarts but
-- can be removed without a config push.
CREATE TABLE IF NOT EXISTS forge_paused_queues (
    queue_name VARCHAR(255) PRIMARY KEY,
    paused_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    paused_by TEXT,
    reason TEXT
);

-- Rate Limiting: Token bucket storage (UNLOGGED: transient state rebuilt on startup)
CREATE UNLOGGED TABLE IF NOT EXISTS forge_rate_limits (
    id UUID PRIMARY KEY DEFAULT uuidv7(),
    bucket_key TEXT NOT NULL,
    tokens DOUBLE PRECISION NOT NULL,
    last_refill TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    max_tokens INTEGER NOT NULL,
    refill_rate DOUBLE PRECISION NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE UNIQUE INDEX IF NOT EXISTS idx_forge_rate_limits_bucket
    ON forge_rate_limits(bucket_key);

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
-- Sends NOTIFY on forge_changes channel when data changes.
-- Format: v1:table:OP:row_id or v1:table:OP:row_id:col1,col2,... (UPDATE only).
-- The leading "v1:" prefix lets future schema bumps land without coordinated
-- cluster restart: a v2 listener can branch on the prefix while v1 emitters
-- and listeners stay in service during rolling upgrades.
CREATE OR REPLACE FUNCTION forge_notify_change() RETURNS TRIGGER AS $$
DECLARE
    row_id TEXT;
    payload TEXT;
    old_json JSONB;
    new_json JSONB;
    changed_cols TEXT[];
BEGIN
    IF TG_OP = 'DELETE' THEN
        row_id := COALESCE(OLD.id::TEXT, '');
    ELSE
        row_id := COALESCE(NEW.id::TEXT, '');
    END IF;

    payload := 'v1:' || TG_TABLE_NAME || ':' || TG_OP || ':' || row_id;

    IF TG_OP = 'UPDATE' THEN
        old_json := to_jsonb(OLD);
        new_json := to_jsonb(NEW);
        changed_cols := ARRAY(
            SELECT key FROM jsonb_each(new_json)
            WHERE new_json -> key IS DISTINCT FROM old_json -> key
        );
        IF array_length(changed_cols, 1) > 0 THEN
            payload := payload || ':' || array_to_string(changed_cols, ',');
        END IF;
    END IF;

    PERFORM pg_notify('forge_changes', payload);

    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    ELSE
        RETURN NEW;
    END IF;
END;
$$ LANGUAGE plpgsql;

-- Validate a PostgreSQL identifier before splicing it into dynamic DDL.
--
-- `format('%I', ...)` already quotes the identifier, so this is not about
-- SQL injection — it's about catching authoring mistakes that PG itself
-- would either truncate silently (>63 chars) or reject with a confusing
-- error far from the call site. Raising here gives the migration author
-- a clear, actionable failure.
--
-- Rules (mirror PG's `NAMEDATALEN - 1 = 63` byte budget and the `pg_*`
-- reservation policy from the docs):
--   - non-empty
--   - octet length <= 63 (so the identifier fits without truncation)
--   - does not start with `pg_` (reserved for system catalogs)
CREATE OR REPLACE FUNCTION forge_validate_identifier(name TEXT) RETURNS VOID AS $$
BEGIN
    IF name IS NULL OR name = '' THEN
        RAISE EXCEPTION 'forge_validate_identifier: identifier must not be empty';
    END IF;
    IF octet_length(name) > 63 THEN
        RAISE EXCEPTION
            'forge_validate_identifier: identifier % exceeds 63 bytes (PG would silently truncate)',
            name;
    END IF;
    IF name LIKE 'pg\_%' ESCAPE '\' THEN
        RAISE EXCEPTION
            'forge_validate_identifier: identifier % uses reserved pg_ prefix',
            name;
    END IF;
END;
$$ LANGUAGE plpgsql;

-- Helper function to enable reactivity on a table
-- Usage: SELECT forge_enable_reactivity('my_table');
CREATE OR REPLACE FUNCTION forge_enable_reactivity(table_name TEXT) RETURNS VOID AS $$
DECLARE
    trigger_name TEXT;
BEGIN
    PERFORM forge_validate_identifier(table_name);
    trigger_name := 'forge_notify_' || table_name;
    -- Validate the derived trigger name too: a 51+ char table_name would
    -- pass the input check but push the prefixed trigger over 63 bytes.
    PERFORM forge_validate_identifier(trigger_name);

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
    PERFORM forge_validate_identifier(table_name);
    trigger_name := 'forge_notify_' || table_name;
    PERFORM forge_validate_identifier(trigger_name);
    EXECUTE format('DROP TRIGGER IF EXISTS %I ON %I', trigger_name, table_name);
END;
$$ LANGUAGE plpgsql;

-- GIN indexes for JSONB columns (enables efficient queries on JSON data)

-- Jobs: Enable queries on input/output JSON
CREATE INDEX IF NOT EXISTS idx_forge_jobs_input_gin
    ON forge_jobs USING GIN (input);
CREATE INDEX IF NOT EXISTS idx_forge_jobs_output_gin
    ON forge_jobs USING GIN (output)
    WHERE output IS NOT NULL;

-- Workflows: Enable queries on workflow data
CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_input_gin
    ON forge_workflow_runs USING GIN (input);
CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_output_gin
    ON forge_workflow_runs USING GIN (output)
    WHERE output IS NOT NULL;

-- Workflow events: Enable queries on event payload
CREATE INDEX IF NOT EXISTS idx_forge_workflow_events_payload_gin
    ON forge_workflow_events USING GIN (payload)
    WHERE payload IS NOT NULL;

-- Workflow steps: Enable queries on step data
CREATE INDEX IF NOT EXISTS idx_forge_workflow_steps_input_gin
    ON forge_workflow_steps USING GIN (input)
    WHERE input IS NOT NULL;
CREATE INDEX IF NOT EXISTS idx_forge_workflow_steps_result_gin
    ON forge_workflow_steps USING GIN (result)
    WHERE result IS NOT NULL;

-- Subscriptions: Enable args matching
CREATE INDEX IF NOT EXISTS idx_forge_subscriptions_args_gin
    ON forge_subscriptions USING GIN (args);

-- Enable reactivity on job/workflow tables for WebSocket subscriptions
SELECT forge_enable_reactivity('forge_jobs');
SELECT forge_enable_reactivity('forge_workflow_runs');
SELECT forge_enable_reactivity('forge_workflow_steps');

-- Daemons: Long-running singleton tasks
CREATE TABLE IF NOT EXISTS forge_daemons (
    name VARCHAR(255) PRIMARY KEY,
    node_id UUID,
    instance_id UUID NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'stopped',
    restarts INTEGER NOT NULL DEFAULT 0,
    last_error TEXT,
    started_at TIMESTAMPTZ,
    last_heartbeat TIMESTAMPTZ
);

CREATE INDEX IF NOT EXISTS idx_forge_daemons_status
    ON forge_daemons(status);

CREATE INDEX IF NOT EXISTS idx_forge_daemons_node
    ON forge_daemons(node_id)
    WHERE node_id IS NOT NULL;

-- Webhooks: Idempotency tracking for webhook events
CREATE TABLE IF NOT EXISTS forge_webhook_events (
    webhook_name VARCHAR(255) NOT NULL,
    idempotency_key VARCHAR(255) NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'claimed',
    processed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (webhook_name, idempotency_key)
);

CREATE INDEX IF NOT EXISTS idx_forge_webhook_events_expires
    ON forge_webhook_events(expires_at);

CREATE INDEX IF NOT EXISTS idx_forge_webhook_events_webhook
    ON forge_webhook_events(webhook_name);

-- Workflow event-driven wakeup via NOTIFY.
-- When a workflow event is inserted, notify the scheduler immediately
-- instead of waiting for the next poll cycle.
CREATE OR REPLACE FUNCTION forge_workflow_event_notify() RETURNS TRIGGER AS $$
BEGIN
    PERFORM pg_notify('forge_workflow_wakeup', NEW.correlation_id);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER forge_workflow_event_notify_trigger
    AFTER INSERT ON forge_workflow_events
    FOR EACH ROW EXECUTE FUNCTION forge_workflow_event_notify();

-- Workflow cancel wakeup via NOTIFY.
-- When an operator sets cancel_requested_at on a suspended run, the scheduler
-- listens on forge_workflow_wakeup and picks the row up within a single poll
-- cycle (target: <50ms) instead of waiting for the wake_at timer to fire.
CREATE OR REPLACE FUNCTION forge_workflow_runs_cancel_notify() RETURNS TRIGGER AS $$
BEGIN
    IF (OLD.cancel_requested_at IS NULL AND NEW.cancel_requested_at IS NOT NULL) THEN
        PERFORM pg_notify('forge_workflow_wakeup', NEW.id::text);
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER forge_workflow_runs_cancel_notify_trigger
    AFTER UPDATE OF cancel_requested_at ON forge_workflow_runs
    FOR EACH ROW EXECUTE FUNCTION forge_workflow_runs_cancel_notify();

-- Periodic cleanup function for expired webhook idempotency records
-- This can be called from a cron job: SELECT forge_cleanup_webhook_events();
CREATE OR REPLACE FUNCTION forge_cleanup_webhook_events() RETURNS INTEGER AS $$
DECLARE
    deleted_count INTEGER;
BEGIN
    DELETE FROM forge_webhook_events WHERE expires_at < NOW();
    GET DIAGNOSTICS deleted_count = ROW_COUNT;
    RETURN deleted_count;
END;
$$ LANGUAGE plpgsql;

-- Periodic cleanup function for expired job records
-- Deletes completed/cancelled/failed jobs past their TTL
-- This can be called from a cron job: SELECT forge_cleanup_expired_jobs();
CREATE OR REPLACE FUNCTION forge_cleanup_expired_jobs() RETURNS INTEGER AS $$
DECLARE
    deleted_count INTEGER;
BEGIN
    DELETE FROM forge_jobs
    WHERE expires_at IS NOT NULL
      AND expires_at < NOW()
      AND status IN ('completed', 'cancelled', 'failed', 'dead_letter');
    GET DIAGNOSTICS deleted_count = ROW_COUNT;
    RETURN deleted_count;
END;
$$ LANGUAGE plpgsql;

-- Auth: Refresh token storage for built-in token rotation
CREATE TABLE IF NOT EXISTS forge_refresh_tokens (
    id          UUID PRIMARY KEY DEFAULT uuidv7(),
    user_id     UUID NOT NULL,
    token_hash  TEXT NOT NULL UNIQUE,
    client_id   TEXT,
    token_family UUID NOT NULL DEFAULT uuidv7(),
    -- Roles snapshot at sign-in. Carried forward on rotation so refreshes
    -- never silently downgrade or escalate; new roles take effect at next
    -- sign-in, which matches the session-bounded security model.
    roles       TEXT[] NOT NULL DEFAULT '{}',
    expires_at  TIMESTAMPTZ NOT NULL,
    created_at  TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_forge_refresh_tokens_user_id
    ON forge_refresh_tokens (user_id);

CREATE INDEX IF NOT EXISTS idx_forge_refresh_tokens_expires_at
    ON forge_refresh_tokens (expires_at);

CREATE INDEX IF NOT EXISTS idx_refresh_tokens_family
    ON forge_refresh_tokens (token_family);

-- Periodically purge expired tokens to prevent table bloat.
-- Runs every hour, deleting tokens that expired more than 24 hours ago
-- (keeps recently-expired tokens for audit/error-reporting purposes).
CREATE OR REPLACE FUNCTION forge_purge_expired_refresh_tokens()
RETURNS void LANGUAGE sql AS $$
    DELETE FROM forge_refresh_tokens
    WHERE expires_at < now() - interval '24 hours';
$$;

-- OAuth: Dynamic client registrations (MCP clients self-register via RFC 7591)
CREATE TABLE IF NOT EXISTS forge_oauth_clients (
    client_id                  TEXT PRIMARY KEY DEFAULT uuidv7()::TEXT,
    client_name                TEXT,
    redirect_uris              TEXT[] NOT NULL DEFAULT '{}',
    token_endpoint_auth_method TEXT NOT NULL DEFAULT 'none',
    created_at                 TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- OAuth: Authorization codes (short-lived, PKCE-bound)
CREATE TABLE IF NOT EXISTS forge_oauth_codes (
    code                   TEXT PRIMARY KEY,
    client_id              TEXT NOT NULL REFERENCES forge_oauth_clients(client_id) ON DELETE CASCADE,
    user_id                UUID NOT NULL,
    redirect_uri           TEXT NOT NULL,
    code_challenge         TEXT NOT NULL,
    code_challenge_method  TEXT NOT NULL DEFAULT 'S256',
    scopes                 TEXT[] NOT NULL DEFAULT '{}',
    expires_at             TIMESTAMPTZ NOT NULL,
    used_at                TIMESTAMPTZ,
    created_at             TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_forge_oauth_codes_expires
    ON forge_oauth_codes(expires_at);

-- Purge expired authorization codes (called by cron or manually)
CREATE OR REPLACE FUNCTION forge_purge_expired_oauth_codes()
RETURNS void LANGUAGE sql AS $$
    DELETE FROM forge_oauth_codes
    WHERE expires_at < now() - interval '1 hour';
$$;

-- Cluster-aware cache invalidation tracking.
-- Used by the Reactor to propagate invalidation events across nodes
-- when a write occurs on one node and subscriptions exist on another.
CREATE TABLE IF NOT EXISTS forge_invalidations (
    id              BIGSERIAL PRIMARY KEY,
    table_name      TEXT NOT NULL,
    row_id          TEXT,
    operation       TEXT NOT NULL CHECK (operation IN ('INSERT', 'UPDATE', 'DELETE')),
    changed_columns TEXT[],
    node_id         UUID,                   -- originating node
    created_at      TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- Index for efficient polling by other nodes
CREATE INDEX IF NOT EXISTS idx_forge_invalidations_created
    ON forge_invalidations (created_at);

-- Auto-purge old invalidation records (keep only last hour)
CREATE OR REPLACE FUNCTION forge_purge_expired_invalidations()
RETURNS void LANGUAGE sql AS $$
    DELETE FROM forge_invalidations
    WHERE created_at < now() - interval '1 hour';
$$;

-- Events table (partitioned by month for retention management)
CREATE TABLE IF NOT EXISTS forge_signals_events (
    id              UUID NOT NULL DEFAULT uuidv7(),
    event_type      VARCHAR(32) NOT NULL,
    event_name      VARCHAR(255),
    correlation_id  VARCHAR(64),
    session_id      UUID,
    visitor_id      VARCHAR(64),
    user_id         UUID,
    tenant_id       UUID,
    properties      JSONB NOT NULL DEFAULT '{}',

    -- Page context
    page_url        TEXT,
    referrer        TEXT,

    -- RPC fields (denormalized for dashboard query performance)
    function_name   VARCHAR(255),
    function_kind   VARCHAR(32),
    duration_ms     INTEGER,
    status          VARCHAR(32),

    -- Diagnostics
    error_message   TEXT,
    error_stack     TEXT,
    error_context   JSONB,

    -- Client context
    client_ip       TEXT,
    user_agent      TEXT,
    device_type     VARCHAR(16),
    browser         VARCHAR(64),
    os              VARCHAR(64),

    -- Acquisition
    utm_source      VARCHAR(255),
    utm_medium      VARCHAR(255),
    utm_campaign    VARCHAR(255),
    utm_term        VARCHAR(255),
    utm_content     VARCHAR(255),

    -- Geo (derived from IP)
    country         VARCHAR(8),
    city            VARCHAR(128),

    -- Classification
    is_bot          BOOLEAN NOT NULL DEFAULT FALSE,

    -- Forward-compat slot. Future-versioned fields land here without ALTER TABLE.
    metadata        JSONB NOT NULL DEFAULT '{}',

    timestamp       TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    PRIMARY KEY (id, timestamp)
) PARTITION BY RANGE (timestamp);

-- Create initial partition for the current month and next month.
-- The runtime creates future partitions via a daily cron.
DO $$
DECLARE
    current_start DATE := date_trunc('month', CURRENT_DATE);
    next_start DATE := current_start + interval '1 month';
    after_next DATE := next_start + interval '1 month';
    partition_name TEXT;
BEGIN
    partition_name := 'forge_signals_events_' || to_char(current_start, 'YYYY_MM');
    EXECUTE format(
        'CREATE TABLE IF NOT EXISTS %I PARTITION OF forge_signals_events
         FOR VALUES FROM (%L) TO (%L)',
        partition_name, current_start, next_start
    );

    partition_name := 'forge_signals_events_' || to_char(next_start, 'YYYY_MM');
    EXECUTE format(
        'CREATE TABLE IF NOT EXISTS %I PARTITION OF forge_signals_events
         FOR VALUES FROM (%L) TO (%L)',
        partition_name, next_start, after_next
    );
END $$;

-- Default partition catches events outside explicit partition ranges
CREATE TABLE IF NOT EXISTS forge_signals_events_default
    PARTITION OF forge_signals_events DEFAULT;

-- Indexes on the parent (inherited by all partitions)
CREATE INDEX IF NOT EXISTS idx_signals_events_timestamp
    ON forge_signals_events (timestamp DESC);

CREATE INDEX IF NOT EXISTS idx_signals_events_user
    ON forge_signals_events (user_id, timestamp DESC)
    WHERE user_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_signals_events_session
    ON forge_signals_events (session_id, timestamp)
    WHERE session_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_signals_events_type
    ON forge_signals_events (event_type, timestamp DESC);

CREATE INDEX IF NOT EXISTS idx_signals_events_function
    ON forge_signals_events (function_name, timestamp DESC)
    WHERE function_name IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_signals_events_correlation
    ON forge_signals_events (correlation_id)
    WHERE correlation_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_signals_events_properties
    ON forge_signals_events USING GIN (properties);

-- Session tracking (server-side, no client cookies)
CREATE TABLE IF NOT EXISTS forge_signals_sessions (
    id                  UUID PRIMARY KEY,
    visitor_id          VARCHAR(64),
    user_id             UUID,
    tenant_id           UUID,

    -- Timeline
    started_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_activity_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    ended_at            TIMESTAMPTZ,
    duration_secs       INTEGER,

    -- Counters (updated incrementally)
    event_count         INTEGER NOT NULL DEFAULT 0,
    page_view_count     INTEGER NOT NULL DEFAULT 0,
    rpc_call_count      INTEGER NOT NULL DEFAULT 0,
    error_count         INTEGER NOT NULL DEFAULT 0,

    -- Navigation
    entry_page          TEXT,
    exit_page           TEXT,

    -- Acquisition (first-touch for this session)
    referrer            TEXT,
    referrer_domain     VARCHAR(255),
    utm_source          VARCHAR(255),
    utm_medium          VARCHAR(255),
    utm_campaign        VARCHAR(255),

    -- Device
    user_agent          TEXT,
    device_type         VARCHAR(16),
    browser             VARCHAR(64),
    os                  VARCHAR(64),
    client_ip           TEXT,

    -- Geo
    country             VARCHAR(8),
    city                VARCHAR(128),

    -- Classification
    is_bot              BOOLEAN NOT NULL DEFAULT FALSE,
    is_bounce           BOOLEAN NOT NULL DEFAULT TRUE
);

CREATE INDEX IF NOT EXISTS idx_signals_sessions_started
    ON forge_signals_sessions (started_at DESC);

CREATE INDEX IF NOT EXISTS idx_signals_sessions_user
    ON forge_signals_sessions (user_id, started_at DESC)
    WHERE user_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_signals_sessions_referrer
    ON forge_signals_sessions (referrer_domain, started_at DESC)
    WHERE referrer_domain IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_signals_sessions_activity
    ON forge_signals_sessions (last_activity_at)
    WHERE ended_at IS NULL;

-- Analytics user profiles (created on identify())
CREATE TABLE IF NOT EXISTS forge_signals_users (
    id                      UUID PRIMARY KEY,
    first_seen_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen_at            TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    -- Acquisition (first-touch attribution)
    first_referrer          TEXT,
    first_referrer_domain   VARCHAR(255),
    first_utm_source        VARCHAR(255),
    first_utm_medium        VARCHAR(255),
    first_utm_campaign      VARCHAR(255),

    -- Aggregates
    total_sessions          INTEGER NOT NULL DEFAULT 0,
    total_events            INTEGER NOT NULL DEFAULT 0,

    -- Custom traits from identify() calls
    traits                  JSONB NOT NULL DEFAULT '{}',

    created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Materialized views for dashboard performance.
-- Refreshed concurrently every 5 minutes by the runtime.

CREATE MATERIALIZED VIEW IF NOT EXISTS forge_signals_daily_stats AS
SELECT
    date_trunc('day', timestamp)::date AS day,
    COUNT(DISTINCT user_id) FILTER (WHERE user_id IS NOT NULL) AS unique_users,
    COUNT(DISTINCT session_id) FILTER (WHERE session_id IS NOT NULL) AS unique_sessions,
    COUNT(*) AS total_events,
    COUNT(*) FILTER (WHERE event_type = 'error') AS total_errors,
    COUNT(*) FILTER (WHERE is_bot = TRUE) AS bot_events,
    COUNT(*) FILTER (WHERE is_bot = FALSE) AS human_events
FROM forge_signals_events
WHERE timestamp > NOW() - INTERVAL '90 days'
GROUP BY 1;

CREATE UNIQUE INDEX IF NOT EXISTS idx_signals_daily_stats_day
    ON forge_signals_daily_stats (day);

CREATE MATERIALIZED VIEW IF NOT EXISTS forge_signals_retention AS
WITH cohorts AS (
    SELECT id AS user_id, date_trunc('week', first_seen_at)::date AS cohort_week
    FROM forge_signals_users
),
activity AS (
    SELECT DISTINCT user_id, date_trunc('week', timestamp)::date AS activity_week
    FROM forge_signals_events
    WHERE user_id IS NOT NULL
)
SELECT
    c.cohort_week,
    EXTRACT(DAYS FROM (a.activity_week::timestamp - c.cohort_week::timestamp))::integer / 7 AS weeks_since,
    COUNT(DISTINCT a.user_id) AS active_users,
    (SELECT COUNT(*) FROM cohorts c2 WHERE c2.cohort_week = c.cohort_week) AS cohort_size
FROM cohorts c
JOIN activity a ON c.user_id = a.user_id
GROUP BY c.cohort_week, weeks_since;

CREATE UNIQUE INDEX IF NOT EXISTS idx_signals_retention_key
    ON forge_signals_retention (cohort_week, weeks_since);

CREATE MATERIALIZED VIEW IF NOT EXISTS forge_signals_function_stats AS
SELECT
    function_name,
    function_kind,
    date_trunc('hour', timestamp) AS hour,
    COUNT(*) AS call_count,
    COUNT(*) FILTER (WHERE status = 'success') AS success_count,
    COUNT(*) FILTER (WHERE status = 'error') AS error_count,
    AVG(duration_ms)::integer AS avg_duration_ms,
    PERCENTILE_CONT(0.95) WITHIN GROUP (ORDER BY duration_ms)::integer AS p95_duration_ms,
    PERCENTILE_CONT(0.99) WITHIN GROUP (ORDER BY duration_ms)::integer AS p99_duration_ms
FROM forge_signals_events
WHERE event_type = 'rpc_call'
  AND function_name IS NOT NULL
  AND timestamp > NOW() - INTERVAL '30 days'
GROUP BY 1, 2, 3;

CREATE UNIQUE INDEX IF NOT EXISTS idx_signals_function_stats_key
    ON forge_signals_function_stats (function_name, function_kind, hour);

-- Partition management helpers

CREATE OR REPLACE FUNCTION forge_signals_ensure_partition(target_date DATE)
RETURNS void LANGUAGE plpgsql AS $$
DECLARE
    month_start DATE := date_trunc('month', target_date);
    month_end DATE := month_start + interval '1 month';
    partition_name TEXT := 'forge_signals_events_' || to_char(month_start, 'YYYY_MM');
BEGIN
    IF NOT EXISTS (
        SELECT 1 FROM pg_class WHERE relname = partition_name
    ) THEN
        EXECUTE format(
            'CREATE TABLE IF NOT EXISTS %I PARTITION OF forge_signals_events
             FOR VALUES FROM (%L) TO (%L)',
            partition_name, month_start, month_end
        );
    END IF;
END $$;

CREATE OR REPLACE FUNCTION forge_signals_drop_old_partitions(retention_days INTEGER)
RETURNS integer LANGUAGE plpgsql AS $$
DECLARE
    cutoff DATE := CURRENT_DATE - (retention_days || ' days')::interval;
    rec RECORD;
    dropped INTEGER := 0;
BEGIN
    FOR rec IN
        SELECT inhrelid::regclass::text AS partition_name
        FROM pg_inherits
        WHERE inhparent = 'forge_signals_events'::regclass
        AND inhrelid::regclass::text != 'forge_signals_events_default'
    LOOP
        -- Extract the YYYY_MM from partition name and check if it's before cutoff
        IF to_date(
            substring(rec.partition_name FROM 'forge_signals_events_(\d{4}_\d{2})'),
            'YYYY_MM'
        ) + interval '1 month' < cutoff THEN
            EXECUTE format('DROP TABLE IF EXISTS %I', rec.partition_name);
            dropped := dropped + 1;
        END IF;
    END LOOP;
    RETURN dropped;
END $$;

CREATE OR REPLACE FUNCTION forge_signals_refresh_views()
RETURNS void LANGUAGE plpgsql AS $$
BEGIN
    REFRESH MATERIALIZED VIEW CONCURRENTLY forge_signals_daily_stats;
    REFRESH MATERIALIZED VIEW CONCURRENTLY forge_signals_retention;
    REFRESH MATERIALIZED VIEW CONCURRENTLY forge_signals_function_stats;
END $$;

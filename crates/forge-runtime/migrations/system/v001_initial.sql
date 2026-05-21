-- Forge system schema. Applied before any user migration. Future schema
-- changes get their own __forge_vNNN file.

-- ---------------------------------------------------------------------------
-- Cluster
-- ---------------------------------------------------------------------------

-- UNLOGGED: transient cluster state that the runtime rebuilds on startup.
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

-- Leader visibility table. The advisory lock is the source of truth; this row
-- exists so operators don't have to inspect pg_locks.
CREATE UNLOGGED TABLE IF NOT EXISTS forge_leaders (
    role VARCHAR(64) PRIMARY KEY,
    node_id UUID NOT NULL,
    acquired_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    lease_until TIMESTAMPTZ NOT NULL
);

-- ---------------------------------------------------------------------------
-- Key-value store
-- ---------------------------------------------------------------------------

-- Reach for this from user code via the `kv` context method.
CREATE TABLE IF NOT EXISTS forge_kv (
    key TEXT PRIMARY KEY,
    value BYTEA NOT NULL,
    expires_at TIMESTAMPTZ,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_forge_kv_expires
    ON forge_kv(expires_at)
    WHERE expires_at IS NOT NULL;

-- Split from forge_kv so increments skip bytea decode/encode.
CREATE TABLE IF NOT EXISTS forge_kv_counters (
    key TEXT PRIMARY KEY,
    value BIGINT NOT NULL DEFAULT 0,
    expires_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_forge_kv_counters_expires
    ON forge_kv_counters(expires_at)
    WHERE expires_at IS NOT NULL;

-- ---------------------------------------------------------------------------
-- Jobs
-- ---------------------------------------------------------------------------

-- Hot table: every worker poll scans it with FOR UPDATE SKIP LOCKED. Terminal
-- rows are archived to forge_jobs_history by forge_archive_completed_jobs().
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
    tenant_id UUID,
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
    -- Forward-compat slot for fields that don't justify an ALTER TABLE.
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

CREATE INDEX IF NOT EXISTS idx_forge_jobs_owner_status
    ON forge_jobs(owner_subject, status)
    WHERE owner_subject IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_forge_jobs_tenant
    ON forge_jobs(tenant_id)
    WHERE tenant_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_forge_jobs_expires
    ON forge_jobs(expires_at)
    WHERE expires_at IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_forge_jobs_input_gin
    ON forge_jobs USING GIN (input);

CREATE INDEX IF NOT EXISTS idx_forge_jobs_output_gin
    ON forge_jobs USING GIN (output)
    WHERE output IS NOT NULL;

-- Identical shape to forge_jobs plus archived_at, so the archive function can
-- INSERT ... SELECT j.*. Append-only — no reactivity trigger.
CREATE TABLE IF NOT EXISTS forge_jobs_history (
    id                  UUID PRIMARY KEY,
    job_type            VARCHAR(255) NOT NULL,
    queue               VARCHAR(64) NOT NULL DEFAULT 'default',
    kind                VARCHAR(32) NOT NULL DEFAULT 'normal',
    input               JSONB NOT NULL DEFAULT '{}',
    output              JSONB,
    job_context         JSONB NOT NULL DEFAULT '{}',
    status              VARCHAR(32) NOT NULL DEFAULT 'pending',
    priority            INTEGER NOT NULL DEFAULT 50,
    attempts            INTEGER NOT NULL DEFAULT 0,
    max_attempts        INTEGER NOT NULL DEFAULT 3,
    last_error          TEXT,
    progress_percent    INTEGER DEFAULT 0,
    progress_message    TEXT,
    worker_capability   VARCHAR(255),
    worker_id           UUID,
    idempotency_key     VARCHAR(255),
    owner_subject       TEXT,
    tenant_id           UUID,
    scheduled_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    claimed_at          TIMESTAMPTZ,
    started_at          TIMESTAMPTZ,
    completed_at        TIMESTAMPTZ,
    failed_at           TIMESTAMPTZ,
    cancel_requested_at TIMESTAMPTZ,
    cancelled_at        TIMESTAMPTZ,
    cancel_reason       TEXT,
    last_heartbeat      TIMESTAMPTZ,
    expires_at          TIMESTAMPTZ,
    metadata            JSONB NOT NULL DEFAULT '{}',
    archived_at         TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_forge_jobs_history_archived_at
    ON forge_jobs_history (archived_at DESC);

CREATE INDEX IF NOT EXISTS idx_forge_jobs_history_owner_subject
    ON forge_jobs_history (owner_subject, archived_at DESC)
    WHERE owner_subject IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_forge_jobs_history_status_completed
    ON forge_jobs_history (status, completed_at DESC)
    WHERE completed_at IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_forge_jobs_history_type_status
    ON forge_jobs_history (job_type, status, archived_at DESC);

-- ---------------------------------------------------------------------------
-- Cron
-- ---------------------------------------------------------------------------

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

-- ---------------------------------------------------------------------------
-- Workflows
-- ---------------------------------------------------------------------------

-- Upserted on startup; one row per (name, version).
CREATE TABLE IF NOT EXISTS forge_workflow_definitions (
    workflow_name VARCHAR(255) NOT NULL,
    workflow_version VARCHAR(255) NOT NULL,
    workflow_signature VARCHAR(64) NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'active',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (workflow_name, workflow_version)
);

-- Status variants: pending, running, sleeping, waiting, completed, failed.
-- Mutable per-step state lives in forge_workflow_state.
CREATE TABLE IF NOT EXISTS forge_workflow_runs (
    id UUID PRIMARY KEY,
    workflow_name VARCHAR(255) NOT NULL,
    workflow_version VARCHAR(255) NOT NULL,
    workflow_signature VARCHAR(64) NOT NULL,
    owner_subject TEXT,
    tenant_id UUID,
    input JSONB NOT NULL DEFAULT '{}',
    output JSONB,
    status VARCHAR(32) NOT NULL DEFAULT 'pending',
    blocking_reason TEXT,
    resolution_reason TEXT,
    current_step VARCHAR(255),
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    error TEXT,
    trace_id VARCHAR(64),
    suspended_at TIMESTAMPTZ,
    wake_at TIMESTAMPTZ,
    waiting_for_event TEXT,
    event_timeout_at TIMESTAMPTZ,
    -- Operator cancel signal. forge_workflow_runs_cancel_notify wakes the
    -- scheduler so compensation runs immediately, bypassing wake_at.
    cancel_requested_at TIMESTAMPTZ,
    cancel_reason TEXT,
    -- Forward-compat slot for fields that don't justify an ALTER TABLE.
    metadata JSONB NOT NULL DEFAULT '{}'
);

CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_status
    ON forge_workflow_runs(status);

CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_cancel_requested
    ON forge_workflow_runs(cancel_requested_at)
    WHERE cancel_requested_at IS NOT NULL
      AND status IN ('pending', 'running', 'sleeping', 'waiting');

CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_sleeping
    ON forge_workflow_runs(wake_at)
    WHERE status = 'sleeping' AND wake_at IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_waiting_timeout
    ON forge_workflow_runs(event_timeout_at)
    WHERE status = 'waiting' AND event_timeout_at IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_pending
    ON forge_workflow_runs(started_at)
    WHERE status = 'pending';

CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_tenant
    ON forge_workflow_runs(tenant_id)
    WHERE tenant_id IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_owner_subject
    ON forge_workflow_runs(owner_subject)
    WHERE owner_subject IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_name_version
    ON forge_workflow_runs(workflow_name, workflow_version)
    WHERE status NOT IN ('completed', 'failed');

CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_input_gin
    ON forge_workflow_runs USING GIN (input);

CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_output_gin
    ON forge_workflow_runs USING GIN (output)
    WHERE output IS NOT NULL;

-- Split from forge_workflow_runs to bound MVCC bloat: state checkpoints would
-- otherwise dirty the entire wide run row at every suspension. Aggressive
-- autovacuum (set below) keeps the side table tight.
CREATE TABLE IF NOT EXISTS forge_workflow_state (
    run_id              UUID PRIMARY KEY REFERENCES forge_workflow_runs(id) ON DELETE CASCADE,
    saved_state         JSONB NOT NULL DEFAULT '{}',
    compensation_state  JSONB,
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

ALTER TABLE forge_workflow_state SET (
    autovacuum_vacuum_scale_factor   = 0.01,
    autovacuum_analyze_scale_factor  = 0.005
);

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

CREATE INDEX IF NOT EXISTS idx_forge_workflow_events_payload_gin
    ON forge_workflow_events USING GIN (payload)
    WHERE payload IS NOT NULL;

-- Supports ON DELETE on forge_workflow_runs without a child seq-scan.
CREATE INDEX IF NOT EXISTS idx_forge_workflow_events_consumed_by
    ON forge_workflow_events(consumed_by)
    WHERE consumed_by IS NOT NULL;

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

CREATE INDEX IF NOT EXISTS idx_forge_workflow_steps_input_gin
    ON forge_workflow_steps USING GIN (input)
    WHERE input IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_forge_workflow_steps_result_gin
    ON forge_workflow_steps USING GIN (result)
    WHERE result IS NOT NULL;

-- ---------------------------------------------------------------------------
-- Admin
-- ---------------------------------------------------------------------------

-- Append-only audit log for /_api/admin/* actions.
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

-- Operator pause state. Survives restarts; clear by deleting the row.
CREATE TABLE IF NOT EXISTS forge_paused_queues (
    queue_name VARCHAR(255) PRIMARY KEY,
    paused_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    paused_by TEXT,
    reason TEXT
);

-- ---------------------------------------------------------------------------
-- Rate limiting
-- ---------------------------------------------------------------------------

-- UNLOGGED: a hot bucket can be rebuilt cheaply from max_tokens, so we trade
-- durability for write throughput.
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

-- ---------------------------------------------------------------------------
-- Realtime: sessions, subscriptions, change tracking
-- ---------------------------------------------------------------------------

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

CREATE INDEX IF NOT EXISTS idx_forge_subscriptions_args_gin
    ON forge_subscriptions USING GIN (args);

-- Durable log for NOTIFY gap recovery. Nodes track their last-seen seq and
-- replay missed rows on reconnect instead of full re-execution.
CREATE TABLE IF NOT EXISTS forge_change_log (
    seq BIGSERIAL PRIMARY KEY,
    table_name TEXT NOT NULL,
    op TEXT NOT NULL,
    row_id TEXT,
    changed_cols TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_forge_change_log_created
    ON forge_change_log (created_at);

-- ---------------------------------------------------------------------------
-- Reactivity helpers
-- ---------------------------------------------------------------------------

-- format('%I', ...) already escapes, so this is not about injection. It
-- catches authoring mistakes that PG would otherwise silently truncate at the
-- 63-byte NAMEDATALEN limit, or reject far from the call site.
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

-- Row-level: writes to forge_change_log and NOTIFYs forge_changes with payload
--   v1:<table>:<OP>:<row_id>[:<col1,col2,...>]#<seq>
-- The seq lets listeners track position. The "v1:" prefix leaves room for a
-- future schema bump without coordinated cluster restart. pg_notify caps the
-- payload at 8000 bytes; if columns push it past 7900 we drop the column list,
-- so invalidation still fires (without column-level filtering).
CREATE OR REPLACE FUNCTION forge_notify_change() RETURNS TRIGGER AS $$
DECLARE
    row_id TEXT;
    payload TEXT;
    old_json JSONB;
    new_json JSONB;
    changed_cols TEXT[];
    cols_str TEXT;
    log_seq BIGINT;
BEGIN
    IF TG_OP = 'DELETE' THEN
        row_id := COALESCE(OLD.id::TEXT, '');
    ELSE
        row_id := COALESCE(NEW.id::TEXT, '');
    END IF;

    cols_str := '';
    IF TG_OP = 'UPDATE' THEN
        old_json := to_jsonb(OLD);
        new_json := to_jsonb(NEW);
        changed_cols := ARRAY(
            SELECT key FROM jsonb_each(new_json)
            WHERE new_json -> key IS DISTINCT FROM old_json -> key
        );
        IF array_length(changed_cols, 1) > 0 THEN
            cols_str := array_to_string(changed_cols, ',');
        END IF;
    END IF;

    INSERT INTO forge_change_log (table_name, op, row_id, changed_cols)
    VALUES (TG_TABLE_NAME, TG_OP, NULLIF(row_id, ''), NULLIF(cols_str, ''))
    RETURNING seq INTO log_seq;

    payload := 'v1:' || TG_TABLE_NAME || ':' || TG_OP || ':' || row_id;
    IF cols_str != '' THEN
        payload := payload || ':' || cols_str;
    END IF;
    payload := payload || '#' || log_seq::TEXT;

    IF length(payload) > 7900 THEN
        payload := 'v1:' || TG_TABLE_NAME || ':' || TG_OP || ':' || row_id || '#' || log_seq::TEXT;
    END IF;

    PERFORM pg_notify('forge_changes', payload);

    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    ELSE
        RETURN NEW;
    END IF;
END;
$$ LANGUAGE plpgsql;

-- Statement-level: one NOTIFY per DML statement so bulk writes don't saturate
-- the NOTIFY queue. Payload prefix "v1s:" tells listeners to invalidate
-- conservatively (the whole table) since no row id is available.
CREATE OR REPLACE FUNCTION forge_notify_change_statement() RETURNS TRIGGER AS $$
DECLARE
    payload TEXT;
BEGIN
    payload := 'v1s:' || TG_TABLE_NAME || ':' || TG_OP;
    PERFORM pg_notify('forge_changes', payload);
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

-- mode = 'row'       per-row trigger; payload includes column list (default).
-- mode = 'statement' one NOTIFY per statement; for bulk-write tables.
-- mode = 'off'       drop both triggers.
-- Idempotent: both forge_notify_<table> and forge_notify_stmt_<table> are
-- dropped before any new trigger is created.
CREATE OR REPLACE FUNCTION forge_enable_reactivity(table_name TEXT, mode TEXT DEFAULT 'row') RETURNS VOID AS $$
DECLARE
    row_trigger_name  TEXT;
    stmt_trigger_name TEXT;
BEGIN
    PERFORM forge_validate_identifier(table_name);
    row_trigger_name  := 'forge_notify_' || table_name;
    stmt_trigger_name := 'forge_notify_stmt_' || table_name;

    -- A table_name > 44 chars would push the statement trigger over 63 bytes.
    PERFORM forge_validate_identifier(row_trigger_name);
    PERFORM forge_validate_identifier(stmt_trigger_name);

    EXECUTE format('DROP TRIGGER IF EXISTS %I ON %I', row_trigger_name,  table_name);
    EXECUTE format('DROP TRIGGER IF EXISTS %I ON %I', stmt_trigger_name, table_name);

    IF mode = 'row' THEN
        EXECUTE format('
            CREATE TRIGGER %I
            AFTER INSERT OR UPDATE OR DELETE ON %I
            FOR EACH ROW EXECUTE FUNCTION forge_notify_change()
        ', row_trigger_name, table_name);

    ELSIF mode = 'statement' THEN
        EXECUTE format('
            CREATE TRIGGER %I
            AFTER INSERT OR UPDATE OR DELETE ON %I
            FOR EACH STATEMENT EXECUTE FUNCTION forge_notify_change_statement()
        ', stmt_trigger_name, table_name);

    ELSIF mode = 'off' THEN
        NULL;

    ELSE
        RAISE EXCEPTION
            'forge_enable_reactivity: unknown mode %. Valid values: row, statement, off',
            mode;
    END IF;
END;
$$ LANGUAGE plpgsql;

CREATE OR REPLACE FUNCTION forge_disable_reactivity(table_name TEXT) RETURNS VOID AS $$
DECLARE
    row_trigger_name  TEXT;
    stmt_trigger_name TEXT;
BEGIN
    PERFORM forge_validate_identifier(table_name);
    row_trigger_name  := 'forge_notify_' || table_name;
    stmt_trigger_name := 'forge_notify_stmt_' || table_name;
    PERFORM forge_validate_identifier(row_trigger_name);
    PERFORM forge_validate_identifier(stmt_trigger_name);
    EXECUTE format('DROP TRIGGER IF EXISTS %I ON %I', row_trigger_name,  table_name);
    EXECUTE format('DROP TRIGGER IF EXISTS %I ON %I', stmt_trigger_name, table_name);
END;
$$ LANGUAGE plpgsql;

-- ---------------------------------------------------------------------------
-- Workflow and job wakeup triggers
-- ---------------------------------------------------------------------------

-- Wake the scheduler on workflow event arrival so it picks up the event in
-- the next poll cycle instead of waiting on a timer.
CREATE OR REPLACE FUNCTION forge_workflow_event_notify() RETURNS TRIGGER AS $$
BEGIN
    PERFORM pg_notify('forge_workflow_wakeup', NEW.correlation_id);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER forge_workflow_event_notify_trigger
    AFTER INSERT ON forge_workflow_events
    FOR EACH ROW EXECUTE FUNCTION forge_workflow_event_notify();

-- Wake the scheduler when cancel_requested_at flips from NULL so compensation
-- runs immediately instead of waiting on wake_at.
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

-- Cut job dispatch latency from poll_interval to ~0 by NOTIFYing
-- forge_jobs_available on enqueue. Guard against pending -> pending re-enqueue
-- so workers don't spin.
CREATE OR REPLACE FUNCTION forge_notify_job_available() RETURNS TRIGGER AS $$
BEGIN
    IF NEW.status = 'pending'
       AND (TG_OP = 'INSERT' OR OLD.status IS DISTINCT FROM NEW.status)
    THEN
        PERFORM pg_notify('forge_jobs_available', NEW.job_type);
    END IF;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER forge_job_enqueue_notify
    AFTER INSERT OR UPDATE OF status ON forge_jobs
    FOR EACH ROW EXECUTE FUNCTION forge_notify_job_available();

-- Internal job and workflow tables use statement-level reactivity. They still
-- need to drive SSE progress updates, but row-level triggers produce write
-- amplification on every status flip and batch enqueue.
SELECT forge_enable_reactivity('forge_jobs',           'statement');
SELECT forge_enable_reactivity('forge_workflow_runs',  'statement');
SELECT forge_enable_reactivity('forge_workflow_steps', 'statement');

-- ---------------------------------------------------------------------------
-- Daemons
-- ---------------------------------------------------------------------------

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

-- ---------------------------------------------------------------------------
-- Webhooks
-- ---------------------------------------------------------------------------

-- Idempotency + replay. raw_body and raw_headers preserve the delivered
-- payload so `forge webhook replay` works without asking the sender to re-emit.
CREATE TABLE IF NOT EXISTS forge_webhook_events (
    webhook_name VARCHAR(255) NOT NULL,
    idempotency_key VARCHAR(255) NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'claimed',
    attempts INTEGER NOT NULL DEFAULT 1,
    raw_body BYTEA,
    raw_headers JSONB,
    result JSONB,
    error TEXT,
    processed_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (webhook_name, idempotency_key)
);

CREATE INDEX IF NOT EXISTS idx_forge_webhook_events_expires
    ON forge_webhook_events(expires_at);

CREATE INDEX IF NOT EXISTS idx_forge_webhook_events_webhook
    ON forge_webhook_events(webhook_name);

CREATE INDEX IF NOT EXISTS idx_forge_webhook_events_failed
    ON forge_webhook_events (webhook_name, processed_at DESC)
    WHERE status = 'failed';

-- ---------------------------------------------------------------------------
-- Auth: refresh tokens and dynamic OAuth client registration
-- ---------------------------------------------------------------------------

CREATE TABLE IF NOT EXISTS forge_refresh_tokens (
    id           UUID PRIMARY KEY DEFAULT uuidv7(),
    user_id      UUID NOT NULL,
    token_hash   TEXT NOT NULL UNIQUE,
    client_id    TEXT,
    token_family UUID NOT NULL DEFAULT uuidv7(),
    -- Snapshot at sign-in, carried forward on rotation. Refreshes never
    -- silently upgrade or downgrade authority; new roles take effect at the
    -- next sign-in.
    roles        TEXT[] NOT NULL DEFAULT '{}',
    expires_at   TIMESTAMPTZ NOT NULL,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_forge_refresh_tokens_user_id
    ON forge_refresh_tokens (user_id);

CREATE INDEX IF NOT EXISTS idx_forge_refresh_tokens_expires_at
    ON forge_refresh_tokens (expires_at);

CREATE INDEX IF NOT EXISTS idx_refresh_tokens_family
    ON forge_refresh_tokens (token_family);

CREATE TABLE IF NOT EXISTS forge_oauth_clients (
    client_id                  TEXT PRIMARY KEY DEFAULT uuidv7()::TEXT,
    client_name                TEXT,
    redirect_uris              TEXT[] NOT NULL DEFAULT '{}',
    token_endpoint_auth_method TEXT NOT NULL DEFAULT 'none',
    created_at                 TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS forge_oauth_codes (
    code                  TEXT PRIMARY KEY,
    client_id             TEXT NOT NULL REFERENCES forge_oauth_clients(client_id) ON DELETE CASCADE,
    user_id               UUID NOT NULL,
    redirect_uri          TEXT NOT NULL,
    code_challenge        TEXT NOT NULL,
    code_challenge_method TEXT NOT NULL DEFAULT 'S256',
    scopes                TEXT[] NOT NULL DEFAULT '{}',
    expires_at            TIMESTAMPTZ NOT NULL,
    used_at               TIMESTAMPTZ,
    created_at            TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_forge_oauth_codes_expires
    ON forge_oauth_codes(expires_at);

-- Supports ON DELETE CASCADE from forge_oauth_clients without a child seq-scan.
CREATE INDEX IF NOT EXISTS idx_forge_oauth_codes_client_id
    ON forge_oauth_codes(client_id);

-- ---------------------------------------------------------------------------
-- Signals: events, sessions, users, rollups
-- ---------------------------------------------------------------------------

-- Partitioned by month for retention management. A daily cron in the runtime
-- creates future partitions; this migration seeds two so a fresh install can
-- ingest events immediately.
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

    page_url        TEXT,
    referrer        TEXT,

    function_name   VARCHAR(255),
    function_kind   VARCHAR(32),
    duration_ms     INTEGER,
    status          VARCHAR(32),

    error_message   TEXT,
    error_stack     TEXT,
    error_context   JSONB,

    client_ip       TEXT,
    user_agent      TEXT,
    device_type     VARCHAR(16),
    browser         VARCHAR(64),
    os              VARCHAR(64),

    utm_source      VARCHAR(255),
    utm_medium      VARCHAR(255),
    utm_campaign    VARCHAR(255),
    utm_term        VARCHAR(255),
    utm_content     VARCHAR(255),

    country         VARCHAR(8),
    city            VARCHAR(128),

    is_bot          BOOLEAN NOT NULL DEFAULT FALSE,

    -- Forward-compat slot for fields that don't justify an ALTER TABLE.
    metadata        JSONB NOT NULL DEFAULT '{}',

    timestamp       TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    PRIMARY KEY (id, timestamp)
) PARTITION BY RANGE (timestamp);

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

CREATE TABLE IF NOT EXISTS forge_signals_events_default
    PARTITION OF forge_signals_events DEFAULT;

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

-- Server-side session tracking (no client cookies).
CREATE TABLE IF NOT EXISTS forge_signals_sessions (
    id                  UUID PRIMARY KEY,
    visitor_id          VARCHAR(64),
    user_id             UUID,
    tenant_id           UUID,

    started_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_activity_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    ended_at            TIMESTAMPTZ,
    duration_secs       INTEGER,

    event_count         INTEGER NOT NULL DEFAULT 0,
    page_view_count     INTEGER NOT NULL DEFAULT 0,
    rpc_call_count      INTEGER NOT NULL DEFAULT 0,
    error_count         INTEGER NOT NULL DEFAULT 0,

    entry_page          TEXT,
    exit_page           TEXT,

    referrer            TEXT,
    referrer_domain     VARCHAR(255),
    utm_source          VARCHAR(255),
    utm_medium          VARCHAR(255),
    utm_campaign        VARCHAR(255),

    user_agent          TEXT,
    device_type         VARCHAR(16),
    browser             VARCHAR(64),
    os                  VARCHAR(64),
    client_ip           TEXT,

    country             VARCHAR(8),
    city                VARCHAR(128),

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

-- Populated by identify(). Written by a background job in batches (every 60s
-- or on session close) — per-request upserts would serialise on the PK lock.
CREATE TABLE IF NOT EXISTS forge_signals_users (
    id                      UUID PRIMARY KEY,
    first_seen_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_seen_at            TIMESTAMPTZ NOT NULL DEFAULT NOW(),

    first_referrer          TEXT,
    first_referrer_domain   VARCHAR(255),
    first_utm_source        VARCHAR(255),
    first_utm_medium        VARCHAR(255),
    first_utm_campaign      VARCHAR(255),

    total_sessions          INTEGER NOT NULL DEFAULT 0,
    total_events            INTEGER NOT NULL DEFAULT 0,

    traits                  JSONB NOT NULL DEFAULT '{}',

    created_at              TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at              TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- One row per (hour, function_name, status_code). NULL function_name covers
-- non-RPC events (page views, custom events).
CREATE TABLE IF NOT EXISTS forge_signals_hourly_stats (
    hour              TIMESTAMPTZ NOT NULL,
    function_name     VARCHAR(255),
    status_code       INTEGER,
    count             BIGINT NOT NULL DEFAULT 0,
    total_duration_ms BIGINT NOT NULL DEFAULT 0,
    error_count       BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (hour, function_name, status_code)
);

CREATE INDEX IF NOT EXISTS idx_signals_hourly_stats_hour
    ON forge_signals_hourly_stats (hour DESC);

CREATE INDEX IF NOT EXISTS idx_signals_hourly_stats_function
    ON forge_signals_hourly_stats (function_name, hour DESC)
    WHERE function_name IS NOT NULL;

CREATE TABLE IF NOT EXISTS forge_signals_daily_rollup (
    day               DATE NOT NULL,
    function_name     VARCHAR(255),
    total_requests    BIGINT NOT NULL DEFAULT 0,
    unique_visitors   BIGINT NOT NULL DEFAULT 0,
    total_duration_ms BIGINT NOT NULL DEFAULT 0,
    error_count       BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (day, function_name)
);

CREATE INDEX IF NOT EXISTS idx_signals_daily_rollup_day
    ON forge_signals_daily_rollup (day DESC);

CREATE INDEX IF NOT EXISTS idx_signals_daily_rollup_function
    ON forge_signals_daily_rollup (function_name, day DESC)
    WHERE function_name IS NOT NULL;

-- ---------------------------------------------------------------------------
-- Maintenance functions
-- ---------------------------------------------------------------------------

CREATE OR REPLACE FUNCTION forge_cleanup_webhook_events() RETURNS INTEGER AS $$
DECLARE
    deleted_count INTEGER;
BEGIN
    DELETE FROM forge_webhook_events WHERE expires_at < NOW();
    GET DIAGNOSTICS deleted_count = ROW_COUNT;
    RETURN deleted_count;
END;
$$ LANGUAGE plpgsql;

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

-- Move one batch of terminal (completed/failed/dead_letter/cancelled) jobs
-- from forge_jobs to forge_jobs_history. Call repeatedly until it returns 0.
-- Expired pending jobs are pruned by forge_cleanup_expired_jobs() instead.
CREATE OR REPLACE FUNCTION forge_archive_completed_jobs(batch_size INT DEFAULT 1000)
RETURNS INT AS $$
DECLARE
    archived_count INT;
BEGIN
    WITH candidates AS (
        SELECT id
        FROM forge_jobs
        WHERE status IN ('completed', 'failed', 'dead_letter', 'cancelled')
        ORDER BY completed_at ASC NULLS LAST
        LIMIT batch_size
        FOR UPDATE SKIP LOCKED
    ),
    moved AS (
        INSERT INTO forge_jobs_history (
            id, job_type, queue, kind, input, output, job_context,
            status, priority, attempts, max_attempts, last_error,
            progress_percent, progress_message, worker_capability, worker_id,
            idempotency_key, owner_subject, scheduled_at, created_at,
            claimed_at, started_at, completed_at, failed_at,
            cancel_requested_at, cancelled_at, cancel_reason,
            last_heartbeat, expires_at, tenant_id, metadata, archived_at
        )
        SELECT
            j.id, j.job_type, j.queue, j.kind, j.input, j.output, j.job_context,
            j.status, j.priority, j.attempts, j.max_attempts, j.last_error,
            j.progress_percent, j.progress_message, j.worker_capability, j.worker_id,
            j.idempotency_key, j.owner_subject, j.scheduled_at, j.created_at,
            j.claimed_at, j.started_at, j.completed_at, j.failed_at,
            j.cancel_requested_at, j.cancelled_at, j.cancel_reason,
            j.last_heartbeat, j.expires_at, j.tenant_id, j.metadata, NOW()
        FROM forge_jobs j
        INNER JOIN candidates c ON j.id = c.id
        ON CONFLICT (id) DO NOTHING
        RETURNING id
    )
    DELETE FROM forge_jobs
    WHERE id IN (SELECT id FROM moved);

    GET DIAGNOSTICS archived_count = ROW_COUNT;
    RETURN archived_count;
END;
$$ LANGUAGE plpgsql;

-- Keep a 24h tail of expired tokens for audit and error-reporting.
CREATE OR REPLACE FUNCTION forge_purge_expired_refresh_tokens()
RETURNS void LANGUAGE sql AS $$
    DELETE FROM forge_refresh_tokens
    WHERE expires_at < now() - interval '24 hours';
$$;

CREATE OR REPLACE FUNCTION forge_purge_expired_oauth_codes()
RETURNS void LANGUAGE sql AS $$
    DELETE FROM forge_oauth_codes
    WHERE expires_at < now() - interval '1 hour';
$$;

-- Nodes that lag past the retention window fall back to full resync.
CREATE OR REPLACE FUNCTION forge_trim_change_log(retention_interval INTERVAL DEFAULT '1 hour')
RETURNS BIGINT AS $$
DECLARE
    deleted_count BIGINT;
BEGIN
    DELETE FROM forge_change_log
    WHERE created_at < now() - retention_interval;
    GET DIAGNOSTICS deleted_count = ROW_COUNT;
    RETURN deleted_count;
END;
$$ LANGUAGE plpgsql;

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

-- Idempotent: ON CONFLICT adds increments, so a retry after partial failure
-- doesn't double-count.
CREATE OR REPLACE FUNCTION forge_signals_roll_up_hour(target_hour TIMESTAMPTZ)
RETURNS VOID LANGUAGE plpgsql AS $$
DECLARE
    bucket_start TIMESTAMPTZ := date_trunc('hour', target_hour);
    bucket_end   TIMESTAMPTZ := bucket_start + interval '1 hour';
BEGIN
    INSERT INTO forge_signals_hourly_stats (
        hour, function_name, status_code,
        count, total_duration_ms, error_count
    )
    SELECT
        bucket_start,
        function_name,
        CASE status
            WHEN 'success' THEN 200
            WHEN 'error'   THEN 500
            ELSE NULL
        END,
        COUNT(*)                                                        AS count,
        COALESCE(SUM(duration_ms), 0)                                   AS total_duration_ms,
        COUNT(*) FILTER (WHERE event_type = 'error' OR status = 'error') AS error_count
    FROM forge_signals_events
    WHERE timestamp >= bucket_start
      AND timestamp <  bucket_end
    GROUP BY function_name, status
    ON CONFLICT (hour, function_name, status_code) DO UPDATE SET
        count             = forge_signals_hourly_stats.count             + EXCLUDED.count,
        total_duration_ms = forge_signals_hourly_stats.total_duration_ms + EXCLUDED.total_duration_ms,
        error_count       = forge_signals_hourly_stats.error_count       + EXCLUDED.error_count;
END $$;

-- Joins back to forge_signals_events for visitor cardinality (bounded to one
-- partition). Idempotent: ON CONFLICT replaces the day's row.
CREATE OR REPLACE FUNCTION forge_signals_roll_up_day(target_day DATE)
RETURNS VOID LANGUAGE plpgsql AS $$
DECLARE
    day_start TIMESTAMPTZ := target_day::TIMESTAMPTZ;
    day_end   TIMESTAMPTZ := day_start + interval '1 day';
BEGIN
    INSERT INTO forge_signals_daily_rollup (
        day, function_name,
        total_requests, unique_visitors, total_duration_ms, error_count
    )
    SELECT
        target_day,
        h.function_name,
        SUM(h.count) AS total_requests,
        COALESCE((
            SELECT COUNT(DISTINCT visitor_id)
            FROM forge_signals_events e
            WHERE e.timestamp >= day_start
              AND e.timestamp <  day_end
              AND (
                  (h.function_name IS NULL     AND e.function_name IS NULL)
               OR (h.function_name IS NOT NULL AND e.function_name = h.function_name)
              )
              AND e.visitor_id IS NOT NULL
        ), 0)                    AS unique_visitors,
        SUM(h.total_duration_ms) AS total_duration_ms,
        SUM(h.error_count)       AS error_count
    FROM forge_signals_hourly_stats h
    WHERE h.hour >= day_start
      AND h.hour <  day_end
    GROUP BY h.function_name
    ON CONFLICT (day, function_name) DO UPDATE SET
        total_requests    = EXCLUDED.total_requests,
        unique_visitors   = EXCLUDED.unique_visitors,
        total_duration_ms = EXCLUDED.total_duration_ms,
        error_count       = EXCLUDED.error_count;
END $$;

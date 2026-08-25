-- Complete Forge schema.

CREATE TABLE IF NOT EXISTS forge_kv (
    key        TEXT COLLATE "C" PRIMARY KEY,
    value      BYTEA NOT NULL,
    expires_at TIMESTAMPTZ
);
CREATE INDEX IF NOT EXISTS forge_kv_expires_at_idx
    ON forge_kv (expires_at) WHERE expires_at IS NOT NULL;

CREATE TABLE IF NOT EXISTS forge_jobs (
    id                  UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    queue               TEXT NOT NULL,
    payload             BYTEA NOT NULL,
    status              TEXT NOT NULL DEFAULT 'available',
    attempts            INT NOT NULL DEFAULT 0,
    max_attempts        INT NOT NULL,
    available_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    leased_until        TIMESTAMPTZ,
    lease_token         UUID,
    lease_secs          DOUBLE PRECISION,
    enqueued_at         TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at        TIMESTAMPTZ,
    dead_attempts       INT NOT NULL DEFAULT 0,
    failure_summary     TEXT,
    dead_lettered_at    TIMESTAMPTZ,
    traceparent         TEXT,
    tracestate          TEXT,
    baggage             TEXT,
    priority            SMALLINT NOT NULL DEFAULT 1,
    concurrency_key     TEXT,
    cancel_requested_at TIMESTAMPTZ,
    payload_retained    BOOLEAN NOT NULL DEFAULT TRUE,
    CONSTRAINT forge_jobs_status_ck
        CHECK (status IN ('available', 'leased', 'done', 'dead', 'cancelled')),
    CONSTRAINT forge_jobs_traceparent_ck
        CHECK (traceparent IS NULL OR octet_length(traceparent) <= 512),
    CONSTRAINT forge_jobs_tracestate_ck
        CHECK (tracestate IS NULL OR octet_length(tracestate) <= 512),
    CONSTRAINT forge_jobs_baggage_ck
        CHECK (baggage IS NULL OR octet_length(baggage) <= 1024),
    CONSTRAINT forge_jobs_priority_ck CHECK (priority BETWEEN 0 AND 2),
    CONSTRAINT forge_jobs_concurrency_key_ck
        CHECK (concurrency_key IS NULL OR octet_length(concurrency_key) BETWEEN 1 AND 256)
);
CREATE INDEX IF NOT EXISTS forge_jobs_claim_idx
    ON forge_jobs (queue, priority DESC, available_at, enqueued_at, id)
    WHERE status = 'available';
CREATE INDEX IF NOT EXISTS forge_jobs_leased_idx
    ON forge_jobs (leased_until) WHERE status = 'leased';
CREATE INDEX IF NOT EXISTS forge_jobs_done_idx
    ON forge_jobs (completed_at) WHERE status = 'done';
CREATE INDEX IF NOT EXISTS forge_jobs_depth_idx ON forge_jobs (queue, status);
CREATE INDEX IF NOT EXISTS forge_jobs_dead_letter_idx
    ON forge_jobs (queue, dead_lettered_at, id)
    WHERE queue LIKE '%.dlq' AND status IN ('available', 'dead');
CREATE INDEX IF NOT EXISTS forge_jobs_concurrency_idx
    ON forge_jobs (queue, concurrency_key)
    WHERE status = 'leased' AND concurrency_key IS NOT NULL;
CREATE INDEX IF NOT EXISTS forge_jobs_status_list_idx
    ON forge_jobs (queue, enqueued_at, id);
CREATE INDEX IF NOT EXISTS forge_jobs_visible_age_idx
    ON forge_jobs (queue, enqueued_at) WHERE status = 'available';

CREATE TABLE IF NOT EXISTS forge_job_dedup (
    queue      TEXT NOT NULL,
    dedup_id   TEXT NOT NULL,
    job_id     UUID NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (queue, dedup_id)
);
CREATE INDEX IF NOT EXISTS forge_job_dedup_expires_idx ON forge_job_dedup (expires_at);

CREATE TABLE IF NOT EXISTS forge_queue_controls (
    queue      TEXT PRIMARY KEY,
    paused     BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TABLE IF NOT EXISTS forge_queue_counters (
    queue           TEXT PRIMARY KEY,
    started_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    enqueued_total  BIGINT NOT NULL DEFAULT 0 CHECK (enqueued_total >= 0),
    settled_total   BIGINT NOT NULL DEFAULT 0 CHECK (settled_total >= 0),
    dead_total      BIGINT NOT NULL DEFAULT 0 CHECK (dead_total >= 0),
    cancelled_total BIGINT NOT NULL DEFAULT 0 CHECK (cancelled_total >= 0)
);

CREATE TABLE IF NOT EXISTS forge_config (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS forge_flags (
    key  TEXT PRIMARY KEY,
    rule JSONB NOT NULL
);

CREATE TABLE IF NOT EXISTS forge_ratelimit (
    bucket       TEXT NOT NULL,
    subject      TEXT NOT NULL,
    tokens       DOUBLE PRECISION,
    window_start DOUBLE PRECISION,
    cur_count    INTEGER,
    prev_count   INTEGER,
    updated_at   TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    PRIMARY KEY (bucket, subject)
);
CREATE INDEX IF NOT EXISTS forge_ratelimit_updated_idx ON forge_ratelimit (updated_at);

CREATE TABLE IF NOT EXISTS forge_ratelimit_reservations (
    id                   UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    bucket               TEXT NOT NULL,
    subject              TEXT NOT NULL,
    algorithm            TEXT NOT NULL,
    capacity             INTEGER NOT NULL,
    period_secs          DOUBLE PRECISION NOT NULL,
    reserved_units       INTEGER NOT NULL,
    committed_units      INTEGER,
    sliding_window_start DOUBLE PRECISION,
    state                TEXT NOT NULL DEFAULT 'pending',
    expires_at           TIMESTAMPTZ NOT NULL,
    created_at           TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    CONSTRAINT forge_ratelimit_reservation_algorithm_ck
        CHECK (algorithm IN ('token_bucket', 'sliding_window')),
    CONSTRAINT forge_ratelimit_reservation_state_ck
        CHECK (state IN ('pending', 'committed', 'released', 'expired')),
    CONSTRAINT forge_ratelimit_reservation_units_ck
        CHECK (reserved_units > 0 AND committed_units BETWEEN 0 AND reserved_units)
);
CREATE INDEX IF NOT EXISTS forge_ratelimit_reservations_expiry_idx
    ON forge_ratelimit_reservations (expires_at) WHERE state = 'pending';

CREATE TABLE IF NOT EXISTS forge_blobs (
    key                 TEXT COLLATE "C" PRIMARY KEY,
    data                BYTEA NOT NULL,
    content_type        TEXT NOT NULL,
    etag                TEXT NOT NULL,
    metadata            JSONB NOT NULL DEFAULT '{}'::jsonb,
    size                BIGINT NOT NULL,
    last_modified       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    cache_control       TEXT,
    content_disposition TEXT,
    checksum_sha256     TEXT
);
CREATE TABLE IF NOT EXISTS forge_fs_blobs (
    key                 TEXT COLLATE "C" PRIMARY KEY,
    data_ref            TEXT NOT NULL,
    content_type        TEXT NOT NULL,
    etag                TEXT NOT NULL,
    metadata            JSONB NOT NULL DEFAULT '{}'::jsonb,
    size                BIGINT NOT NULL,
    last_modified       TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    cache_control       TEXT,
    content_disposition TEXT,
    checksum_sha256     TEXT
);

CREATE TABLE IF NOT EXISTS forge_sessions (
    token_hash    TEXT PRIMARY KEY,
    user_id       TEXT NOT NULL,
    idle_secs     DOUBLE PRECISION NOT NULL,
    created_at    TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    idle_deadline TIMESTAMPTZ NOT NULL,
    abs_deadline  TIMESTAMPTZ NOT NULL,
    app           TEXT NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS forge_sessions_user_idx ON forge_sessions (user_id);
CREATE INDEX IF NOT EXISTS forge_sessions_expiry_idx ON forge_sessions (idle_deadline);

CREATE TABLE IF NOT EXISTS forge_api_keys (
    id         TEXT PRIMARY KEY,
    key_hash   TEXT NOT NULL UNIQUE,
    owner_id   TEXT NOT NULL,
    label      TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    app        TEXT NOT NULL DEFAULT '',
    expires_at TIMESTAMPTZ,
    scopes     TEXT[] NOT NULL DEFAULT '{}',
    metadata   JSONB NOT NULL DEFAULT '{}'::jsonb,
    CONSTRAINT forge_api_keys_scopes_ck CHECK (cardinality(scopes) <= 32),
    CONSTRAINT forge_api_keys_metadata_ck CHECK (octet_length(metadata::text) <= 4096)
);
CREATE INDEX IF NOT EXISTS forge_api_keys_owner_idx ON forge_api_keys (owner_id);
CREATE INDEX IF NOT EXISTS forge_api_keys_expiry_idx
    ON forge_api_keys (expires_at) WHERE expires_at IS NOT NULL;

CREATE TABLE IF NOT EXISTS forge_auth_tokens (
    token_hash TEXT PRIMARY KEY,
    user_id    TEXT NOT NULL,
    purpose    TEXT NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    expires_at TIMESTAMPTZ NOT NULL,
    app        TEXT NOT NULL DEFAULT '',
    payload    BYTEA NOT NULL DEFAULT ''::bytea,
    CONSTRAINT forge_auth_tokens_payload_ck CHECK (octet_length(payload) <= 4096)
);
CREATE INDEX IF NOT EXISTS forge_auth_tokens_expiry_idx ON forge_auth_tokens (expires_at);

CREATE TABLE IF NOT EXISTS forge_schedules (
    name           TEXT NOT NULL,
    kind           TEXT NOT NULL,
    cron_expr      TEXT,
    target_queue   TEXT NOT NULL,
    payload        BYTEA NOT NULL,
    job_id         UUID,
    next_run       TIMESTAMPTZ NOT NULL,
    last_run       TIMESTAMPTZ,
    created_at     TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    app            TEXT NOT NULL DEFAULT '',
    max_attempts   INT,
    paused         BOOLEAN NOT NULL DEFAULT FALSE,
    misfire_policy TEXT NOT NULL DEFAULT 'run_once',
    max_catch_up   INT NOT NULL DEFAULT 0,
    PRIMARY KEY (name, app),
    CONSTRAINT forge_schedules_kind_ck CHECK (kind IN ('cron', 'at')),
    CONSTRAINT forge_schedules_misfire_policy_ck CHECK (
        (misfire_policy IN ('skip', 'run_once') AND max_catch_up = 0)
        OR (misfire_policy = 'catch_up' AND max_catch_up BETWEEN 1 AND 100)
    )
);
CREATE INDEX IF NOT EXISTS forge_schedules_due_idx ON forge_schedules (next_run);
CREATE INDEX IF NOT EXISTS forge_schedules_active_due_idx
    ON forge_schedules (app, next_run) WHERE paused = FALSE;

CREATE TABLE IF NOT EXISTS forge_scheduler_state (
    app                  TEXT PRIMARY KEY,
    last_successful_tick TIMESTAMPTZ,
    enqueue_failures     BIGINT NOT NULL DEFAULT 0 CHECK (enqueue_failures >= 0)
);

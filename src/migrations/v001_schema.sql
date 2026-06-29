-- Forge schema: the full set of forge_* tables, idempotent in one transaction.

-- kv — lineage: Redis. Contract: docs/contracts/kv.md
-- key uses C collation so prefix scans and scan keyset pagination are byte-wise/exact.
CREATE TABLE IF NOT EXISTS forge_kv (
    key        TEXT COLLATE "C" PRIMARY KEY,
    value      BYTEA       NOT NULL,
    expires_at TIMESTAMPTZ
);

-- Partial index over expirable rows only, so the expiry sweep touches a small index.
CREATE INDEX IF NOT EXISTS forge_kv_expires_at_idx
    ON forge_kv (expires_at)
    WHERE expires_at IS NOT NULL;

-- queue — lineage: AWS SQS. Contract: docs/contracts/queue.md
-- State machine (`status`): available -> leased -> done. Failed delivery (nack or
-- lease expiry) returns to available with `attempts` bumped; on exhaustion the row is
-- re-homed to "<queue>.dlq" as a fresh available job. A job that exhausts its attempts
-- inside a *.dlq queue parks as the terminal 'dead' status instead of re-homing into an
-- unwatched .dlq.dlq. `attempts` counts FAILED deliveries (0 at rest); surfaced attempt
-- number is attempts+1. Dedup lives entirely in forge_job_dedup.
CREATE TABLE IF NOT EXISTS forge_jobs (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    queue        TEXT        NOT NULL,
    payload      BYTEA       NOT NULL,
    status       TEXT        NOT NULL DEFAULT 'available',
    attempts     INT         NOT NULL DEFAULT 0,
    max_attempts INT         NOT NULL,
    backoff      JSONB       NOT NULL,
    available_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    leased_until TIMESTAMPTZ,
    lease_token  UUID,
    -- recorded at claim so heartbeat re-extends by the same visibility timeout
    lease_secs   DOUBLE PRECISION,
    enqueued_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    CONSTRAINT forge_jobs_status_ck
        CHECK (status IN ('available', 'leased', 'done', 'dead'))
);

-- partial index for the dequeue claim path (queue, available_at ORDER BY, SKIP LOCKED)
CREATE INDEX IF NOT EXISTS forge_jobs_claim_idx
    ON forge_jobs (queue, available_at)
    WHERE status = 'available';

-- partial index for lease-expiry reclaim
CREATE INDEX IF NOT EXISTS forge_jobs_leased_idx
    ON forge_jobs (leased_until)
    WHERE status = 'leased';

-- partial index for the retention purge of done rows
CREATE INDEX IF NOT EXISTS forge_jobs_done_idx
    ON forge_jobs (completed_at)
    WHERE status = 'done';

-- depth() filters `WHERE queue = $1` across all statuses; a (queue, status) index
-- serves it directly instead of degrading to a full scan under long retention.
CREATE INDEX IF NOT EXISTS forge_jobs_depth_idx ON forge_jobs (queue, status);

-- Windowed enqueue dedup: a separate table (not a unique index on forge_jobs) makes
-- the window expressible — an entry guards the slot until `expires_at`, after which a
-- new enqueue takes it via ON CONFLICT ... WHERE expires_at <= now().
CREATE TABLE IF NOT EXISTS forge_job_dedup (
    queue      TEXT        NOT NULL,
    dedup_id   TEXT        NOT NULL,
    job_id     UUID        NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (queue, dedup_id)
);

CREATE INDEX IF NOT EXISTS forge_job_dedup_expires_idx
    ON forge_job_dedup (expires_at);

-- config (+ flags) — lineage: 12-factor + OpenFeature. Contract: docs/contracts/config.md
-- Raw string values; resolution layers env > store > default live in app code.
CREATE TABLE IF NOT EXISTS forge_config (
    key   TEXT PRIMARY KEY,
    value TEXT NOT NULL
);

-- Boolean feature-flag rules, stored as JSONB (On/Off/Percent/AllowList).
CREATE TABLE IF NOT EXISTS forge_flags (
    key  TEXT  PRIMARY KEY,
    rule JSONB NOT NULL
);

-- ratelimit — lineage: token bucket / GCRA + IETF RateLimit fields. Contract: docs/contracts/ratelimit.md
-- One row per (bucket, subject). Token-bucket uses `tokens` + `updated_at`; the
-- sliding window uses `window_start` (epoch seconds) + `cur_count`/`prev_count`.
-- The algo-specific columns are nullable; a missing value means "fresh".
CREATE TABLE IF NOT EXISTS forge_ratelimit (
    bucket       TEXT             NOT NULL,
    subject      TEXT             NOT NULL,
    tokens       DOUBLE PRECISION,
    window_start DOUBLE PRECISION,
    cur_count    INTEGER,
    prev_count   INTEGER,
    updated_at   TIMESTAMPTZ      NOT NULL DEFAULT now(),
    PRIMARY KEY (bucket, subject)
);

-- Index for the idle-row sweep.
CREATE INDEX IF NOT EXISTS forge_ratelimit_updated_idx ON forge_ratelimit (updated_at);

-- blob — lineage: AWS S3 API. Contract: docs/contracts/blob.md
-- One row per object. Body in BYTEA (fine at the v1 50 MiB cap). key uses C collation
-- so prefix scans + keyset pagination are byte-wise/exact (matches the S3 list order).
CREATE TABLE IF NOT EXISTS forge_blobs (
    key           TEXT COLLATE "C" PRIMARY KEY,
    data          BYTEA       NOT NULL,
    content_type  TEXT        NOT NULL,
    etag          TEXT        NOT NULL,
    metadata      JSONB       NOT NULL DEFAULT '{}'::jsonb,
    size          BIGINT      NOT NULL,
    last_modified TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- blob (filesystem backend) — Contract: docs/contracts/blob.md
-- Metadata for objects whose BYTES live on a local filesystem directory instead of in
-- BYTEA. Same columns as forge_blobs, minus the body: `data_ref` is the bytes' path
-- relative to the configured blob root. Always created (cheap, empty when the
-- filesystem backend is unused) so the schema is uniform across deployments.
CREATE TABLE IF NOT EXISTS forge_fs_blobs (
    key           TEXT COLLATE "C" PRIMARY KEY,
    data_ref      TEXT        NOT NULL,
    content_type  TEXT        NOT NULL,
    etag          TEXT        NOT NULL,
    metadata      JSONB       NOT NULL DEFAULT '{}'::jsonb,
    size          BIGINT      NOT NULL,
    last_modified TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- auth — lineage: OWASP + PHC + Stripe/GitHub keys. Contract: docs/contracts/auth.md
-- Forge does NOT own users; user_id/owner_id are opaque app strings. Only hashes stored.
-- The `app` column namespaces sessions/keys per app sharing a database (default '' =
-- the unnamespaced app), since a token/id lookup can't be prefix-scoped like kv.

-- Sessions: opaque token stored as its SHA-256, with sliding idle + hard absolute deadlines.
CREATE TABLE IF NOT EXISTS forge_sessions (
    token_hash    TEXT             PRIMARY KEY,
    user_id       TEXT             NOT NULL,
    idle_secs     DOUBLE PRECISION NOT NULL,
    created_at    TIMESTAMPTZ      NOT NULL DEFAULT now(),
    idle_deadline TIMESTAMPTZ      NOT NULL,
    abs_deadline  TIMESTAMPTZ      NOT NULL,
    app           TEXT             NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS forge_sessions_user_idx ON forge_sessions (user_id);
-- Index for the expired-session sweep.
CREATE INDEX IF NOT EXISTS forge_sessions_expiry_idx ON forge_sessions (idle_deadline);

-- API keys: fk_-prefixed secret stored as its SHA-256; id is the non-secret handle.
CREATE TABLE IF NOT EXISTS forge_api_keys (
    id         TEXT        PRIMARY KEY,
    key_hash   TEXT        NOT NULL UNIQUE,
    owner_id   TEXT        NOT NULL,
    label      TEXT        NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    app        TEXT        NOT NULL DEFAULT ''
);
CREATE INDEX IF NOT EXISTS forge_api_keys_owner_idx ON forge_api_keys (owner_id);

-- schedule — lineage: cron + Unix at + k8s CronJob. Contract: docs/contracts/schedule.md
-- A thin layer over the queue: due rows are claimed (FOR UPDATE SKIP LOCKED) and a job
-- is inserted into forge_jobs in the same transaction, so a tick enqueues exactly once.
-- A schedule name is unique per `app` (PK is (name, app)), so two apps can both register
-- a cron named "nightly". `max_attempts`/`backoff` are per-schedule queue opts; NULL
-- means "inherit the queue default", resolved at tick time.
CREATE TABLE IF NOT EXISTS forge_schedules (
    name         TEXT        NOT NULL,
    kind         TEXT        NOT NULL,   -- 'cron' (recurring) | 'at' (one-shot)
    cron_expr    TEXT,                   -- set for kind = 'cron'
    target_queue TEXT        NOT NULL,
    payload      BYTEA       NOT NULL,
    job_id       UUID,                   -- pre-minted JobId for kind = 'at'
    next_run     TIMESTAMPTZ NOT NULL,
    last_run     TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    app          TEXT        NOT NULL DEFAULT '',
    max_attempts INT,
    backoff      JSONB,
    PRIMARY KEY (name, app),
    CONSTRAINT forge_schedules_kind_ck CHECK (kind IN ('cron', 'at'))
);

-- Index for the ticker's "due now" claim.
CREATE INDEX IF NOT EXISTS forge_schedules_due_idx ON forge_schedules (next_run);

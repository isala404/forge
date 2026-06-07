-- queue — lineage: AWS SQS. Contract: docs/contracts/queue.md
-- State machine (`status`): available -> leased -> done. Failed delivery (nack
-- or lease expiry) returns to available with `attempts` bumped; on exhaustion
-- the row is re-homed to "<queue>.dlq" as a fresh available job. `attempts`
-- counts FAILED deliveries (0 at rest); surfaced attempt number is attempts+1.
CREATE TABLE IF NOT EXISTS forge_jobs (
    id           UUID        PRIMARY KEY DEFAULT gen_random_uuid(),
    queue        TEXT        NOT NULL,
    payload      BYTEA       NOT NULL,
    status       TEXT        NOT NULL DEFAULT 'available',
    attempts     INT         NOT NULL DEFAULT 0,
    max_attempts INT         NOT NULL,
    backoff      JSONB       NOT NULL,
    dedup_id     TEXT,
    available_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    leased_until TIMESTAMPTZ,
    lease_token  UUID,
    -- recorded at claim so heartbeat re-extends by the same visibility timeout
    lease_secs   DOUBLE PRECISION,
    enqueued_at  TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    CONSTRAINT forge_jobs_status_ck
        CHECK (status IN ('available', 'leased', 'done'))
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

-- Windowed enqueue dedup: a separate table (not a unique index on forge_jobs)
-- makes the window expressible — an entry guards the slot until `expires_at`,
-- after which a new enqueue takes it via ON CONFLICT ... WHERE expires_at <= now().
CREATE TABLE IF NOT EXISTS forge_job_dedup (
    queue      TEXT        NOT NULL,
    dedup_id   TEXT        NOT NULL,
    job_id     UUID        NOT NULL,
    expires_at TIMESTAMPTZ NOT NULL,
    PRIMARY KEY (queue, dedup_id)
);

CREATE INDEX IF NOT EXISTS forge_job_dedup_expires_idx
    ON forge_job_dedup (expires_at);

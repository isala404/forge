-- Forge transactional outbox row contract v1.
--
-- This table is application-owned. Put it beside the application's domain tables,
-- insert rows with the application's own database driver in the same transaction as
-- the domain change, and grant Forge's runtime role SELECT/UPDATE access. Forge never
-- includes this table in its private schema migrations or ownership manifest.
CREATE TABLE IF NOT EXISTS app_forge_outbox_v1 (
    event_id          UUID PRIMARY KEY,
    namespace         TEXT NOT NULL DEFAULT '',
    destination       TEXT NOT NULL,
    payload           BYTEA NOT NULL,
    delay_seconds     DOUBLE PRECISION NOT NULL DEFAULT 0,
    max_attempts      INTEGER NOT NULL DEFAULT 5,
    dedup_id          TEXT,
    created_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    available_at      TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    dispatch_state    TEXT NOT NULL DEFAULT 'pending',
    dispatch_attempts INTEGER NOT NULL DEFAULT 0,
    claim_token       UUID,
    claimed_until     TIMESTAMPTZ,
    dispatched_at     TIMESTAMPTZ,
    failure_summary   TEXT,
    traceparent       TEXT,
    tracestate        TEXT,
    baggage           TEXT,
    CONSTRAINT app_forge_outbox_v1_state_ck
        CHECK (dispatch_state IN ('pending', 'claimed', 'dispatched')),
    CONSTRAINT app_forge_outbox_v1_delay_ck
        CHECK (delay_seconds >= 0 AND delay_seconds <= 900),
    CONSTRAINT app_forge_outbox_v1_attempts_ck
        CHECK (max_attempts BETWEEN 1 AND 1000),
    CONSTRAINT app_forge_outbox_v1_destination_ck
        CHECK (destination <> ''),
    CONSTRAINT app_forge_outbox_v1_failure_ck
        CHECK (failure_summary IS NULL OR length(failure_summary) <= 512)
);

CREATE INDEX IF NOT EXISTS app_forge_outbox_v1_pending_idx
    ON app_forge_outbox_v1 (available_at, created_at, event_id)
    WHERE dispatch_state IN ('pending', 'claimed');

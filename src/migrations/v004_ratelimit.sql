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

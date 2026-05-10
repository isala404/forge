-- Key-value store for framework internals (rate limits, cache metadata, etc.)
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

-- Atomic counter store for rate limiting and usage tracking.
CREATE TABLE IF NOT EXISTS forge_kv_counters (
    key TEXT PRIMARY KEY,
    value BIGINT NOT NULL DEFAULT 0,
    expires_at TIMESTAMPTZ,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX IF NOT EXISTS idx_forge_kv_counters_expires
    ON forge_kv_counters(expires_at)
    WHERE expires_at IS NOT NULL;

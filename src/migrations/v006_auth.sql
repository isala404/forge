-- auth — lineage: OWASP + PHC + Stripe/GitHub keys. Contract: docs/contracts/auth.md
-- Forge does NOT own users; user_id/owner_id are opaque app strings. Only hashes stored.

-- Sessions: opaque token stored as its SHA-256, with sliding idle + hard absolute deadlines.
CREATE TABLE IF NOT EXISTS forge_sessions (
    token_hash    TEXT             PRIMARY KEY,
    user_id       TEXT             NOT NULL,
    idle_secs     DOUBLE PRECISION NOT NULL,
    created_at    TIMESTAMPTZ      NOT NULL DEFAULT now(),
    idle_deadline TIMESTAMPTZ      NOT NULL,
    abs_deadline  TIMESTAMPTZ      NOT NULL
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
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE INDEX IF NOT EXISTS forge_api_keys_owner_idx ON forge_api_keys (owner_id);

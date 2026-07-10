-- One-time tokens: single-use, purpose-scoped secrets (password reset, email
-- verification, magic links) stored as their SHA-256. Consuming a token deletes
-- its row; `purpose` must match at consume time. Same `app` namespacing rule as
-- sessions and API keys.
CREATE TABLE IF NOT EXISTS forge_auth_tokens (
    token_hash TEXT        PRIMARY KEY,
    user_id    TEXT        NOT NULL,
    purpose    TEXT        NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    expires_at TIMESTAMPTZ NOT NULL,
    app        TEXT        NOT NULL DEFAULT ''
);
-- Index for the expired-token sweep.
CREATE INDEX IF NOT EXISTS forge_auth_tokens_expiry_idx ON forge_auth_tokens (expires_at);

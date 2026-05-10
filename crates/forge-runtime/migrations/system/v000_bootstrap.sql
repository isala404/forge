-- Bootstrap: migration tracking table.
-- Idempotent CREATE TABLE that the runner applies before anything else.
-- Every other migration (system or user) records its version in this table.
CREATE TABLE IF NOT EXISTS forge_system_migrations (
    version VARCHAR(255) PRIMARY KEY,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    checksum VARCHAR(64) NOT NULL
);

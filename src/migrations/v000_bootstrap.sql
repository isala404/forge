-- Migration tracking table; checksum lets the runner skip re-runs and detect post-apply drift.
CREATE TABLE IF NOT EXISTS forge_system_migrations (
    version    VARCHAR(255) PRIMARY KEY,
    applied_at TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    checksum   VARCHAR(64)  NOT NULL
);

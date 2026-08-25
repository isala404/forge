-- Internal migration ledger. This is created before the single tracked schema migration.
CREATE TABLE IF NOT EXISTS forge_system_migrations (
    version    VARCHAR(255) PRIMARY KEY,
    applied_at TIMESTAMPTZ  NOT NULL DEFAULT NOW(),
    checksum   VARCHAR(64)  NOT NULL
);

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

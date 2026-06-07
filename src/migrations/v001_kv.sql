-- kv — lineage: Redis. Contract: docs/contracts/kv.md
-- key uses C collation so prefix scans and scan keyset pagination are byte-wise/exact.
CREATE TABLE IF NOT EXISTS forge_kv (
    key        TEXT COLLATE "C" PRIMARY KEY,
    value      BYTEA       NOT NULL,
    expires_at TIMESTAMPTZ
);

-- Partial index over expirable rows only, so the expiry sweep touches a small index.
CREATE INDEX IF NOT EXISTS forge_kv_expires_at_idx
    ON forge_kv (expires_at)
    WHERE expires_at IS NOT NULL;

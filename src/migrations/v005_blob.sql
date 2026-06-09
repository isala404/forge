-- blob — lineage: AWS S3 API. Contract: docs/contracts/blob.md
-- One row per object. Body in BYTEA (fine at the v1 50 MiB cap). key uses C collation
-- so prefix scans + keyset pagination are byte-wise/exact (matches the S3 list order).
CREATE TABLE IF NOT EXISTS forge_blobs (
    key           TEXT COLLATE "C" PRIMARY KEY,
    data          BYTEA       NOT NULL,
    content_type  TEXT        NOT NULL,
    etag          TEXT        NOT NULL,
    metadata      JSONB       NOT NULL DEFAULT '{}'::jsonb,
    size          BIGINT      NOT NULL,
    last_modified TIMESTAMPTZ NOT NULL DEFAULT now()
);

-- blob (filesystem backend) — lineage: AWS S3 API. Contract: docs/contracts/blob.md
-- Metadata for objects whose BYTES live on a local filesystem directory instead of in
-- BYTEA. Same columns as forge_blobs, minus the body: `data_ref` is the bytes' path
-- relative to the configured blob root. The table is always created (cheap, empty when
-- the filesystem backend is unused) so the schema is uniform across deployments. key
-- uses C collation so prefix scans + keyset pagination are byte-wise/exact, matching
-- forge_blobs and S3 list order.
CREATE TABLE IF NOT EXISTS forge_fs_blobs (
    key           TEXT COLLATE "C" PRIMARY KEY,
    data_ref      TEXT        NOT NULL,
    content_type  TEXT        NOT NULL,
    etag          TEXT        NOT NULL,
    metadata      JSONB       NOT NULL DEFAULT '{}'::jsonb,
    size          BIGINT      NOT NULL,
    last_modified TIMESTAMPTZ NOT NULL DEFAULT now()
);

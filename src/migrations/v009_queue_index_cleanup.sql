-- v009: depth() index + drop the dead dedup_id column (DB-1, DB-2).

-- depth() filters `WHERE queue = $1` across all statuses, but the v002 indexes are
-- all partial on a single status with the wrong leading column, so a busy app's
-- depth() (the thing dashboards poll) degrades to a full-table scan under 7-day
-- retention. A (queue, status) index serves it directly.
CREATE INDEX IF NOT EXISTS forge_jobs_depth_idx ON forge_jobs (queue, status);

-- dedup_id on forge_jobs was never written — dedup lives entirely in
-- forge_job_dedup — so it is a permanently-NULL column. Drop it before the schema
-- freezes rather than ship the debt.
ALTER TABLE forge_jobs DROP COLUMN IF EXISTS dedup_id;

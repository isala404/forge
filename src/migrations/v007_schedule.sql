-- schedule — lineage: cron + Unix at + k8s CronJob. Contract: docs/contracts/schedule.md
-- A thin layer over the queue: due rows are claimed (FOR UPDATE SKIP LOCKED) and a job
-- is inserted into forge_jobs in the same transaction, so a tick enqueues exactly once.
CREATE TABLE IF NOT EXISTS forge_schedules (
    name         TEXT        PRIMARY KEY,
    kind         TEXT        NOT NULL,   -- 'cron' (recurring) | 'at' (one-shot)
    cron_expr    TEXT,                   -- set for kind = 'cron'
    target_queue TEXT        NOT NULL,
    payload      BYTEA       NOT NULL,
    job_id       UUID,                   -- pre-minted JobId for kind = 'at'
    next_run     TIMESTAMPTZ NOT NULL,
    last_run     TIMESTAMPTZ,
    created_at   TIMESTAMPTZ NOT NULL DEFAULT now(),
    CONSTRAINT forge_schedules_kind_ck CHECK (kind IN ('cron', 'at'))
);

-- Index for the ticker's "due now" claim.
CREATE INDEX IF NOT EXISTS forge_schedules_due_idx ON forge_schedules (next_run);

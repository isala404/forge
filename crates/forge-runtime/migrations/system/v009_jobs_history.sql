-- Introduce a history table for terminal jobs to keep the hot forge_jobs table lean.
--
-- forge_jobs is the hottest table in the system: every worker poll issues a
-- FOR UPDATE SKIP LOCKED scan against it, and every status transition updates a
-- row in place. As completed, failed, dead_letter, and cancelled jobs accumulate
-- the table grows and its indexes swell, slowing SKIP LOCKED scans even though
-- terminal rows are never returned by the worker claim query.
--
-- forge_jobs_history holds terminal rows after they are archived. Archiving is
-- explicit and operator-driven (via forge_archive_completed_jobs()), not
-- automatic on completion, so in-flight monitoring queries that read recently
-- completed jobs still work if the archive job runs on a delay.
--
-- The reactivity trigger on forge_jobs was removed in v007. No trigger is added
-- to forge_jobs_history — it is an append-only audit table.

-- History table: same schema as forge_jobs, archived_at appended.
-- Identical column list ensures INSERT INTO ... SELECT * works without an
-- explicit column enumeration if the schema drifts in future migrations.
CREATE TABLE IF NOT EXISTS forge_jobs_history (
    id                  UUID PRIMARY KEY,
    job_type            VARCHAR(255) NOT NULL,
    queue               VARCHAR(64) NOT NULL DEFAULT 'default',
    kind                VARCHAR(32) NOT NULL DEFAULT 'normal',
    input               JSONB NOT NULL DEFAULT '{}',
    output              JSONB,
    job_context         JSONB NOT NULL DEFAULT '{}',
    status              VARCHAR(32) NOT NULL DEFAULT 'pending',
    priority            INTEGER NOT NULL DEFAULT 50,
    attempts            INTEGER NOT NULL DEFAULT 0,
    max_attempts        INTEGER NOT NULL DEFAULT 3,
    last_error          TEXT,
    progress_percent    INTEGER DEFAULT 0,
    progress_message    TEXT,
    worker_capability   VARCHAR(255),
    worker_id           UUID,
    idempotency_key     VARCHAR(255),
    owner_subject       TEXT,
    scheduled_at        TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at          TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    claimed_at          TIMESTAMPTZ,
    started_at          TIMESTAMPTZ,
    completed_at        TIMESTAMPTZ,
    failed_at           TIMESTAMPTZ,
    cancel_requested_at TIMESTAMPTZ,
    cancelled_at        TIMESTAMPTZ,
    cancel_reason       TEXT,
    last_heartbeat      TIMESTAMPTZ,
    expires_at          TIMESTAMPTZ,
    tenant_id           UUID,
    metadata            JSONB NOT NULL DEFAULT '{}',
    -- When the row was moved to history. Not present on forge_jobs.
    archived_at         TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- History lookups are almost always by owner or by terminal status + time range.
CREATE INDEX IF NOT EXISTS idx_forge_jobs_history_archived_at
    ON forge_jobs_history (archived_at DESC);

CREATE INDEX IF NOT EXISTS idx_forge_jobs_history_owner_subject
    ON forge_jobs_history (owner_subject, archived_at DESC)
    WHERE owner_subject IS NOT NULL;

CREATE INDEX IF NOT EXISTS idx_forge_jobs_history_status_completed
    ON forge_jobs_history (status, completed_at DESC)
    WHERE completed_at IS NOT NULL;

-- Composite index to serve the job-type + status dashboard queries efficiently.
CREATE INDEX IF NOT EXISTS idx_forge_jobs_history_type_status
    ON forge_jobs_history (job_type, status, archived_at DESC);

-- Tenant isolation: add tenant_id for multi-tenant deployments.
-- Jobs dispatched on behalf of a tenant carry their tenant_id; the runtime
-- sets PG session GUC 'forge.tenant_id' on the executing connection so that
-- RLS policies on user tables enforce isolation automatically.
ALTER TABLE forge_jobs ADD COLUMN IF NOT EXISTS tenant_id UUID;

CREATE INDEX IF NOT EXISTS idx_forge_jobs_tenant
    ON forge_jobs (tenant_id)
    WHERE tenant_id IS NOT NULL;

-- Additional index on forge_jobs for owner queries that need status filtering.
-- The existing idx_forge_jobs_owner_subject (v001) covers owner lookups but has
-- no status column, forcing a filter step on every call. This composite index
-- serves the common "show me my pending/running jobs" query pattern.
CREATE INDEX IF NOT EXISTS idx_forge_jobs_owner_status
    ON forge_jobs (owner_subject, status)
    WHERE owner_subject IS NOT NULL;

-- Archive function: move a batch of terminal jobs from forge_jobs to
-- forge_jobs_history in a single transaction. Returns the number of rows moved.
--
-- Batch size defaults to 1000 to keep the transaction short and limit lock
-- contention with the worker poll. Call repeatedly (e.g. from a cron job or
-- daemon) until it returns 0 to drain the backlog.
--
-- Terminal statuses: completed, failed, dead_letter, cancelled.
-- Rows that are expired (expires_at < NOW()) but still pending are not moved;
-- forge_cleanup_expired_jobs() handles those.
CREATE OR REPLACE FUNCTION forge_archive_completed_jobs(batch_size INT DEFAULT 1000)
RETURNS INT AS $$
DECLARE
    archived_count INT;
BEGIN
    WITH candidates AS (
        SELECT id
        FROM forge_jobs
        WHERE status IN ('completed', 'failed', 'dead_letter', 'cancelled')
        ORDER BY completed_at ASC NULLS LAST
        LIMIT batch_size
        FOR UPDATE SKIP LOCKED
    ),
    moved AS (
        INSERT INTO forge_jobs_history (
            id, job_type, queue, kind, input, output, job_context,
            status, priority, attempts, max_attempts, last_error,
            progress_percent, progress_message, worker_capability, worker_id,
            idempotency_key, owner_subject, scheduled_at, created_at,
            claimed_at, started_at, completed_at, failed_at,
            cancel_requested_at, cancelled_at, cancel_reason,
            last_heartbeat, expires_at, tenant_id, metadata, archived_at
        )
        SELECT
            j.id, j.job_type, j.queue, j.kind, j.input, j.output, j.job_context,
            j.status, j.priority, j.attempts, j.max_attempts, j.last_error,
            j.progress_percent, j.progress_message, j.worker_capability, j.worker_id,
            j.idempotency_key, j.owner_subject, j.scheduled_at, j.created_at,
            j.claimed_at, j.started_at, j.completed_at, j.failed_at,
            j.cancel_requested_at, j.cancelled_at, j.cancel_reason,
            j.last_heartbeat, j.expires_at, j.tenant_id, j.metadata, NOW()
        FROM forge_jobs j
        INNER JOIN candidates c ON j.id = c.id
        ON CONFLICT (id) DO NOTHING
        RETURNING id
    )
    DELETE FROM forge_jobs
    WHERE id IN (SELECT id FROM moved);

    GET DIAGNOSTICS archived_count = ROW_COUNT;
    RETURN archived_count;
END;
$$ LANGUAGE plpgsql;

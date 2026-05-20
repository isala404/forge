-- Fix partial indexes after v005 split 'waiting' into 'sleeping' + 'waiting'.
-- The old idx_forge_workflow_runs_wake filters on status='waiting' but durable
-- sleeps now use status='sleeping', causing seq scans at scale.

-- Drop the stale index
DROP INDEX IF EXISTS idx_forge_workflow_runs_wake;

-- Sleeping workflows: timer-based wakeup (scheduler polls wake_at)
CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_sleeping
    ON forge_workflow_runs(wake_at)
    WHERE status = 'sleeping' AND wake_at IS NOT NULL;

-- Waiting workflows: event-based with optional timeout
CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_waiting_timeout
    ON forge_workflow_runs(event_timeout_at)
    WHERE status = 'waiting' AND event_timeout_at IS NOT NULL;

-- Pending workflows: ready for initial execution
CREATE INDEX IF NOT EXISTS idx_forge_workflow_runs_pending
    ON forge_workflow_runs(started_at)
    WHERE status = 'pending';

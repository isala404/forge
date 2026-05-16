-- Simplify workflow status from 12 variants to 6:
-- Pending, Running, Sleeping, Waiting, Completed, Failed.

-- Rename 'created' → 'pending'
UPDATE forge_workflow_runs SET status = 'pending' WHERE status = 'created';

-- Collapse blocked/operator statuses into 'failed' with reason preserved
UPDATE forge_workflow_runs SET status = 'failed', error = COALESCE(blocking_reason, resolution_reason, 'blocked: ' || status), completed_at = COALESCE(completed_at, NOW())
WHERE status IN ('blocked_missing_version', 'blocked_signature_mismatch', 'blocked_missing_handler', 'retired_unresumable', 'cancelled_by_operator');

-- Collapse compensation statuses into terminal
UPDATE forge_workflow_runs SET status = 'failed', error = COALESCE(error, 'compensation completed'), completed_at = COALESCE(completed_at, NOW())
WHERE status = 'compensated';
UPDATE forge_workflow_runs SET status = 'failed', error = COALESCE(error, 'failed during compensation'), completed_at = COALESCE(completed_at, NOW())
WHERE status = 'compensating';

-- Split 'waiting' into 'sleeping' (timer) vs 'waiting' (event)
UPDATE forge_workflow_runs SET status = 'sleeping'
WHERE status = 'waiting' AND wake_at IS NOT NULL AND waiting_for_event IS NULL;

-- Update default
ALTER TABLE forge_workflow_runs ALTER COLUMN status SET DEFAULT 'pending';

-- Update ready_workflows view if it exists (scheduler query)
CREATE OR REPLACE FUNCTION forge_ready_workflows_check()
RETURNS void AS $$
BEGIN
  -- No-op; scheduler queries use inline SQL, not views.
  NULL;
END;
$$ LANGUAGE plpgsql;
DROP FUNCTION IF EXISTS forge_ready_workflows_check();

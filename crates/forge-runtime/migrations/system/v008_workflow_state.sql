-- Extract workflow mutable state into a dedicated table to reduce MVCC bloat.
--
-- forge_workflow_runs receives frequent JSONB updates to saved_state and
-- compensation_state at every suspension point. In PostgreSQL, UPDATE always
-- writes a new heap tuple — even when only one column changes — so every
-- state checkpoint creates a dead tuple covering the entire (wide) row.
-- autovacuum reclaims these, but on active workflow tables the dead-tuple
-- churn outpaces default autovacuum thresholds, causing table bloat and
-- index bloat on the surrounding indexes.
--
-- Extracting saved_state and compensation_state into a narrow side table
-- (forge_workflow_state) limits dead-tuple width to ~3 columns and makes
-- autovacuum far more effective. The main forge_workflow_runs row, which
-- changes at coarse workflow lifecycle events, remains wide but changes
-- infrequently relative to step-level state updates.
--
-- No reactivity trigger is added to forge_workflow_state. Step-level state
-- updates are internal; no user query subscribes to them via the invalidation
-- engine, and adding a trigger here would recreate the write-amplification
-- problem this migration is solving.

-- Side table: one row per workflow run, updated on every state checkpoint.
CREATE TABLE IF NOT EXISTS forge_workflow_state (
    run_id              UUID PRIMARY KEY REFERENCES forge_workflow_runs(id) ON DELETE CASCADE,
    saved_state         JSONB NOT NULL DEFAULT '{}',
    compensation_state  JSONB,
    updated_at          TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

-- Aggressive autovacuum: vacuum after 1% dead tuples (vs default 20%),
-- analyze after 0.5%. This keeps bloat bounded even under sustained update
-- load from long-running workflows with frequent suspension points.
ALTER TABLE forge_workflow_state SET (
    autovacuum_vacuum_scale_factor   = 0.01,
    autovacuum_analyze_scale_factor  = 0.005
);

-- Migrate existing state from the main table.
-- Rows with a non-empty saved_state or a non-null compensation_state get a
-- corresponding state row. Runs where both are empty/null need no state row;
-- the executor already handles a missing state row as equivalent to defaults.
INSERT INTO forge_workflow_state (run_id, saved_state, compensation_state, updated_at)
SELECT
    id,
    COALESCE(saved_state, '{}'),
    compensation_state,
    NOW()
FROM forge_workflow_runs
WHERE saved_state IS DISTINCT FROM '{}'::jsonb
   OR compensation_state IS NOT NULL
ON CONFLICT (run_id) DO NOTHING;

-- Drop the columns from the main table.
-- From this point forward the executor reads/writes forge_workflow_state.
ALTER TABLE forge_workflow_runs
    DROP COLUMN IF EXISTS saved_state,
    DROP COLUMN IF EXISTS compensation_state;

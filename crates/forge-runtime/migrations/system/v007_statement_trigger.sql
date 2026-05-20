-- Statement-level reactivity trigger to prevent FOR EACH ROW amplification.
--
-- The row-level forge_notify_change() trigger (v001, updated v002) fires once
-- per affected row. A 50k-row UPDATE produces 50k NOTIFYs and 50k change-log
-- inserts, saturating the NOTIFY queue and introducing write-amplification
-- proportional to batch size. Statement-level triggers fire once per statement
-- regardless of row count.
--
-- Strategy:
--   - Internal tables (forge_jobs, forge_workflow_runs, forge_workflow_steps)
--     do not need reactivity: no user query subscribes to them via the
--     invalidation engine. They are removed from the reactivity firehose.
--   - The new forge_enable_reactivity() replaces the v001 version and adds a
--     'mode' parameter so operators can opt tables into statement-level triggers.
--
-- Payload format (statement-level): v1s:<table>:<OP>
-- The leading "v1s:" prefix is distinct from the row-level "v1:" prefix so
-- a listener can identify statement-level events without ambiguity and handle
-- them conservatively (full-table invalidation rather than row-level filtering).

-- Statement-level notification function.
-- Fires once per DML statement. Emits a single NOTIFY regardless of row count.
-- No change-log write: statement-level triggers cannot access OLD/NEW in PG 16+
-- in a meaningful per-row way, and the change log is row-oriented.
CREATE OR REPLACE FUNCTION forge_notify_change_statement() RETURNS TRIGGER AS $$
DECLARE
    payload TEXT;
BEGIN
    payload := 'v1s:' || TG_TABLE_NAME || ':' || TG_OP;
    PERFORM pg_notify('forge_changes', payload);
    RETURN NULL;
END;
$$ LANGUAGE plpgsql;

-- Drop the v001 single-arg overload before creating the mode-aware version.
-- Without this, PG cannot resolve calls like forge_enable_reactivity('table')
-- because both the 1-param and 2-param-with-default signatures match.
DROP FUNCTION IF EXISTS forge_enable_reactivity(TEXT);

-- Replace forge_enable_reactivity() with a mode-aware version.
--
-- mode = 'row'       (default) — per-row trigger backed by forge_notify_change().
--                    Backward compatible with all v001 callers.
-- mode = 'statement' — statement-level trigger backed by forge_notify_change_statement().
--                    Use for tables with frequent bulk writes (batch jobs, imports).
-- mode = 'off'       — drop all reactivity triggers for the table.
--                    Use for internal tables that no user query subscribes to.
--
-- Both the row trigger (forge_notify_<table>) and the statement trigger
-- (forge_notify_stmt_<table>) are cleaned up before re-creating the requested
-- variant, so switching modes is idempotent.
CREATE OR REPLACE FUNCTION forge_enable_reactivity(table_name TEXT, mode TEXT DEFAULT 'row') RETURNS VOID AS $$
DECLARE
    row_trigger_name  TEXT;
    stmt_trigger_name TEXT;
BEGIN
    PERFORM forge_validate_identifier(table_name);
    row_trigger_name  := 'forge_notify_' || table_name;
    stmt_trigger_name := 'forge_notify_stmt_' || table_name;

    -- Validate derived names. A table_name > 44 chars would push the
    -- statement trigger name over 63 bytes; catch it here.
    PERFORM forge_validate_identifier(row_trigger_name);
    PERFORM forge_validate_identifier(stmt_trigger_name);

    -- Always drop both variants first so the caller doesn't have to reason
    -- about which trigger is currently active.
    EXECUTE format('DROP TRIGGER IF EXISTS %I ON %I', row_trigger_name,  table_name);
    EXECUTE format('DROP TRIGGER IF EXISTS %I ON %I', stmt_trigger_name, table_name);

    IF mode = 'row' THEN
        EXECUTE format('
            CREATE TRIGGER %I
            AFTER INSERT OR UPDATE OR DELETE ON %I
            FOR EACH ROW EXECUTE FUNCTION forge_notify_change()
        ', row_trigger_name, table_name);

    ELSIF mode = 'statement' THEN
        EXECUTE format('
            CREATE TRIGGER %I
            AFTER INSERT OR UPDATE OR DELETE ON %I
            FOR EACH STATEMENT EXECUTE FUNCTION forge_notify_change_statement()
        ', stmt_trigger_name, table_name);

    ELSIF mode = 'off' THEN
        -- Both triggers already dropped above; nothing more to do.
        NULL;

    ELSE
        RAISE EXCEPTION
            'forge_enable_reactivity: unknown mode %. Valid values: row, statement, off',
            mode;
    END IF;
END;
$$ LANGUAGE plpgsql;

-- Switch forge_jobs, forge_workflow_runs, and forge_workflow_steps from row-level
-- to statement-level triggers. These tables still need reactivity (the job and
-- workflow SSE subscription systems consume change events to push progress
-- updates to clients) but row-level triggers produce write-amplification on
-- every status update and batch job enqueue.
SELECT forge_enable_reactivity('forge_jobs',           'statement');
SELECT forge_enable_reactivity('forge_workflow_runs',  'statement');
SELECT forge_enable_reactivity('forge_workflow_steps', 'statement');

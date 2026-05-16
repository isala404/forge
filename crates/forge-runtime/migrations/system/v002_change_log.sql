-- Durable change log for NOTIFY gap recovery.
-- Each node tracks its last-seen seq and replays missed changes on reconnect
-- instead of re-executing all active subscriptions.

CREATE TABLE IF NOT EXISTS forge_change_log (
    seq BIGSERIAL PRIMARY KEY,
    table_name TEXT NOT NULL,
    op TEXT NOT NULL,
    row_id TEXT,
    changed_cols TEXT,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX IF NOT EXISTS idx_forge_change_log_created
    ON forge_change_log (created_at);

-- Update the trigger to write to the change log alongside pg_notify.
-- The seq is included in the NOTIFY payload so listeners can track position.
CREATE OR REPLACE FUNCTION forge_notify_change() RETURNS TRIGGER AS $$
DECLARE
    row_id TEXT;
    payload TEXT;
    old_json JSONB;
    new_json JSONB;
    changed_cols TEXT[];
    cols_str TEXT;
    log_seq BIGINT;
BEGIN
    IF TG_OP = 'DELETE' THEN
        row_id := COALESCE(OLD.id::TEXT, '');
    ELSE
        row_id := COALESCE(NEW.id::TEXT, '');
    END IF;

    cols_str := '';
    IF TG_OP = 'UPDATE' THEN
        old_json := to_jsonb(OLD);
        new_json := to_jsonb(NEW);
        changed_cols := ARRAY(
            SELECT key FROM jsonb_each(new_json)
            WHERE new_json -> key IS DISTINCT FROM old_json -> key
        );
        IF array_length(changed_cols, 1) > 0 THEN
            cols_str := array_to_string(changed_cols, ',');
        END IF;
    END IF;

    INSERT INTO forge_change_log (table_name, op, row_id, changed_cols)
    VALUES (TG_TABLE_NAME, TG_OP, NULLIF(row_id, ''), NULLIF(cols_str, ''))
    RETURNING seq INTO log_seq;

    payload := 'v1:' || TG_TABLE_NAME || ':' || TG_OP || ':' || row_id;
    IF cols_str != '' THEN
        payload := payload || ':' || cols_str;
    END IF;
    payload := payload || '#' || log_seq::TEXT;

    -- pg_notify has an 8000-byte payload limit. If the payload (with columns)
    -- exceeds 7900 bytes, drop the column list. The listener handles missing
    -- columns conservatively (invalidates all groups for the table).
    IF length(payload) > 7900 THEN
        payload := 'v1:' || TG_TABLE_NAME || ':' || TG_OP || ':' || row_id || '#' || log_seq::TEXT;
    END IF;

    PERFORM pg_notify('forge_changes', payload);

    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    ELSE
        RETURN NEW;
    END IF;
END;
$$ LANGUAGE plpgsql;

-- Retention: keep 1 hour of change log by default. Nodes that are down
-- longer than that will fall back to full resync on reconnect.
CREATE OR REPLACE FUNCTION forge_trim_change_log(retention_interval INTERVAL DEFAULT '1 hour')
RETURNS BIGINT AS $$
DECLARE
    deleted_count BIGINT;
BEGIN
    DELETE FROM forge_change_log
    WHERE created_at < now() - retention_interval;
    GET DIAGNOSTICS deleted_count = ROW_COUNT;
    RETURN deleted_count;
END;
$$ LANGUAGE plpgsql;

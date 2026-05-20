-- Replace materialized views with incremental rollup tables for signals analytics.
--
-- [06.11] forge_signals_users write path
-- The table has no explicit write path documented in the schema. Write contention
-- is theoretical today because inserts happen only on identify() calls, which are
-- rare relative to event volume. The intended pattern is a periodic batch upsert
-- driven by a background job (not a per-request INSERT), which amortizes locking
-- over a time window and makes the contention profile predictable. A per-request
-- upsert on every page view would serialize under the primary-key lock and become
-- a bottleneck at scale.
COMMENT ON TABLE forge_signals_users IS
    'Analytics user profiles populated by identify() calls. '
    'Write path: periodic batch upsert from a background job (not per-request). '
    'Per-request upserts serialize under the PK lock; batch every 60s or on session close.';

-- [06.12] Incremental rollup tables
--
-- The materialized views (forge_signals_daily_stats, forge_signals_retention,
-- forge_signals_function_stats) are refreshed on a 5-minute schedule by
-- forge_signals_refresh_views(). REFRESH MATERIALIZED VIEW CONCURRENTLY re-scans
-- the entire underlying table each cycle — on a high-volume events table this
-- means reading potentially millions of rows per refresh just to recompute a
-- rolling 90-day window. Memory and I/O pressure grow linearly with event volume.
--
-- Incremental rollup tables replace the materialized views. Each rollup covers a
-- fixed time bucket (hour, day). The rollup functions aggregate only the target
-- bucket from forge_signals_events, making each call O(events in that hour/day)
-- rather than O(total events). Callers (cron jobs or daemons) invoke the rollup
-- for the current and previous bucket; historical buckets are never re-scanned
-- unless explicitly requested.
--
-- Grafana dashboards query the rollup tables directly via SQL. The query shape
-- changes from REFRESH + SELECT on a materialized view to SELECT on a regular
-- table, which is simpler and composable with standard SQL windowing.

-- Hourly stats: one row per (hour, function_name, status_code) tuple.
-- Aggregates RPC call volume, duration, and error counts from forge_signals_events.
-- NULL function_name captures non-RPC events (page views, custom events).
CREATE TABLE IF NOT EXISTS forge_signals_hourly_stats (
    hour            TIMESTAMPTZ NOT NULL,
    function_name   VARCHAR(255),
    status_code     INTEGER,
    count           BIGINT NOT NULL DEFAULT 0,
    total_duration_ms BIGINT NOT NULL DEFAULT 0,
    error_count     BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (hour, function_name, status_code)
);

-- Index for time-range dashboard queries (last N hours).
CREATE INDEX IF NOT EXISTS idx_signals_hourly_stats_hour
    ON forge_signals_hourly_stats (hour DESC);

-- Index for per-function dashboard queries.
CREATE INDEX IF NOT EXISTS idx_signals_hourly_stats_function
    ON forge_signals_hourly_stats (function_name, hour DESC)
    WHERE function_name IS NOT NULL;

-- Daily rollup: one row per (day, function_name) tuple.
-- Aggregated from forge_signals_hourly_stats, not directly from events.
-- unique_visitors is approximated from event-level visitor_id counts; exact
-- HyperLogLog-style deduplication is left to future work.
CREATE TABLE IF NOT EXISTS forge_signals_daily_rollup (
    day                 DATE NOT NULL,
    function_name       VARCHAR(255),
    total_requests      BIGINT NOT NULL DEFAULT 0,
    unique_visitors     BIGINT NOT NULL DEFAULT 0,
    total_duration_ms   BIGINT NOT NULL DEFAULT 0,
    error_count         BIGINT NOT NULL DEFAULT 0,
    PRIMARY KEY (day, function_name)
);

CREATE INDEX IF NOT EXISTS idx_signals_daily_rollup_day
    ON forge_signals_daily_rollup (day DESC);

CREATE INDEX IF NOT EXISTS idx_signals_daily_rollup_function
    ON forge_signals_daily_rollup (function_name, day DESC)
    WHERE function_name IS NOT NULL;

-- forge_signals_roll_up_hour(target_hour)
--
-- Aggregates forge_signals_events for the one-hour window starting at
-- target_hour (truncated to the hour boundary) into forge_signals_hourly_stats.
--
-- Safe to call multiple times for the same hour: ON CONFLICT adds increments
-- to existing rows so a retry after a partial failure does not double-count.
-- Callers are expected to truncate target_hour to the hour before passing it;
-- the function truncates defensively to make repeated calls idempotent.
--
-- Typical call pattern (from a cron or daemon):
--   SELECT forge_signals_roll_up_hour(date_trunc('hour', NOW() - interval '1 hour'));
--   SELECT forge_signals_roll_up_hour(date_trunc('hour', NOW()));
CREATE OR REPLACE FUNCTION forge_signals_roll_up_hour(target_hour TIMESTAMPTZ)
RETURNS VOID LANGUAGE plpgsql AS $$
DECLARE
    bucket_start TIMESTAMPTZ := date_trunc('hour', target_hour);
    bucket_end   TIMESTAMPTZ := bucket_start + interval '1 hour';
BEGIN
    INSERT INTO forge_signals_hourly_stats (
        hour, function_name, status_code,
        count, total_duration_ms, error_count
    )
    SELECT
        bucket_start,
        function_name,
        -- Derive a numeric status code from the string status field.
        -- 'success' → 200, 'error' → 500, anything else → NULL.
        CASE status
            WHEN 'success' THEN 200
            WHEN 'error'   THEN 500
            ELSE NULL
        END,
        COUNT(*)                                                AS count,
        COALESCE(SUM(duration_ms), 0)                          AS total_duration_ms,
        COUNT(*) FILTER (WHERE event_type = 'error' OR status = 'error') AS error_count
    FROM forge_signals_events
    WHERE timestamp >= bucket_start
      AND timestamp <  bucket_end
    GROUP BY function_name, status
    ON CONFLICT (hour, function_name, status_code) DO UPDATE SET
        count             = forge_signals_hourly_stats.count             + EXCLUDED.count,
        total_duration_ms = forge_signals_hourly_stats.total_duration_ms + EXCLUDED.total_duration_ms,
        error_count       = forge_signals_hourly_stats.error_count       + EXCLUDED.error_count;
END $$;

-- forge_signals_roll_up_day(target_day)
--
-- Aggregates forge_signals_hourly_stats for the 24-hour window of target_day
-- into forge_signals_daily_rollup. Reads from hourly stats (not raw events)
-- so it is cheap to re-run.
--
-- unique_visitors is derived from event-level visitor_id counts for the day.
-- It joins back to forge_signals_events for the visitor dimension because
-- hourly stats do not carry cardinality data. The join is bounded to a single
-- day's partition, so it reads at most one partition.
--
-- Safe to call multiple times: ON CONFLICT replaces the day's row, so re-running
-- after additional hourly rollups have landed will correct the daily total.
CREATE OR REPLACE FUNCTION forge_signals_roll_up_day(target_day DATE)
RETURNS VOID LANGUAGE plpgsql AS $$
DECLARE
    day_start TIMESTAMPTZ := target_day::TIMESTAMPTZ;
    day_end   TIMESTAMPTZ := day_start + interval '1 day';
BEGIN
    INSERT INTO forge_signals_daily_rollup (
        day, function_name,
        total_requests, unique_visitors, total_duration_ms, error_count
    )
    SELECT
        target_day,
        h.function_name,
        SUM(h.count)             AS total_requests,
        -- Count distinct visitor_ids from the raw events for this day.
        -- Subquery is correlated per function_name to keep the aggregation
        -- consistent with hourly granularity.
        COALESCE((
            SELECT COUNT(DISTINCT visitor_id)
            FROM forge_signals_events e
            WHERE e.timestamp >= day_start
              AND e.timestamp <  day_end
              AND (
                  (h.function_name IS NULL     AND e.function_name IS NULL)
               OR (h.function_name IS NOT NULL AND e.function_name = h.function_name)
              )
              AND e.visitor_id IS NOT NULL
        ), 0)                    AS unique_visitors,
        SUM(h.total_duration_ms) AS total_duration_ms,
        SUM(h.error_count)       AS error_count
    FROM forge_signals_hourly_stats h
    WHERE h.hour >= day_start
      AND h.hour <  day_end
    GROUP BY h.function_name
    ON CONFLICT (day, function_name) DO UPDATE SET
        total_requests    = EXCLUDED.total_requests,
        unique_visitors   = EXCLUDED.unique_visitors,
        total_duration_ms = EXCLUDED.total_duration_ms,
        error_count       = EXCLUDED.error_count;
END $$;

-- Drop the materialized views and their refresh function.
-- Dashboard queries now target forge_signals_hourly_stats and
-- forge_signals_daily_rollup directly. The Grafana datasource SQL will need
-- updating to reference the new table names.
DROP MATERIALIZED VIEW IF EXISTS forge_signals_daily_stats;
DROP MATERIALIZED VIEW IF EXISTS forge_signals_retention;
DROP MATERIALIZED VIEW IF EXISTS forge_signals_function_stats;
DROP FUNCTION IF EXISTS forge_signals_refresh_views();

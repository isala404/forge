-- v011: per-app namespace columns (P1-2).
--
-- kv/ratelimit/blob namespace by key prefix, and queue/config by name prefix.
-- Sessions, API keys, and schedules are looked up by token/id (sessions, keys) or
-- listed (schedules), where a prefix can't cleanly scope. They get an explicit
-- `app` column instead, so an app sharing a database can neither validate another
-- app's session/key nor see another app's schedules. Default '' = the unnamespaced
-- app, so existing rows keep working unchanged.
ALTER TABLE forge_sessions ADD COLUMN IF NOT EXISTS app TEXT NOT NULL DEFAULT '';
ALTER TABLE forge_api_keys  ADD COLUMN IF NOT EXISTS app TEXT NOT NULL DEFAULT '';
ALTER TABLE forge_schedules ADD COLUMN IF NOT EXISTS app TEXT NOT NULL DEFAULT '';

-- A schedule name is unique per app, not globally, so two apps can both register
-- a cron named "nightly". The ticker and cancel/list scope by (name, app).
ALTER TABLE forge_schedules DROP CONSTRAINT IF EXISTS forge_schedules_pkey;
ALTER TABLE forge_schedules ADD PRIMARY KEY (name, app);

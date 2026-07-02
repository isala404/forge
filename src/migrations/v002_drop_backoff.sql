-- Drop the always-default `backoff` columns. Retry timing is the queue's default backoff
-- policy, resolved at delivery time and never caller-configurable, so persisting a column
-- that was always the default bought nothing. The internal `Backoff` type stays as the
-- runtime policy; only the dead storage goes.
ALTER TABLE forge_jobs DROP COLUMN backoff;
ALTER TABLE forge_schedules DROP COLUMN backoff;

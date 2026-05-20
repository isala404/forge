-- Store raw webhook payloads for replay capability.
--
-- The existing forge_webhook_events table tracks idempotency state but not the
-- raw request body. Without the body, operators cannot replay a failed webhook
-- delivery — they must ask the sender to re-send, which isn't always possible.
--
-- This migration adds a raw_body column and a result column to support:
--   1. Automatic storage of the raw request body on receipt
--   2. `forge webhook replay <webhook_name> <idempotency_key>` CLI command
--   3. Dead-letter inspection for failed webhooks

ALTER TABLE forge_webhook_events
    ADD COLUMN IF NOT EXISTS raw_body BYTEA,
    ADD COLUMN IF NOT EXISTS raw_headers JSONB,
    ADD COLUMN IF NOT EXISTS result JSONB,
    ADD COLUMN IF NOT EXISTS error TEXT,
    ADD COLUMN IF NOT EXISTS attempts INTEGER NOT NULL DEFAULT 1;

-- Index for finding failed webhooks that may need replay.
CREATE INDEX IF NOT EXISTS idx_forge_webhook_events_failed
    ON forge_webhook_events (webhook_name, processed_at DESC)
    WHERE status = 'failed';

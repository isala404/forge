-- Domain tables for the chatapp example. Forge owns and migrates the forge_* tables;
-- these hold the app's own data. All three backends apply this file on startup.

CREATE TABLE IF NOT EXISTS users (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    username      text NOT NULL UNIQUE,
    display_name  text NOT NULL,
    password_hash text NOT NULL,
    created_at    timestamptz NOT NULL DEFAULT now()
);

CREATE TABLE IF NOT EXISTS chats (
    id            uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    kind          text NOT NULL,                  -- 'direct' | 'group'
    title         text,
    created_by    uuid NOT NULL REFERENCES users(id),
    created_at    timestamptz NOT NULL DEFAULT now(),
    disappearing_seconds integer                  -- null => off
);

CREATE TABLE IF NOT EXISTS chat_members (
    chat_id   uuid NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    user_id   uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    joined_at timestamptz NOT NULL DEFAULT now(),
    PRIMARY KEY (chat_id, user_id)
);
CREATE INDEX IF NOT EXISTS chat_members_user ON chat_members (user_id);

CREATE TABLE IF NOT EXISTS messages (
    id           uuid PRIMARY KEY DEFAULT gen_random_uuid(),
    chat_id      uuid NOT NULL REFERENCES chats(id) ON DELETE CASCADE,
    sender_id    uuid NOT NULL REFERENCES users(id),
    body         text NOT NULL,
    media_key    text,
    content_type text,
    created_at   timestamptz NOT NULL DEFAULT now(),
    expires_at   timestamptz                      -- non-null => disappearing; hard-deleted at this instant
);
CREATE INDEX IF NOT EXISTS messages_chat_created ON messages (chat_id, created_at DESC);

CREATE TABLE IF NOT EXISTS receipts (
    message_id   uuid NOT NULL REFERENCES messages(id) ON DELETE CASCADE,
    user_id      uuid NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    delivered_at timestamptz,
    read_at      timestamptz,
    PRIMARY KEY (message_id, user_id)
);

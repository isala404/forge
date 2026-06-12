"""Queries against the unprefixed chat tables over an asyncpg pool. Forge owns the
forge_* tables; this module owns users/chats/chat_members/messages/receipts.

Read paths used behind DataLoaders take id lists and return batched rows, so a query
selecting N messages issues one query per relational field, not N."""

from __future__ import annotations

import uuid
from datetime import datetime
from pathlib import Path

import asyncpg

MIGRATIONS = Path(__file__).resolve().parent / "migrations.sql"


async def migrate(pool: asyncpg.Pool) -> None:
    sql = MIGRATIONS.read_text()
    async with pool.acquire() as conn:
        await conn.execute(sql)


async def create_user(pool, username, display_name, password_hash) -> uuid.UUID:
    uid = uuid.uuid4()
    await pool.execute(
        "INSERT INTO users (id, username, display_name, password_hash) VALUES ($1,$2,$3,$4)",
        uid, username, display_name, password_hash,
    )
    return uid


async def username_taken(pool, username) -> bool:
    return await pool.fetchval(
        "SELECT EXISTS(SELECT 1 FROM users WHERE username = $1)", username
    )


async def credentials(pool, username):
    row = await pool.fetchrow(
        "SELECT id, password_hash FROM users WHERE username = $1", username
    )
    return (row["id"], row["password_hash"]) if row else None


async def set_password_hash(pool, user_id, password_hash) -> None:
    await pool.execute(
        "UPDATE users SET password_hash = $2 WHERE id = $1", user_id, password_hash
    )


async def user_by_username(pool, username):
    return await pool.fetchrow(
        "SELECT id, username, display_name FROM users WHERE username = $1", username
    )


async def chats_for_user(pool, user_id):
    return await pool.fetch(
        "SELECT c.id, c.kind, c.title, c.disappearing_seconds, "
        "       COALESCE(m.last_at, c.created_at) AS activity_at "
        "FROM chats c "
        "JOIN chat_members cm ON cm.chat_id = c.id AND cm.user_id = $1 "
        "LEFT JOIN ("
        "  SELECT chat_id, max(created_at) AS last_at FROM messages "
        "  WHERE expires_at IS NULL OR expires_at > now() GROUP BY chat_id"
        ") m ON m.chat_id = c.id "
        "ORDER BY activity_at DESC",
        user_id,
    )


async def chat(pool, cid):
    return await pool.fetchrow(
        "SELECT id, kind, title, disappearing_seconds FROM chats WHERE id = $1", cid
    )


async def is_member(pool, chat_id, user_id) -> bool:
    return await pool.fetchval(
        "SELECT EXISTS(SELECT 1 FROM chat_members WHERE chat_id = $1 AND user_id = $2)",
        chat_id, user_id,
    )


async def create_chat(pool, kind, title, created_by, member_ids) -> uuid.UUID:
    cid = uuid.uuid4()
    async with pool.acquire() as conn, conn.transaction():
        await conn.execute(
            "INSERT INTO chats (id, kind, title, created_by) VALUES ($1,$2,$3,$4)",
            cid, kind, title, created_by,
        )
        await conn.executemany(
            "INSERT INTO chat_members (chat_id, user_id) VALUES ($1,$2) "
            "ON CONFLICT (chat_id, user_id) DO NOTHING",
            [(cid, mid) for mid in member_ids],
        )
    return cid


async def add_member(pool, chat_id, user_id) -> None:
    await pool.execute(
        "INSERT INTO chat_members (chat_id, user_id) VALUES ($1,$2) "
        "ON CONFLICT (chat_id, user_id) DO NOTHING",
        chat_id, user_id,
    )


async def insert_message_with_receipts(
    pool, mid, chat_id, sender_id, body, media_key, content_type, expires_at
) -> None:
    """Insert the message and its other-member receipts atomically: a crash between
    the two must not leave a message with no receipts (an unread silently lost)."""
    async with pool.acquire() as conn, conn.transaction():
        await conn.execute(
            "INSERT INTO messages "
            "(id, chat_id, sender_id, body, media_key, content_type, expires_at) "
            "VALUES ($1,$2,$3,$4,$5,$6,$7)",
            mid, chat_id, sender_id, body, media_key, content_type, expires_at,
        )
        rows = await conn.fetch(
            "SELECT user_id FROM chat_members WHERE chat_id = $1 AND user_id <> $2",
            chat_id, sender_id,
        )
        recipients = [r["user_id"] for r in rows]
        if recipients:
            await conn.executemany(
                "INSERT INTO receipts (message_id, user_id) VALUES ($1,$2) "
                "ON CONFLICT (message_id, user_id) DO NOTHING",
                [(mid, uid) for uid in recipients],
            )


async def message(pool, mid):
    return await pool.fetchrow(
        "SELECT id, chat_id, sender_id, body, media_key, content_type, created_at "
        "FROM messages WHERE id = $1 "
        "  AND (expires_at IS NULL OR expires_at > now())",
        mid,
    )


async def list_messages(pool, chat_id, before: datetime | None, limit: int):
    return await pool.fetch(
        "SELECT id, chat_id, sender_id, body, media_key, content_type, created_at "
        "FROM messages "
        "WHERE chat_id = $1 "
        "  AND (expires_at IS NULL OR expires_at > now()) "
        "  AND ($2::timestamptz IS NULL OR created_at < $2) "
        "ORDER BY created_at DESC LIMIT $3",
        chat_id, before, limit,
    )


async def mark_delivered(pool, message_id, user_id) -> bool:
    status = await pool.execute(
        "UPDATE receipts SET delivered_at = now() "
        "WHERE message_id = $1 AND user_id = $2 AND delivered_at IS NULL",
        message_id, user_id,
    )
    return status.rsplit(" ", 1)[-1] != "0"


async def mark_read(pool, chat_id, message_id, user_id) -> bool:
    """Flip the receipt's read_at, scoped to a message that belongs to chat_id so a
    member of one chat cannot mark a receipt on another chat's message. Returns whether
    a row was updated."""
    status = await pool.execute(
        "UPDATE receipts SET read_at = COALESCE(read_at, now()), "
        "delivered_at = COALESCE(delivered_at, now()) "
        "WHERE message_id = $1 AND user_id = $2 "
        "AND EXISTS (SELECT 1 FROM messages WHERE id = $1 AND chat_id = $3)",
        message_id, user_id, chat_id,
    )
    return status.rsplit(" ", 1)[-1] != "0"


async def other_member_ids(pool, chat_id, sender):
    rows = await pool.fetch(
        "SELECT user_id FROM chat_members WHERE chat_id = $1 AND user_id <> $2",
        chat_id, sender,
    )
    return [r["user_id"] for r in rows]


async def set_disappearing(pool, chat_id, seconds: int | None) -> None:
    await pool.execute(
        "UPDATE chats SET disappearing_seconds = $2 WHERE id = $1", chat_id, seconds
    )


async def clear_pending_expirations(pool, chat_id) -> None:
    """Turning disappearing OFF recalls not-yet-expired messages: clearing expires_at
    makes their already-scheduled reap jobs no-ops (the reaper skips live rows)."""
    await pool.execute(
        "UPDATE messages SET expires_at = NULL "
        "WHERE chat_id = $1 AND expires_at IS NOT NULL AND expires_at > now()",
        chat_id,
    )


async def message_for_reap(pool, mid):
    """Fetch (media_key, expires_at) by id with NO live filter, so the reaper can tell
    'already gone' (None) from 'recalled / not yet due' (expires_at null or future)."""
    return await pool.fetchrow(
        "SELECT media_key, expires_at FROM messages WHERE id = $1", mid
    )


async def delete_expired_message(pool, mid) -> None:
    """Hard-delete a message only if it is actually due (expires_at <= now()).
    Idempotent: a recalled or already-gone message is a no-op."""
    await pool.execute(
        "DELETE FROM messages WHERE id = $1 AND expires_at <= now()", mid
    )


async def due_disappearing_messages(pool, limit: int):
    """Reconciliation: disappearing messages past their expiry that a dropped reap
    enqueue may have left behind."""
    return await pool.fetch(
        "SELECT id, media_key FROM messages WHERE expires_at <= now() LIMIT $1", limit
    )


async def undelivered_message_ids(pool, limit: int):
    """Reconciliation: messages older than 30s with at least one never-delivered
    receipt, i.e. whose fanout enqueue may have been dropped post-commit."""
    rows = await pool.fetch(
        "SELECT DISTINCT m.id FROM messages m JOIN receipts r ON r.message_id = m.id "
        "WHERE r.delivered_at IS NULL "
        "  AND m.created_at < now() - interval '30 seconds' "
        "  AND (m.expires_at IS NULL OR m.expires_at > now()) "
        "LIMIT $1",
        limit,
    )
    return [r["id"] for r in rows]


# Batch loaders — each takes a list of ids and returns rows for the DataLoaders.

async def users_by_ids(pool, ids: list[uuid.UUID]):
    return await pool.fetch(
        "SELECT id, username, display_name FROM users WHERE id = ANY($1::uuid[])", ids
    )


async def members_by_chat_ids(pool, chat_ids: list[uuid.UUID]):
    return await pool.fetch(
        "SELECT cm.chat_id, u.id, u.username, u.display_name "
        "FROM chat_members cm JOIN users u ON u.id = cm.user_id "
        "WHERE cm.chat_id = ANY($1::uuid[]) "
        "ORDER BY u.display_name",
        chat_ids,
    )


async def last_messages_by_chat_ids(pool, chat_ids: list[uuid.UUID]):
    return await pool.fetch(
        "SELECT DISTINCT ON (chat_id) "
        "  id, chat_id, sender_id, body, media_key, content_type, created_at "
        "FROM messages "
        "WHERE chat_id = ANY($1::uuid[]) AND (expires_at IS NULL OR expires_at > now()) "
        "ORDER BY chat_id, created_at DESC",
        chat_ids,
    )


async def receipts_by_message_ids(pool, message_ids: list[uuid.UUID]):
    return await pool.fetch(
        "SELECT message_id, user_id, delivered_at, read_at "
        "FROM receipts WHERE message_id = ANY($1::uuid[])",
        message_ids,
    )


async def unread_counts(pool, viewer_id, chat_ids: list[uuid.UUID]):
    """Unread per chat for one viewer, derived from receipts (the single source of
    truth): the count of the viewer's still-unread receipts on live messages."""
    return await pool.fetch(
        "SELECT m.chat_id AS chat_id, count(*)::int AS n "
        "FROM receipts r JOIN messages m ON m.id = r.message_id "
        "WHERE r.user_id = $1 AND r.read_at IS NULL "
        "  AND (m.expires_at IS NULL OR m.expires_at > now()) "
        "  AND m.chat_id = ANY($2::uuid[]) "
        "GROUP BY m.chat_id",
        viewer_id, chat_ids,
    )

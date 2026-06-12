// Forge owns every forge_* table; this module owns the chat domain over a `pg`
// pool against the same database. Table names are unprefixed per the contract.

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { randomUUID } from "node:crypto";
import type { Pool } from "pg";

const MIGRATIONS = readFileSync(
  join(dirname(fileURLToPath(import.meta.url)), "..", "..", "migrations.sql"),
  "utf8",
);

export type ChatKind = "direct" | "group";

export interface UserRow {
  id: string;
  username: string;
  display_name: string;
}

export interface ChatRow {
  id: string;
  kind: ChatKind;
  title: string | null;
  created_by: string;
  created_at: Date;
  disappearing_seconds: number | null;
}

export interface MessageRow {
  id: string;
  chat_id: string;
  sender_id: string;
  body: string;
  media_key: string | null;
  content_type: string | null;
  created_at: Date;
}

export interface ReceiptRow {
  message_id: string;
  user_id: string;
  delivered_at: Date | null;
  read_at: Date | null;
}

export interface Credentials {
  id: string;
  passwordHash: string;
}

export async function migrate(pool: Pool): Promise<void> {
  await pool.query(MIGRATIONS);
}

export async function createUser(
  pool: Pool,
  username: string,
  displayName: string,
  passwordHash: string,
): Promise<string> {
  const id = randomUUID();
  await pool.query(
    "INSERT INTO users (id, username, display_name, password_hash) VALUES ($1,$2,$3,$4)",
    [id, username, displayName, passwordHash],
  );
  return id;
}

export async function usernameTaken(pool: Pool, username: string): Promise<boolean> {
  const { rows } = await pool.query<{ e: boolean }>(
    "SELECT EXISTS(SELECT 1 FROM users WHERE username = $1) AS e",
    [username],
  );
  return rows[0]!.e;
}

export async function credentials(pool: Pool, username: string): Promise<Credentials | null> {
  const { rows } = await pool.query<{ id: string; password_hash: string }>(
    "SELECT id, password_hash FROM users WHERE username = $1",
    [username],
  );
  const row = rows[0];
  return row ? { id: row.id, passwordHash: row.password_hash } : null;
}

export async function setPasswordHash(pool: Pool, userId: string, hash: string): Promise<void> {
  await pool.query("UPDATE users SET password_hash = $2 WHERE id = $1", [userId, hash]);
}

const USER_COLS = "id, username, display_name";

export async function usersByIds(pool: Pool, ids: readonly string[]): Promise<UserRow[]> {
  if (ids.length === 0) return [];
  const { rows } = await pool.query<UserRow>(
    `SELECT ${USER_COLS} FROM users WHERE id = ANY($1::uuid[])`,
    [ids],
  );
  return rows;
}

export async function userByUsername(pool: Pool, username: string): Promise<UserRow | null> {
  const { rows } = await pool.query<UserRow>(
    `SELECT ${USER_COLS} FROM users WHERE username = $1`,
    [username],
  );
  return rows[0] ?? null;
}

export async function chatsForUser(pool: Pool, userId: string): Promise<ChatRow[]> {
  // Most-recent activity first: order by the chat's latest message, falling back
  // to its creation time when it has none.
  const { rows } = await pool.query<ChatRow>(
    `SELECT c.id, c.kind, c.title, c.created_by, c.created_at, c.disappearing_seconds
       FROM chats c
       JOIN chat_members m ON m.chat_id = c.id
      WHERE m.user_id = $1
      ORDER BY COALESCE(
        (SELECT max(created_at) FROM messages
          WHERE chat_id = c.id AND (expires_at IS NULL OR expires_at > now())), c.created_at
      ) DESC`,
    [userId],
  );
  return rows;
}

export async function chatsByIds(pool: Pool, ids: readonly string[]): Promise<ChatRow[]> {
  if (ids.length === 0) return [];
  const { rows } = await pool.query<ChatRow>(
    `SELECT id, kind, title, created_by, created_at, disappearing_seconds
       FROM chats WHERE id = ANY($1::uuid[])`,
    [ids],
  );
  return rows;
}

export async function isMember(pool: Pool, chatId: string, userId: string): Promise<boolean> {
  const { rows } = await pool.query<{ e: boolean }>(
    "SELECT EXISTS(SELECT 1 FROM chat_members WHERE chat_id = $1 AND user_id = $2) AS e",
    [chatId, userId],
  );
  return rows[0]!.e;
}

// Batched members-by-chat: one round trip for many chats. Returns chat_id alongside
// each user so the DataLoader can group them back to their chats.
export async function membersByChatIds(
  pool: Pool,
  chatIds: readonly string[],
): Promise<Array<UserRow & { chat_id: string }>> {
  if (chatIds.length === 0) return [];
  const { rows } = await pool.query<UserRow & { chat_id: string }>(
    `SELECT m.chat_id, u.id, u.username, u.display_name
       FROM users u
       JOIN chat_members m ON m.user_id = u.id
      WHERE m.chat_id = ANY($1::uuid[])
      ORDER BY u.display_name`,
    [chatIds],
  );
  return rows;
}

export async function lastMessagesByChatIds(
  pool: Pool,
  chatIds: readonly string[],
): Promise<MessageRow[]> {
  if (chatIds.length === 0) return [];
  const { rows } = await pool.query<MessageRow>(
    `SELECT DISTINCT ON (chat_id)
            id, chat_id, sender_id, body, media_key, content_type, created_at
       FROM messages
      WHERE chat_id = ANY($1::uuid[]) AND (expires_at IS NULL OR expires_at > now())
      ORDER BY chat_id, created_at DESC`,
    [chatIds],
  );
  return rows;
}

export async function createChat(
  pool: Pool,
  kind: ChatKind,
  title: string | null,
  createdBy: string,
  memberIds: readonly string[],
): Promise<string> {
  const id = randomUUID();
  const client = await pool.connect();
  try {
    await client.query("BEGIN");
    await client.query(
      "INSERT INTO chats (id, kind, title, created_by) VALUES ($1,$2,$3,$4)",
      [id, kind, title, createdBy],
    );
    for (const uid of memberIds) {
      await client.query(
        `INSERT INTO chat_members (chat_id, user_id) VALUES ($1,$2)
         ON CONFLICT (chat_id, user_id) DO NOTHING`,
        [id, uid],
      );
    }
    await client.query("COMMIT");
  } catch (e) {
    await client.query("ROLLBACK");
    throw e;
  } finally {
    client.release();
  }
  return id;
}

export async function addMember(pool: Pool, chatId: string, userId: string): Promise<void> {
  await pool.query(
    `INSERT INTO chat_members (chat_id, user_id) VALUES ($1,$2)
     ON CONFLICT (chat_id, user_id) DO NOTHING`,
    [chatId, userId],
  );
}

const MESSAGE_COLS = "id, chat_id, sender_id, body, media_key, content_type, created_at";

// Atomic: the message row and its recipients' receipts land together, so a crash
// can't orphan a message with no unread tracking. Mirrors createChat's transaction.
export async function insertMessageWithReceipts(
  pool: Pool,
  id: string,
  chatId: string,
  senderId: string,
  body: string,
  mediaKey: string | null,
  contentType: string | null,
  expiresAt: Date | null,
): Promise<void> {
  const client = await pool.connect();
  try {
    await client.query("BEGIN");
    await client.query(
      `INSERT INTO messages (id, chat_id, sender_id, body, media_key, content_type, expires_at)
       VALUES ($1,$2,$3,$4,$5,$6,$7)`,
      [id, chatId, senderId, body, mediaKey, contentType, expiresAt],
    );
    const { rows } = await client.query<{ user_id: string }>(
      "SELECT user_id FROM chat_members WHERE chat_id = $1 AND user_id <> $2",
      [chatId, senderId],
    );
    if (rows.length > 0) {
      await client.query(
        `INSERT INTO receipts (message_id, user_id)
         SELECT $1, unnest($2::uuid[])
         ON CONFLICT (message_id, user_id) DO NOTHING`,
        [id, rows.map((r) => r.user_id)],
      );
    }
    await client.query("COMMIT");
  } catch (e) {
    await client.query("ROLLBACK");
    throw e;
  } finally {
    client.release();
  }
}

export async function messageById(pool: Pool, id: string): Promise<MessageRow | null> {
  const { rows } = await pool.query<MessageRow>(
    `SELECT ${MESSAGE_COLS} FROM messages
      WHERE id = $1 AND (expires_at IS NULL OR expires_at > now())`,
    [id],
  );
  return rows[0] ?? null;
}

// Newest-first window: `before` is an exclusive created_at cursor; expired rows hidden.
export async function listMessages(
  pool: Pool,
  chatId: string,
  before: Date | null,
  limit: number,
): Promise<MessageRow[]> {
  const { rows } = await pool.query<MessageRow>(
    `SELECT ${MESSAGE_COLS} FROM messages
      WHERE chat_id = $1
        AND (expires_at IS NULL OR expires_at > now())
        AND ($2::timestamptz IS NULL OR created_at < $2)
      ORDER BY created_at DESC
      LIMIT $3`,
    [chatId, before, limit],
  );
  return rows;
}

// Flips delivered_at exactly once; returns whether this call did it (idempotency gate).
export async function markDelivered(
  pool: Pool,
  messageId: string,
  userId: string,
): Promise<boolean> {
  const res = await pool.query(
    `UPDATE receipts SET delivered_at = now()
      WHERE message_id = $1 AND user_id = $2 AND delivered_at IS NULL`,
    [messageId, userId],
  );
  return (res.rowCount ?? 0) > 0;
}

// Scoped to chatId so a member of one chat can't flip a receipt on a message in
// another. Returns whether a receipt row was actually updated.
export async function markRead(
  pool: Pool,
  chatId: string,
  messageId: string,
  userId: string,
): Promise<boolean> {
  const res = await pool.query(
    `UPDATE receipts
        SET read_at = COALESCE(read_at, now()),
            delivered_at = COALESCE(delivered_at, now())
      WHERE message_id = $1 AND user_id = $2
        AND EXISTS (SELECT 1 FROM messages WHERE id = $1 AND chat_id = $3)`,
    [messageId, userId, chatId],
  );
  return (res.rowCount ?? 0) > 0;
}

// Unread per chat for one viewer = count of their receipts still unread on a
// live message. Receipts are the single source of truth; there is no kv counter.
export async function unreadCountsByChatIds(
  pool: Pool,
  viewerId: string,
  chatIds: readonly string[],
): Promise<Map<string, number>> {
  if (chatIds.length === 0) return new Map();
  const { rows } = await pool.query<{ chat_id: string; n: number }>(
    `SELECT m.chat_id AS chat_id, count(*)::int AS n
       FROM receipts r JOIN messages m ON m.id = r.message_id
      WHERE r.user_id = $1 AND r.read_at IS NULL
        AND (m.expires_at IS NULL OR m.expires_at > now())
        AND m.chat_id = ANY($2::uuid[])
      GROUP BY m.chat_id`,
    [viewerId, chatIds],
  );
  return new Map(rows.map((r) => [r.chat_id, r.n]));
}

export async function receiptsByMessageIds(
  pool: Pool,
  messageIds: readonly string[],
): Promise<ReceiptRow[]> {
  if (messageIds.length === 0) return [];
  const { rows } = await pool.query<ReceiptRow>(
    `SELECT message_id, user_id, delivered_at, read_at
       FROM receipts WHERE message_id = ANY($1::uuid[])
      ORDER BY user_id`,
    [messageIds],
  );
  return rows;
}

export async function otherMemberIds(
  pool: Pool,
  chatId: string,
  sender: string,
): Promise<string[]> {
  const { rows } = await pool.query<{ user_id: string }>(
    "SELECT user_id FROM chat_members WHERE chat_id = $1 AND user_id <> $2",
    [chatId, sender],
  );
  return rows.map((r) => r.user_id);
}

export async function chatById(pool: Pool, id: string): Promise<ChatRow | null> {
  const { rows } = await pool.query<ChatRow>(
    `SELECT id, kind, title, created_by, created_at, disappearing_seconds
       FROM chats WHERE id = $1`,
    [id],
  );
  return rows[0] ?? null;
}

export async function setDisappearing(
  pool: Pool,
  chatId: string,
  seconds: number | null,
): Promise<void> {
  await pool.query("UPDATE chats SET disappearing_seconds = $2 WHERE id = $1", [chatId, seconds]);
}

// Turning disappearing OFF recalls pending reaps: clearing expires_at on not-yet-
// expired messages makes their scheduled reap jobs no-ops (see reapMessage).
export async function clearExpiry(pool: Pool, chatId: string): Promise<void> {
  await pool.query(
    `UPDATE messages SET expires_at = NULL
      WHERE chat_id = $1 AND expires_at IS NOT NULL AND expires_at > now()`,
    [chatId],
  );
}

export interface ReapTarget {
  media_key: string | null;
  expires_at: Date | null;
}

// Reap lookup: the raw (media_key, expires_at) by id, ignoring the live filter, so
// the worker can decide whether the message is actually due or was recalled.
export async function reapTarget(pool: Pool, id: string): Promise<ReapTarget | null> {
  const { rows } = await pool.query<ReapTarget>(
    "SELECT media_key, expires_at FROM messages WHERE id = $1",
    [id],
  );
  return rows[0] ?? null;
}

// Hard-deletes one message only if it is actually due. Idempotent: re-running on an
// already-gone or no-longer-due message deletes nothing.
export async function deleteIfDue(pool: Pool, id: string): Promise<void> {
  await pool.query("DELETE FROM messages WHERE id = $1 AND expires_at <= now()", [id]);
}

// Reconciliation: due messages (id + media key) for the self-heal reap sweep.
export async function dueMessages(
  pool: Pool,
  limit: number,
): Promise<Array<{ id: string; media_key: string | null }>> {
  const { rows } = await pool.query<{ id: string; media_key: string | null }>(
    "SELECT id, media_key FROM messages WHERE expires_at <= now() LIMIT $1",
    [limit],
  );
  return rows;
}

// Reconciliation: messages older than the grace window with at least one receipt
// still undelivered, for the self-heal fanout re-enqueue.
export async function undeliveredMessageIds(pool: Pool, limit: number): Promise<string[]> {
  const { rows } = await pool.query<{ id: string }>(
    `SELECT DISTINCT m.id
       FROM messages m JOIN receipts r ON r.message_id = m.id
      WHERE r.delivered_at IS NULL AND m.created_at < now() - interval '30 seconds'
        AND (m.expires_at IS NULL OR m.expires_at > now())
      LIMIT $1`,
    [limit],
  );
  return rows.map((r) => r.id);
}

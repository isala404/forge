import { randomUUID } from "node:crypto";
import pg from "pg";
import DataLoader from "dataloader";
import { ForgeClient, type JsSubscription } from "forge-node";

import * as db from "./db.ts";
import type { MessageRow, ReceiptRow, UserRow } from "./db.ts";

export const DEFAULT_MAX_UPLOAD_BYTES = 10 * 1024 * 1024;
export const SESSION_IDLE_SECS = 30 * 60;
export const SESSION_ABSOLUTE_SECS = 7 * 24 * 60 * 60;

export const PRESENCE_TOPIC = "presence";
export const FANOUT_QUEUE = "fanout";
export const REAP_QUEUE = "reap";
// Its worker always nacks (max 1 attempt), so a triggered job dead-letters into
// `fail.dlq` — the opsStats DLQ gauge reads from there.
export const FAIL_QUEUE = "fail";

export function chatTopic(chatId: string): string {
  return `chat:${chatId}`;
}

function envInt(key: string, fallback: number): number {
  const v = parseInt(process.env[key] ?? "", 10);
  return Number.isFinite(v) ? v : fallback;
}

export function presenceTtlSecs(): number {
  return envInt("APP_PRESENCE_TTL_SECS", 30);
}

export function disappearingSecs(): number {
  return envInt("APP_DISAPPEARING_SECS", 86_400);
}

export function schedulerMs(): number {
  return envInt("APP_SCHEDULER_MS", 30_000);
}

// Wire shape shared byte-for-byte across all three backends.
export type RealtimeEvent =
  | { type: "message"; message_id: string }
  | { type: "typing"; user_id: string; typing: boolean }
  | { type: "receipt"; message_id: string; user_id: string }
  | { type: "presence"; user_id: string; online: boolean };

export interface CurrentUser {
  id: string;
  // The session token when authed via a session; empty string for an API-key
  // principal (which `logout` cannot revoke).
  token: string;
}

function groupBy<T, K>(rows: readonly T[], key: (row: T) => K): Map<K, T[]> {
  const out = new Map<K, T[]>();
  for (const row of rows) {
    const k = key(row);
    const bucket = out.get(k);
    if (bucket) bucket.push(row);
    else out.set(k, [row]);
  }
  return out;
}

// Per-request batchers. A query selecting 50 messages resolves their senders and
// receipts in one round trip each, not 50.
export interface Loaders {
  userById: DataLoader<string, UserRow | null>;
  membersByChatId: DataLoader<string, UserRow[]>;
  lastMessageByChatId: DataLoader<string, MessageRow | null>;
  receiptsByMessageId: DataLoader<string, ReceiptRow[]>;
  online: DataLoader<string, boolean>;
  unread: DataLoader<string, number>;
}

export class AppCtx {
  forge: ForgeClient;
  pool: pg.Pool;
  // A throwaway argon2id hash minted once at startup (see initAppCtx). `login`
  // verifies the submitted password against it when the username doesn't exist, so
  // the username-miss path spends the same argon2 time as a real verify and the
  // timing no longer reveals which usernames are registered.
  decoyHash: string;

  constructor(forge: ForgeClient, pool: pg.Pool, decoyHash: string) {
    this.forge = forge;
    this.pool = pool;
    this.decoyHash = decoyHash;
  }

  makeLoaders(viewerId: string | null): Loaders {
    const pool = this.pool;
    return {
      userById: new DataLoader(async (ids) => {
        const rows = await db.usersByIds(pool, ids as string[]);
        const byId = new Map(rows.map((u) => [u.id, u]));
        return ids.map((id) => byId.get(id) ?? null);
      }),
      membersByChatId: new DataLoader(async (chatIds) => {
        const rows = await db.membersByChatIds(pool, chatIds as string[]);
        const grouped = groupBy(rows, (r) => r.chat_id);
        return chatIds.map((id) =>
          (grouped.get(id) ?? []).map(({ chat_id: _c, ...u }) => u as UserRow),
        );
      }),
      lastMessageByChatId: new DataLoader(async (chatIds) => {
        const rows = await db.lastMessagesByChatIds(pool, chatIds as string[]);
        const byChat = new Map(rows.map((m) => [m.chat_id, m]));
        return chatIds.map((id) => byChat.get(id) ?? null);
      }),
      receiptsByMessageId: new DataLoader(async (messageIds) => {
        const rows = await db.receiptsByMessageIds(pool, messageIds as string[]);
        const grouped = groupBy(rows, (r) => r.message_id);
        return messageIds.map((id) => grouped.get(id) ?? []);
      }),
      online: new DataLoader(async (userIds) => {
        const keys = userIds.map((id) => `online:${id}`);
        let vals: Array<string | undefined | null>;
        try {
          vals = await this.forge.kvMget(keys);
        } catch {
          return userIds.map(() => false);
        }
        return vals.map((v) => v != null);
      }),
      unread: new DataLoader(async (chatIds) => {
        if (!viewerId) return chatIds.map(() => 0);
        const counts = await db.unreadCountsByChatIds(pool, viewerId, chatIds as string[]);
        return chatIds.map((id) => counts.get(id) ?? 0);
      }),
    };
  }

  subscribe(topic: string): AsyncIterableIterator<RealtimeEvent> {
    let sub: JsSubscription | null = null;
    const ready = this.forge.pubsubSubscribe(topic).then((s) => {
      sub = s;
    });
    let done = false;
    const it: AsyncIterableIterator<RealtimeEvent> = {
      async next(): Promise<IteratorResult<RealtimeEvent>> {
        await ready;
        for (;;) {
          if (done || !sub) return { value: undefined, done: true };
          const buf = await sub.next();
          if (buf === null) {
            done = true;
            return { value: undefined, done: true };
          }
          try {
            return { value: JSON.parse(buf.toString("utf8")) as RealtimeEvent, done: false };
          } catch {
            continue;
          }
        }
      },
      async return(): Promise<IteratorResult<RealtimeEvent>> {
        done = true;
        sub = null; // drop the handle -> unsubscribe + release the connection
        return { value: undefined, done: true };
      },
      [Symbol.asyncIterator]() {
        return this;
      },
    };
    return it;
  }

  async publish(topic: string, event: RealtimeEvent): Promise<void> {
    try {
      await this.forge.pubsubPublish(topic, JSON.stringify(event));
    } catch (e) {
      console.warn(`pubsub publish failed (${topic}):`, (e as Error).message);
    }
  }

  async touchPresence(userId: string): Promise<void> {
    await this.forge.kvSet(`online:${userId}`, "1", presenceTtlSecs(), false);
  }

  async maxUploadBytes(): Promise<number> {
    try {
      const raw = await this.forge.configGet("max_upload_bytes");
      if (raw == null) return DEFAULT_MAX_UPLOAD_BYTES;
      const n = parseInt(raw.trim(), 10);
      return Number.isFinite(n) ? n : DEFAULT_MAX_UPLOAD_BYTES;
    } catch {
      return DEFAULT_MAX_UPLOAD_BYTES;
    }
  }

  async reactionsEnabled(userId: string): Promise<boolean> {
    try {
      return await this.forge.flag("reactions_v2", false, userId);
    } catch {
      return false;
    }
  }

  async setReactionsRollout(percent: number): Promise<void> {
    await this.forge.setFlagPercent("reactions_v2", percent);
  }

  // Accurate up to the 1000-key scan cap; not for very large user bases.
  async onlineCount(): Promise<number> {
    try {
      return (await this.forge.kvScan("online:", 1000)).length;
    } catch {
      return 0;
    }
  }

  async enqueueFailing(): Promise<void> {
    await this.forge.queueEnqueue(FAIL_QUEUE, "boom", 1);
  }

  async dlqCount(): Promise<number> {
    const d = await this.forge.queueDepth(`${FAIL_QUEUE}.dlq`);
    return d.visible + d.inFlight + d.delayed;
  }
}

export interface GqlContext {
  app: AppCtx;
  loaders: Loaders;
  currentUser: CurrentUser | null;
}

export function postgresUrl(): string {
  return (
    process.env.FORGE_POSTGRES_URL ||
    "postgres://postgres:forge@127.0.0.1:5432/chatapp_node"
  );
}

export async function initAppCtx(pgUrl = postgresUrl()): Promise<AppCtx> {
  // connect migrates Forge's system tables at startup; it owns its database.
  const forge = await ForgeClient.connect(
    pgUrl,
    process.env.FORGE_BLOB_SIGNING_SECRET || "dev-secret-change-me",
  );
  const pool = new pg.Pool({ connectionString: pgUrl, max: 10 });
  await db.migrate(pool);
  // Mint the login decoy hash once, via forge's own hasher so its argon2 params
  // always match real password hashes. `login` verifies against it on a username
  // miss to keep that path's timing indistinguishable from a real verify.
  const decoyHash = await forge.hashPassword(randomUUID());
  return new AppCtx(forge, pool, decoyHash);
}

// A bearer token authenticates as either a session (slides the idle deadline) or
// an API key. Session wins; an API-key principal carries an empty token.
export async function userFromBearer(
  app: AppCtx,
  authorization: string | undefined,
): Promise<CurrentUser | null> {
  if (!authorization) return null;
  const m = /^bearer\s+(.+)$/i.exec(authorization.trim());
  if (!m) return null;
  const token = m[1]!;

  let userId: string | null;
  try {
    userId = await app.forge.validateSession(token);
  } catch {
    userId = null;
  }
  if (userId) return { id: userId, token };

  let ownerId: string | null;
  try {
    ownerId = await app.forge.verifyApiKey(token);
  } catch {
    ownerId = null;
  }
  if (ownerId) return { id: ownerId, token: "" };
  return null;
}

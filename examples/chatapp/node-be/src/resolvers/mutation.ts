import { randomUUID } from "node:crypto";

import * as db from "../db.ts";
import type { ChatRow, MessageRow, UserRow } from "../db.ts";
import {
  chatTopic,
  disappearingSecs,
  PRESENCE_TOPIC,
  FANOUT_QUEUE,
  REAP_QUEUE,
} from "../context.ts";
import { err, mapDb, mapForge } from "../errors.ts";
import type { MutationResolvers } from "../generated/graphql.ts";
import {
  APIKEY_LIMIT,
  OTP_LIMIT,
  SEND_LIMIT,
  UPLOAD_LIMIT,
  issueSession,
  parseId,
  requireAuth,
  requireMember,
} from "./helpers.ts";

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

export const Mutation: MutationResolvers = {
  signup: async (_r, { username, displayName, password }, ctx) => {
    const app = ctx.app;
    const uname = username.trim();
    // Validate before spending a rate-limit token so bad input can't burn a
    // victim's bucket; normalized username keys the bucket and stored row alike.
    if (uname.length < 3 || password.length < 6) {
      throw err("INVALID", "username must be >= 3 chars and password >= 6 chars");
    }
    // fail CLOSED: a rate-limit backend error must surface, not grant access.
    let d;
    try {
      d = await app.forge.rateLimitCheck("otp", uname, OTP_LIMIT.max, OTP_LIMIT.perSeconds, false);
    } catch (e) {
      throw mapForge(e);
    }
    if (!d.allowed) throw err("LIMIT", "too many signup attempts; try again later");
    let taken: boolean;
    try {
      taken = await db.usernameTaken(app.pool, uname);
    } catch (e) {
      throw mapDb(e);
    }
    if (taken) throw err("PRECONDITION", "username already taken");
    let hash: string;
    try {
      hash = await app.forge.hashPassword(password);
    } catch (e) {
      throw mapForge(e);
    }
    let userId: string;
    try {
      userId = await db.createUser(app.pool, uname, displayName, hash);
    } catch (e) {
      throw mapDb(e);
    }
    return issueSession(ctx, userId);
  },

  login: async (_r, { username, password }, ctx) => {
    const app = ctx.app;
    const uname = username.trim();
    let d;
    try {
      d = await app.forge.rateLimitCheck("otp", uname, OTP_LIMIT.max, OTP_LIMIT.perSeconds, false);
    } catch (e) {
      throw mapForge(e);
    }
    if (!d.allowed) throw err("LIMIT", "too many login attempts; try again later");
    let creds;
    try {
      creds = await db.credentials(app.pool, uname);
    } catch (e) {
      throw mapDb(e);
    }
    if (!creds) throw err("UNAUTHENTICATED", "invalid username or password");
    let ok: boolean;
    try {
      ok = await app.forge.verifyPassword(password, creds.passwordHash);
    } catch (e) {
      throw mapForge(e);
    }
    if (!ok) throw err("UNAUTHENTICATED", "invalid username or password");
    // Transparently upgrade a hash minted under older argon2 params; a rehash
    // failure must never block an otherwise-valid login.
    try {
      if (app.forge.needsRehash(creds.passwordHash)) {
        const fresh = await app.forge.hashPassword(password);
        await db.setPasswordHash(app.pool, creds.id, fresh);
      }
    } catch (e) {
      console.warn("password rehash skipped:", (e as Error).message);
    }
    return issueSession(ctx, creds.id);
  },

  logout: async (_r, _a, ctx) => {
    if (ctx.currentUser?.token) {
      try {
        await ctx.app.forge.revokeSession(ctx.currentUser.token);
      } catch (e) {
        throw mapForge(e);
      }
    }
    return true;
  },

  logoutAll: async (_r, _a, ctx) => {
    const user = requireAuth(ctx);
    try {
      await ctx.app.forge.revokeAllSessions(user.id);
    } catch (e) {
      throw mapForge(e);
    }
    return true;
  },

  createChat: async (_r, { kind, title, memberUsernames }, ctx) => {
    const app = ctx.app;
    const user = requireAuth(ctx);
    const ids = [user.id];
    for (const uname of memberUsernames) {
      let u: UserRow | null;
      try {
        u = await db.userByUsername(app.pool, uname);
      } catch (e) {
        throw mapDb(e);
      }
      if (!u) throw err("NOT_FOUND", `no such user: ${uname}`);
      if (!ids.includes(u.id)) ids.push(u.id);
    }
    const kindStr = kind === "DIRECT" ? "direct" : "group";
    if (kind === "DIRECT" && ids.length !== 2) {
      throw err("INVALID", "a direct chat needs exactly one other member");
    }
    let chatId: string;
    try {
      chatId = await db.createChat(app.pool, kindStr, title ?? null, user.id, ids);
    } catch (e) {
      throw mapDb(e);
    }
    let row: ChatRow | null;
    try {
      row = await db.chatById(app.pool, chatId);
    } catch (e) {
      throw mapDb(e);
    }
    if (!row) throw err("BACKEND", "chat vanished after create");
    return row;
  },

  addMember: async (_r, { chatId, username }, ctx) => {
    const app = ctx.app;
    const user = requireAuth(ctx);
    const id = parseId(chatId);
    await requireMember(app, id, user.id);
    let u: UserRow | null;
    try {
      u = await db.userByUsername(app.pool, username);
    } catch (e) {
      throw mapDb(e);
    }
    if (!u) throw err("NOT_FOUND", `no such user: ${username}`);
    try {
      await db.addMember(app.pool, id, u.id);
    } catch (e) {
      throw mapDb(e);
    }
    let row: ChatRow | null;
    try {
      row = await db.chatById(app.pool, id);
    } catch (e) {
      throw mapDb(e);
    }
    if (!row) throw err("NOT_FOUND", "chat not found");
    return row;
  },

  requestUpload: async (_r, { chatId }, ctx) => {
    const app = ctx.app;
    const user = requireAuth(ctx);
    const id = parseId(chatId);
    await requireMember(app, id, user.id);
    // fail CLOSED: deny presign minting if the rate-limit backend errors.
    let rl;
    try {
      rl = await app.forge.rateLimitCheck("upload", user.id, UPLOAD_LIMIT.max, UPLOAD_LIMIT.perSeconds, false);
    } catch (e) {
      throw mapForge(e);
    }
    if (!rl.allowed) throw err("LIMIT", "too many upload requests; slow down");
    const maxBytes = await app.maxUploadBytes();
    const key = `media/${id}/${randomUUID()}`;
    let uploadUrl: string;
    try {
      uploadUrl = await app.forge.blobPresignUpload(key, 600, maxBytes);
    } catch (e) {
      throw mapForge(e);
    }
    return { key, uploadUrl, maxBytes: Math.min(maxBytes, 0x7fffffff) };
  },

  sendMessage: async (_r, { chatId, body, mediaKey, idempotencyKey }, ctx) => {
    const app = ctx.app;
    const user = requireAuth(ctx);
    const id = parseId(chatId);
    await requireMember(app, id, user.id);

    // fail OPEN: a rate-limit backend error must not block delivery.
    let d;
    try {
      d = await app.forge.rateLimitCheck("send", user.id, SEND_LIMIT.max, SEND_LIMIT.perSeconds, true);
    } catch (e) {
      throw mapForge(e);
    }
    if (!d.allowed) throw err("LIMIT", "you are sending too fast; slow down");
    if (body.trim() === "" && !mediaKey) {
      throw err("INVALID", "message must have text or an attachment");
    }

    let contentType: string | null = null;
    if (mediaKey) {
      if (!mediaKey.startsWith(`media/${id}/`)) {
        throw err("INVALID", "media_key does not belong to this chat");
      }
      try {
        contentType = await app.forge.blobContentType(mediaKey);
      } catch (e) {
        throw mapForge(e);
      }
    }

    let expiresAt: Date | null = null;
    try {
      const chat = await db.chatById(app.pool, id);
      if (chat?.disappearing_seconds) {
        expiresAt = new Date(Date.now() + chat.disappearing_seconds * 1000);
      }
    } catch (e) {
      throw mapDb(e);
    }

    const msgId = randomUUID();

    // Client idempotency: claim the key with SET NX before inserting. The first
    // caller wins and proceeds; a resend after a lost response loses the claim
    // and returns the winner's original message instead of inserting a duplicate.
    if (idempotencyKey) {
      const key = `idem:send:${user.id}:${idempotencyKey}`;
      let won: boolean;
      try {
        won = await app.forge.kvSet(key, msgId, 86400, true);
      } catch (e) {
        throw mapForge(e);
      }
      if (!won) {
        // Another in-flight/just-completed send owns this key; its value is the
        // winner's msgId. Wait briefly for the winner to finish its insert.
        for (let i = 0; i < 5; i++) {
          let existing: string | null;
          try {
            existing = await app.forge.kvGet(key);
          } catch (e) {
            throw mapForge(e);
          }
          if (existing) {
            let m: MessageRow | null;
            try {
              m = await db.messageById(app.pool, existing);
            } catch (e) {
              throw mapDb(e);
            }
            if (m) return m;
          }
          await sleep(50);
        }
        throw err("INVALID", "duplicate send in progress; retry");
      }
    }

    try {
      await db.insertMessageWithReceipts(
        app.pool,
        msgId,
        id,
        user.id,
        body,
        mediaKey ?? null,
        contentType,
        expiresAt,
      );
    } catch (e) {
      throw mapDb(e);
    }

    await app.publish(chatTopic(id), { type: "message", message_id: msgId });
    try {
      // Dedup on the message id so a retried sendMessage resolver can't double-enqueue.
      await app.forge.queueEnqueue(FANOUT_QUEUE, JSON.stringify({ message_id: msgId }), undefined, msgId);
    } catch (e) {
      throw mapForge(e);
    }

    if (expiresAt) {
      try {
        await app.forge.scheduleAt(expiresAt.getTime(), REAP_QUEUE, JSON.stringify({ message_id: msgId }));
      } catch (e) {
        throw mapForge(e);
      }
    }

    let row: MessageRow | null;
    try {
      row = await db.messageById(app.pool, msgId);
    } catch (e) {
      throw mapDb(e);
    }
    if (!row) throw err("BACKEND", "message vanished after insert");
    return row;
  },

  setDisappearing: async (_r, { chatId, enabled }, ctx) => {
    const app = ctx.app;
    const user = requireAuth(ctx);
    const id = parseId(chatId);
    await requireMember(app, id, user.id);
    try {
      await db.setDisappearing(app.pool, id, enabled ? disappearingSecs() : null);
      // Turning OFF recalls pending reaps for not-yet-expired messages.
      if (!enabled) await db.clearExpiry(app.pool, id);
    } catch (e) {
      throw mapDb(e);
    }
    let row: ChatRow | null;
    try {
      row = await db.chatById(app.pool, id);
    } catch (e) {
      throw mapDb(e);
    }
    if (!row) throw err("NOT_FOUND", "chat not found");
    return row;
  },

  setTyping: async (_r, { chatId, typing }, ctx) => {
    const app = ctx.app;
    const user = requireAuth(ctx);
    const id = parseId(chatId);
    await requireMember(app, id, user.id);
    // The indicator rides the pubsub 'typing' event; nothing reads a kv key.
    await app.publish(chatTopic(id), { type: "typing", user_id: user.id, typing });
    return true;
  },

  markRead: async (_r, { chatId, messageId }, ctx) => {
    const app = ctx.app;
    const user = requireAuth(ctx);
    const id = parseId(chatId);
    const mId = parseId(messageId);
    await requireMember(app, id, user.id);
    let updated: boolean;
    try {
      // mark_read sets receipts.read_at, the single source of truth for unread.
      updated = await db.markRead(app.pool, id, mId, user.id);
    } catch (e) {
      throw mapDb(e);
    }
    // Best-effort true, but only fire the receipt event when a real row flipped so
    // a cross-chat messageId can't broadcast a bogus receipt.
    if (updated) {
      await app.publish(chatTopic(id), { type: "receipt", message_id: mId, user_id: user.id });
    }
    return true;
  },

  heartbeat: async (_r, _a, ctx) => {
    const user = requireAuth(ctx);
    try {
      await ctx.app.touchPresence(user.id);
    } catch (e) {
      throw mapForge(e);
    }
    await ctx.app.publish(PRESENCE_TOPIC, { type: "presence", user_id: user.id, online: true });
    return true;
  },

  createApiKey: async (_r, { label }, ctx) => {
    const app = ctx.app;
    const user = requireAuth(ctx);
    // fail CLOSED: deny key minting if the rate-limit backend errors.
    let rl;
    try {
      rl = await app.forge.rateLimitCheck("apikey", user.id, APIKEY_LIMIT.max, APIKEY_LIMIT.perSeconds, false);
    } catch (e) {
      throw mapForge(e);
    }
    if (!rl.allowed) throw err("LIMIT", "too many API keys created; try again later");
    try {
      const key = await app.forge.createApiKey(user.id, label);
      return { id: key.id, secret: key.secret };
    } catch (e) {
      throw mapForge(e);
    }
  },

  setReactionsRollout: async (_r, { percent }, ctx) => {
    requireAuth(ctx);
    const pct = Math.min(Math.max(percent, 0), 100);
    try {
      await ctx.app.setReactionsRollout(pct);
    } catch (e) {
      throw mapForge(e);
    }
    return true;
  },

  triggerFailingJob: async (_r, _a, ctx) => {
    requireAuth(ctx);
    try {
      await ctx.app.enqueueFailing();
    } catch (e) {
      throw mapForge(e);
    }
    return true;
  },
};

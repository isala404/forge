import { randomUUID } from "node:crypto";

import * as db from "../db.ts";
import type { ChatRow, MessageRow } from "../db.ts";
import { chatTopic, disappearingSecs, FANOUT_QUEUE, REAP_QUEUE } from "../context.ts";
import { err, mapDb, mapForge } from "../errors.ts";
import type { MutationResolvers, QueryResolvers, SubscriptionResolvers } from "../generated/graphql.ts";
import { SEND_LIMIT, UPLOAD_LIMIT, mapEvents, parseId, requireAuth, requireMember } from "./helpers.ts";

function sleep(ms: number): Promise<void> {
  return new Promise((r) => setTimeout(r, ms));
}

export const query: Required<Pick<QueryResolvers, "messages">> = {
  messages: async (_r, { chatId, before, limit }, ctx) => {
    const user = requireAuth(ctx);
    const id = parseId(chatId);
    await requireMember(ctx.app, id, user.id);
    const lim = Math.min(Math.max(limit, 1), 200);
    try {
      return await db.listMessages(ctx.app.pool, id, before ?? null, lim);
    } catch (e) {
      throw mapDb(e);
    }
  },
};

export const mutation: Required<Pick<MutationResolvers, "requestUpload" | "sendMessage" | "setDisappearing">> = {
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
      await app.forge.queue<{ message_id: string }>(FANOUT_QUEUE)
        .enqueue({ message_id: msgId }, { dedupId: msgId });
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
};

export const subscription: Required<Pick<SubscriptionResolvers, "messageAdded">> = {
  messageAdded: {
    subscribe: async (_r, { chatId }, ctx) => {
      const user = requireAuth(ctx);
      const id = parseId(chatId);
      await requireMember(ctx.app, id, user.id);
      return mapEvents(ctx, chatTopic(id), async (ev) => {
        if (ev.type !== "message") return null;
        const m = await db.messageById(ctx.app.pool, ev.message_id);
        return m ? { messageAdded: m } : null;
      });
    },
  },
};

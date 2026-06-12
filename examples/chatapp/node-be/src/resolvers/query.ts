import * as db from "../db.ts";
import type { UserRow } from "../db.ts";
import { mapDb } from "../errors.ts";
import type { QueryResolvers } from "../generated/graphql.ts";
import { parseId, requireAuth, requireMember } from "./helpers.ts";

export const Query: QueryResolvers = {
  me: async (_r, _a, ctx) => {
    if (!ctx.currentUser) return null;
    try {
      return await ctx.loaders.userById.load(ctx.currentUser.id);
    } catch (e) {
      throw mapDb(e);
    }
  },

  chats: async (_r, _a, ctx) => {
    const user = requireAuth(ctx);
    try {
      return await db.chatsForUser(ctx.app.pool, user.id);
    } catch (e) {
      throw mapDb(e);
    }
  },

  chat: async (_r, { id }, ctx) => {
    const user = requireAuth(ctx);
    const chatId = parseId(id);
    await requireMember(ctx.app, chatId, user.id);
    try {
      return await db.chatById(ctx.app.pool, chatId);
    } catch (e) {
      throw mapDb(e);
    }
  },

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

  presence: async (_r, { userIds }, ctx) => {
    requireAuth(ctx);
    const ids = userIds.map(parseId);
    let rows: UserRow[];
    try {
      rows = await db.usersByIds(ctx.app.pool, ids);
    } catch (e) {
      throw mapDb(e);
    }
    // Preserve request order, drop unknown ids.
    const byId = new Map(rows.map((u) => [u.id, u]));
    return ids.flatMap((id) => {
      const u = byId.get(id);
      return u ? [u] : [];
    });
  },

  reactionsEnabled: async (_r, _a, ctx) => {
    if (!ctx.currentUser) return false;
    return ctx.app.reactionsEnabled(ctx.currentUser.id);
  },

  opsStats: async (_r, _a, ctx) => {
    requireAuth(ctx);
    const [onlineCount, dlqCount] = await Promise.all([
      ctx.app.onlineCount(),
      ctx.app.dlqCount(),
    ]);
    return { onlineCount, dlqCount };
  },
};

import * as db from "../db.ts";
import type { UserRow } from "../db.ts";
import { chatTopic, PRESENCE_TOPIC } from "../context.ts";
import { mapDb, mapForge } from "../errors.ts";
import type { MutationResolvers, QueryResolvers, SubscriptionResolvers } from "../generated/graphql.ts";
import { mapEvents, parseId, requireAuth, requireMember } from "./helpers.ts";

export const query: Required<Pick<QueryResolvers, "presence">> = {
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
};

export const mutation: Required<Pick<MutationResolvers, "setTyping" | "heartbeat">> = {
  setTyping: async (_r, { chatId, typing }, ctx) => {
    const app = ctx.app;
    const user = requireAuth(ctx);
    const id = parseId(chatId);
    await requireMember(app, id, user.id);
    // The indicator rides the pubsub 'typing' event; nothing reads a kv key.
    await app.publish(chatTopic(id), { type: "typing", user_id: user.id, typing });
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
};

export const subscription: Required<Pick<SubscriptionResolvers, "typing" | "presenceChanged">> = {
  typing: {
    subscribe: async (_r, { chatId }, ctx) => {
      const user = requireAuth(ctx);
      const id = parseId(chatId);
      await requireMember(ctx.app, id, user.id);
      return mapEvents(ctx, chatTopic(id), async (ev) => {
        if (ev.type !== "typing" || ev.user_id === user.id) return null;
        const u = await ctx.loaders.userById.load(ev.user_id);
        return u ? { typing: { user: u, typing: ev.typing } } : null;
      });
    },
  },

  presenceChanged: {
    subscribe: async (_r, { userIds }, ctx) => {
      requireAuth(ctx);
      const wanted = new Set(userIds.map(parseId));
      return mapEvents(ctx, PRESENCE_TOPIC, async (ev) => {
        if (ev.type !== "presence" || !wanted.has(ev.user_id)) return null;
        const u = await ctx.loaders.userById.load(ev.user_id);
        return u ? { presenceChanged: u } : null;
      });
    },
  },
};

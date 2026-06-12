import * as db from "../db.ts";
import { chatTopic, PRESENCE_TOPIC } from "../context.ts";
import type { SubscriptionResolvers } from "../generated/graphql.ts";
import { mapEvents, parseId, requireAuth, requireMember } from "./helpers.ts";

export const Subscription: SubscriptionResolvers = {
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
  receiptChanged: {
    subscribe: async (_r, { chatId }, ctx) => {
      const user = requireAuth(ctx);
      const id = parseId(chatId);
      await requireMember(ctx.app, id, user.id);
      return mapEvents(ctx, chatTopic(id), async (ev) => {
        if (ev.type !== "receipt") return null;
        const rows = await db.receiptsByMessageIds(ctx.app.pool, [ev.message_id]);
        const r = rows.find((x) => x.user_id === ev.user_id);
        return r ? { receiptChanged: r } : null;
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

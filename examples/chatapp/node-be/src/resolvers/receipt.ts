import * as db from "../db.ts";
import { chatTopic } from "../context.ts";
import { mapDb } from "../errors.ts";
import type { MutationResolvers, SubscriptionResolvers } from "../generated/graphql.ts";
import { mapEvents, parseId, requireAuth, requireMember } from "./helpers.ts";

export const mutation: Required<Pick<MutationResolvers, "markRead">> = {
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
};

export const subscription: Required<Pick<SubscriptionResolvers, "receiptChanged">> = {
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
};

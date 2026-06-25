import { mapForge } from "../errors.ts";
import type { MutationResolvers, QueryResolvers } from "../generated/graphql.ts";
import { requireAdmin, requireAuth } from "./helpers.ts";

export const query: Required<Pick<QueryResolvers, "reactionsEnabled" | "opsStats">> = {
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

export const mutation: Required<Pick<MutationResolvers, "setReactionsRollout" | "triggerFailingJob">> = {
  setReactionsRollout: async (_r, { percent }, ctx) => {
    requireAdmin(ctx);
    const pct = Math.min(Math.max(percent, 0), 100);
    try {
      await ctx.app.setReactionsRollout(pct);
    } catch (e) {
      throw mapForge(e);
    }
    return true;
  },

  triggerFailingJob: async (_r, _a, ctx) => {
    requireAdmin(ctx);
    try {
      await ctx.app.enqueueFailing();
    } catch (e) {
      throw mapForge(e);
    }
    return true;
  },
};

import * as db from "../db.ts";
import { err, mapDb, mapForge } from "../errors.ts";
import type { MutationResolvers, QueryResolvers } from "../generated/graphql.ts";
import { APIKEY_LIMIT, OTP_LIMIT, issueSession, requireAuth } from "./helpers.ts";

export const query: Required<Pick<QueryResolvers, "me">> = {
  me: async (_r, _a, ctx) => {
    if (!ctx.currentUser) return null;
    try {
      return await ctx.loaders.userById.load(ctx.currentUser.id);
    } catch (e) {
      throw mapDb(e);
    }
  },
};

export const mutation: Required<Pick<MutationResolvers, "signup" | "login" | "logout" | "logoutAll" | "createApiKey">> = {
  signup: async (_r, { username, displayName, password }, ctx) => {
    const app = ctx.app;
    const uname = username.trim();
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
    if (!creds) {
      // Verify against the decoy so an unknown username costs the same argon2 time
      // as a real one; otherwise the timing gap enumerates valid usernames.
      try {
        await app.forge.verifyPassword(password, app.decoyHash);
      } catch {
        /* discard: this verify exists only to equalize timing */
      }
      throw err("UNAUTHENTICATED", "invalid username or password");
    }
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
};

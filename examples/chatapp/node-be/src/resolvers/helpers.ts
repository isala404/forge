import * as db from "../db.ts";
import type { UserRow } from "../db.ts";
import {
  type AppCtx,
  type CurrentUser,
  type GqlContext,
  type RealtimeEvent,
  SESSION_IDLE_SECS,
  SESSION_ABSOLUTE_SECS,
} from "../context.ts";
import { err, mapDb, mapForge } from "../errors.ts";

export const SEND_LIMIT = { max: 5, perSeconds: 10 } as const;
export const OTP_LIMIT = { max: 10, perSeconds: 60 } as const;
export const UPLOAD_LIMIT = { max: 30, perSeconds: 60 } as const;
export const APIKEY_LIMIT = { max: 5, perSeconds: 3600 } as const;
export const UUID_RE = /^[0-9a-f]{8}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{4}-[0-9a-f]{12}$/i;

export function requireAuth(ctx: GqlContext): CurrentUser {
  if (!ctx.currentUser) throw err("UNAUTHENTICATED", "not authenticated");
  return ctx.currentUser;
}

// Gate the ops/admin mutations. The allowlist is a comma-separated list of user
// ids in ADMIN_USER_IDS. Unset means an empty allowlist, so these mutations are
// denied for everyone (fail closed), the right default for a demo that ships no
// roles system. The single entry "*" allows any authenticated user: a dev/demo
// convenience, never for production.
export function requireAdmin(ctx: GqlContext): CurrentUser {
  const user = requireAuth(ctx);
  const allowed = (process.env.ADMIN_USER_IDS ?? "")
    .split(",")
    .map((id) => id.trim())
    .some((id) => id === "*" || (id !== "" && id === user.id));
  if (!allowed) throw err("FORBIDDEN", "admin only");
  return user;
}

export function parseId(id: string): string {
  if (!UUID_RE.test(id)) throw err("INVALID", `not a valid id: ${id}`);
  return id;
}

export async function requireMember(app: AppCtx, chatId: string, userId: string): Promise<void> {
  let ok: boolean;
  try {
    ok = await db.isMember(app.pool, chatId, userId);
  } catch (e) {
    throw mapDb(e);
  }
  if (!ok) throw err("NOT_FOUND", "chat not found or not a member");
}

export async function loadUser(ctx: GqlContext, id: string): Promise<UserRow> {
  let u: UserRow | null;
  try {
    u = await ctx.loaders.userById.load(id);
  } catch (e) {
    throw mapDb(e);
  }
  if (!u) throw err("NOT_FOUND", "user not found");
  return u;
}

export interface SessionPayload {
  token: string;
  user: UserRow;
}

export async function issueSession(ctx: GqlContext, userId: string): Promise<SessionPayload> {
  const app = ctx.app;
  let token: string;
  try {
    token = await app.forge.createSession(userId, SESSION_IDLE_SECS, SESSION_ABSOLUTE_SECS);
  } catch (e) {
    throw mapForge(e);
  }
  const user = await loadUser(ctx, userId);
  return { token, user };
}

// How often a long-lived subscription re-checks that its session still validates.
const REVALIDATE_MS = 60_000;

// True if the principal's session still validates. API-key principals carry an
// empty token (no session to expire), so they always pass.
async function stillValid(ctx: GqlContext): Promise<boolean> {
  const user = ctx.currentUser;
  if (!user || !user.token) return true;
  try {
    return (await ctx.app.forge.validateSession(user.token)) === user.id;
  } catch {
    // Treat a backend error as still-valid: don't drop a happy-path stream on a
    // transient blip. A revoked session returns null (not an error) and ends it.
    return true;
  }
}

// Subscriptions: subscribe to the chat/presence topic, filter by event type,
// re-hydrate the domain object (via loaders), and yield the GraphQL payload.
// Closing the generator (graphql-ws calls return()) releases the JsSubscription.
export async function* mapEvents<T>(
  ctx: GqlContext,
  topic: string,
  mapFn: (event: RealtimeEvent) => Promise<T | null>,
): AsyncGenerator<T> {
  const source = ctx.app.subscribe(topic);
  let nextCheck = Date.now() + REVALIDATE_MS;
  try {
    for await (const event of source) {
      if (Date.now() >= nextCheck) {
        if (!(await stillValid(ctx))) return;
        nextCheck = Date.now() + REVALIDATE_MS;
      }
      let payload: T | null;
      try {
        payload = await mapFn(event);
      } catch {
        continue;
      }
      if (payload != null) yield payload;
    }
  } finally {
    await source.return?.();
  }
}

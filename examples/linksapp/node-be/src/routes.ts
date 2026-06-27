import { randomUUID } from "node:crypto";

import type { ForgeClient } from "forge-node";
import { Hono } from "hono";
import { cors } from "hono/cors";
import { streamSSE } from "hono/streaming";
import type { ContentfulStatusCode } from "hono/utils/http-status";
import QRCode from "qrcode";

import {
  HttpError,
  type Credentials,
  type Link,
  type LinkCreate,
  type LinkRecord,
  type OwnedLink,
  type UserRecord,
} from "./types.ts";
import {
  CLICKS_QUEUE,
  DEFAULT_MAX_LINKS,
  EXPIRE_QUEUE,
  RESERVED_SLUGS,
  SESSION_ABSOLUTE_SECS,
  SESSION_IDLE_SECS,
  SLUG_RE,
  bearerToken,
  clickTopic,
  clicksKey,
  envOr,
  ownerKey,
  publicUser,
  qrKey,
  randomSlug,
  slugKey,
  userEmailKey,
  userIdKey,
  validateCredentials,
  validateSlug,
  validateUrl,
} from "./utils.ts";
import { deleteLink } from "./worker.ts";

export type Bindings = {
  forge: ForgeClient;
};

export const api = new Hono<{ Bindings: Bindings }>();

async function readJson<T>(request: Request): Promise<T> {
  const contentLength = Number(request.headers.get("content-length") || "0");
  if (contentLength > 64 * 1024) throw new HttpError(413, "request body too large");

  const body = await request.text();
  if (!body) return {} as T;
  if (Buffer.byteLength(body) > 64 * 1024) throw new HttpError(413, "request body too large");

  try {
    return JSON.parse(body) as T;
  } catch {
    throw new HttpError(400, "invalid json body");
  }
}

api.use(
  "*",
  cors({
    origin: envOr("CORS_ORIGIN", "*"),
    allowMethods: ["GET", "POST", "PATCH", "DELETE", "OPTIONS"],
    allowHeaders: ["Content-Type", "Authorization"],
    maxAge: 86_400,
  }),
);

api.get("/healthz", (c) => c.text("ok"));

api.get("/api/meta", async (c) => {
  const forge = c.env.forge;
  const [customSlugs, depth] = await Promise.all([
    forge.flag("custom_slugs", false),
    forge.queueDepth(CLICKS_QUEUE),
  ]);

  return c.json({
    backend: "node",
    forge: forge.backendReport().map((line) => ({
      primitive: line.primitive,
      provider: line.provider,
      durable: line.durable,
      caveats: line.caveats,
    })),
    features: { customSlugs },
    clicksQueueDepth: {
      visible: depth.visible,
      inFlight: depth.inFlight,
      delayed: depth.delayed,
    },
  });
});

api.post("/api/signup", async (c) => {
  const forge = c.env.forge;
  const { email, password } = validateCredentials(await readJson<Credentials>(c.req.raw));

  const authLimit = await forge.rateLimitCheck("links-auth", email, 20, 60, true);
  if (!authLimit.allowed) throw new HttpError(429, "too many auth attempts; try again soon");

  const user: UserRecord = {
    id: randomUUID(),
    email,
    password_hash: await forge.hashPassword(password),
  };

  const inserted = await forge.kvSet(userEmailKey(email), JSON.stringify(user), null, true);
  if (!inserted) throw new HttpError(409, "email already registered");

  await forge.kvSet(userIdKey(user.id), JSON.stringify(user));

  const token = await forge.createSession(user.id, SESSION_IDLE_SECS, SESSION_ABSOLUTE_SECS);
  return c.json({ token, user: publicUser(user) }, 201);
});

api.post("/api/login", async (c) => {
  const forge = c.env.forge;
  const { email, password } = validateCredentials(await readJson<Credentials>(c.req.raw));

  const authLimit = await forge.rateLimitCheck("links-auth", email, 20, 60, true);
  if (!authLimit.allowed) throw new HttpError(429, "too many auth attempts; try again soon");

  const rawUser = await forge.kvGet(userEmailKey(email));
  if (!rawUser) throw new HttpError(401, "invalid email or password");
  const user = JSON.parse(rawUser) as UserRecord;

  const passwordOk = await forge.verifyPassword(password, user.password_hash);
  if (!passwordOk) throw new HttpError(401, "invalid email or password");

  const token = await forge.createSession(user.id, SESSION_IDLE_SECS, SESSION_ABSOLUTE_SECS);
  return c.json({ token, user: publicUser(user) });
});

api.post("/api/logout", async (c) => {
  await c.env.forge.revokeSession(bearerToken(c.req.header("authorization")));
  return c.body(null, 204);
});

api.get("/api/me", async (c) => {
  const forge = c.env.forge;
  const userId = await forge.validateSession(bearerToken(c.req.header("authorization")));
  if (!userId) throw new HttpError(401, "authentication required");

  const rawUser = await forge.kvGet(userIdKey(userId));
  if (!rawUser) throw new HttpError(401, "authentication required");

  return c.json({ user: publicUser(JSON.parse(rawUser) as UserRecord) });
});

api.get("/api/links", async (c) => {
  const forge = c.env.forge;
  const userId = await forge.validateSession(bearerToken(c.req.header("authorization")));
  if (!userId) throw new HttpError(401, "authentication required");

  const rawOwner = await forge.kvGet(ownerKey(userId));
  const owned: OwnedLink[] = rawOwner ? (JSON.parse(rawOwner) as OwnedLink[]) : [];

  const clickValues =
    owned.length > 0 ? await forge.kvMget(owned.map((l) => clicksKey(l.slug))) : [];

  const links: Link[] = owned.map((l, i) => ({
    slug: l.slug,
    url: l.url,
    createdAt: l.createdAt,
    expiresAt: l.expiresAt,
    clicks: parseInt(clickValues[i] ?? "0", 10) || 0,
  }));

  return c.json({ links });
});

api.post("/api/links", async (c) => {
  const forge = c.env.forge;
  const userId = await forge.validateSession(bearerToken(c.req.header("authorization")));
  if (!userId) throw new HttpError(401, "authentication required");

  const input = await readJson<LinkCreate>(c.req.raw);

  const url = validateUrl(input.url);

  const rawMaxLinks = await forge.configGet("max_links_per_user");
  const maxLinks =
    rawMaxLinks !== null ? parseInt(rawMaxLinks, 10) || DEFAULT_MAX_LINKS : DEFAULT_MAX_LINKS;

  const rawOwner = await forge.kvGet(ownerKey(userId));
  const ownerList: OwnedLink[] = rawOwner ? (JSON.parse(rawOwner) as OwnedLink[]) : [];
  if (ownerList.length >= maxLinks) throw new HttpError(409, "link limit reached");

  // Resolve the slug and reserve it atomically with SET NX.
  const customSlugsEnabled = await forge.flag("custom_slugs", false, userId);

  const now = new Date();
  const createdAt = now.toISOString();
  const ttlSeconds =
    typeof input.ttlSeconds === "number" && input.ttlSeconds > 0 ? input.ttlSeconds : null;
  const expiresAt = ttlSeconds !== null ? new Date(now.getTime() + ttlSeconds * 1000).toISOString() : null;

  let resolvedSlug = "";

  if (
    typeof input.slug === "string" &&
    input.slug.trim() !== "" &&
    customSlugsEnabled
  ) {
    // Custom slug path: validate once, attempt SET NX once
    resolvedSlug = validateSlug(input.slug.trim());
    const linkRec: LinkRecord = { slug: resolvedSlug, url, ownerId: userId, createdAt, expiresAt };
    const ok = await forge.kvSet(slugKey(resolvedSlug), JSON.stringify(linkRec), null, true);
    if (!ok) throw new HttpError(409, "slug already taken");
  } else {
    // Random slug path: retry up to 5 times
    let reserved = false;
    for (let attempt = 0; attempt < 5; attempt++) {
      resolvedSlug = randomSlug();
      const linkRec: LinkRecord = { slug: resolvedSlug, url, ownerId: userId, createdAt, expiresAt };
      if (await forge.kvSet(slugKey(resolvedSlug), JSON.stringify(linkRec), null, true)) {
        reserved = true;
        break;
      }
    }
    if (!reserved) throw new HttpError(409, "slug already taken");
  }

  // Prepend to keep the list newest-first.
  const ownedLink: OwnedLink = { slug: resolvedSlug, url, createdAt, expiresAt };
  ownerList.unshift(ownedLink);
  await forge.kvSet(ownerKey(userId), JSON.stringify(ownerList));

  const svg = await QRCode.toString(`/${resolvedSlug}`, { type: "svg", margin: 1, width: 160 });
  await forge.blobPut(qrKey(resolvedSlug), svg, "image/svg+xml");

  if (expiresAt !== null) {
    await forge.scheduleAt(
      new Date(expiresAt).getTime(),
      EXPIRE_QUEUE,
      JSON.stringify({ slug: resolvedSlug }),
    );
  }

  const link: Link = { slug: resolvedSlug, url, createdAt, expiresAt, clicks: 0 };
  return c.json(link, 201);
});

api.delete("/api/links/:slug", async (c) => {
  const forge = c.env.forge;
  const slug = c.req.param("slug");
  const userId = await forge.validateSession(bearerToken(c.req.header("authorization")));
  if (!userId) throw new HttpError(401, "authentication required");

  const raw = await forge.kvGet(slugKey(slug));
  if (!raw) throw new HttpError(404, "link not found");
  const rec = JSON.parse(raw) as LinkRecord;
  if (rec.ownerId !== userId) throw new HttpError(404, "link not found");

  await deleteLink(forge, slug);
  return c.body(null, 204);
});

api.get("/api/links/:slug/qr.svg", async (c) => {
  const forge = c.env.forge;
  const slug = c.req.param("slug");

  const svg = await forge.blobGet(qrKey(slug));
  if (!svg) throw new HttpError(404, "qr not found");

  return new Response(svg, { headers: { "content-type": "image/svg+xml" } });
});

api.get("/api/links/:slug/live", async (c) => {
  const forge = c.env.forge;
  const slug = c.req.param("slug");

  return streamSSE(c, async (stream) => {
    const sub = await forge.pubsubSubscribe(clickTopic(slug));
    stream.onAbort(() => {
      void sub.close();
    });
    for (;;) {
      const buf = await sub.next();
      if (buf === null) break;
      await stream.writeSSE({ data: buf.toString("utf8") });
    }
  });
});

// Must be last: catches /:slug redirects. Validates the slug against SLUG_RE
// and the reserved list before attempting a lookup.
api.get("/:slug", async (c) => {
  const forge = c.env.forge;
  const slug = c.req.param("slug");

  if (!SLUG_RE.test(slug) || RESERVED_SLUGS.has(slug)) {
    return c.json({ error: "link not found" }, 404);
  }

  const raw = await forge.kvGet(slugKey(slug));
  if (!raw) return c.json({ error: "link not found" }, 404);

  const rec = JSON.parse(raw) as LinkRecord;

  if (rec.expiresAt !== null && new Date(rec.expiresAt).getTime() < Date.now()) {
    return c.json({ error: "link not found" }, 404);
  }

  const rl = await forge.rateLimitCheck("redirect", slug, 600, 60, true);
  if (!rl.allowed) return c.json({ error: "too many requests" }, 429);

  await forge.kvIncr(clicksKey(slug), 1);
  await forge.queueEnqueue(CLICKS_QUEUE, JSON.stringify({ slug }), 3);

  return c.redirect(rec.url, 302);
});

api.notFound((c) => c.json({ error: "not found" }, 404));

api.onError((err, c) => {
  if (err instanceof HttpError) {
    return c.json({ error: err.message }, err.status as ContentfulStatusCode);
  }
  console.error("request error:", err);
  return c.json({ error: "internal error" }, 500);
});

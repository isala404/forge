import http from "node:http";
import { timingSafeEqual } from "node:crypto";
import type { IncomingMessage, ServerResponse } from "node:http";
import { createYoga } from "graphql-yoga";
import { useServer } from "graphql-ws/use/ws";
import { WebSocketServer } from "ws";

import { type AppCtx, type GqlContext, initAppCtx, userFromBearer } from "./context.ts";
import { buildSchema } from "./schema.ts";
import { runFanoutWorker, runReapWorker, runFailWorker, runScheduler } from "./worker.ts";

function envOr(key: string, def: string): string {
  return process.env[key] ?? def;
}

function corsHeaders(): Record<string, string> {
  return {
    "Access-Control-Allow-Origin": envOr("CORS_ORIGIN", "*"),
    "Access-Control-Allow-Methods": "GET, POST, PUT, OPTIONS",
    "Access-Control-Allow-Headers": "Content-Type, Authorization",
    "Access-Control-Max-Age": "86400",
  };
}

function send(res: ServerResponse, status: number, body: string): void {
  res.writeHead(status, { "content-type": "text/plain; charset=utf-8", ...corsHeaders() });
  res.end(body);
}

function sendJson(res: ServerResponse, status: number, body: unknown): void {
  res.writeHead(status, { "content-type": "application/json; charset=utf-8", ...corsHeaders() });
  res.end(JSON.stringify(body));
}

function hasOpsToken(req: IncomingMessage): boolean {
  const expected = process.env.OPS_TOKEN;
  const header = req.headers.authorization;
  if (!expected || typeof header !== "string" || !header.startsWith("Bearer ")) return false;
  const actualBytes = Buffer.from(header.slice(7));
  const expectedBytes = Buffer.from(expected);
  return actualBytes.length === expectedBytes.length && timingSafeEqual(actualBytes, expectedBytes);
}

// The forgelib binding has no built-in HTTP router, so /api/files is served
// here against the same presign contract. Binary goes through put/getBytes intact.
const BLOB_BODY_LIMIT = 50 * 1024 * 1024;

// Cache directive on presigned downloads, matching the Rust router: the client may
// cache the bytes but must revalidate each use (the ETag makes that a cheap 304);
// `private` keeps a shared proxy from caching one user's signed object.
const CACHE_CONTROL = "private, no-cache";

// Does an If-None-Match value match `etag` (already quoted)? Supports the
// comma-separated list form and the `*` wildcard, per RFC 9110.
function etagMatches(ifNoneMatch: string, etag: string): boolean {
  return ifNoneMatch
    .split(",")
    .map((c) => c.trim())
    .some((candidate) => candidate === "*" || candidate === etag);
}

function httpEtag(etag: string): string {
  return etag.startsWith('"') && etag.endsWith('"') ? etag : `"${etag}"`;
}

function readBody(req: IncomingMessage, limit: number): Promise<Buffer> {
  return new Promise((resolve, reject) => {
    const chunks: Buffer[] = [];
    let total = 0;
    req.on("data", (c: Buffer) => {
      total += c.length;
      if (total > limit) {
        reject(new Error("body too large"));
        req.destroy();
        return;
      }
      chunks.push(c);
    });
    req.on("end", () => resolve(Buffer.concat(chunks)));
    req.on("error", reject);
  });
}

export interface RunningServer {
  server: http.Server;
  app: AppCtx;
  port: number;
  close(): Promise<void>;
}

export async function startServer(port = parseInt(envOr("PORT", "8082"), 10)): Promise<RunningServer> {
  const app = await initAppCtx();
  const schema = buildSchema();

  let stopping = false;
  const stopped = (): boolean => stopping;
  runFanoutWorker(app, stopped);
  runReapWorker(app, stopped);
  runFailWorker(app, stopped);
  runScheduler(app, stopped);

  async function makeContext(authorization: string | undefined): Promise<GqlContext> {
    const currentUser = await userFromBearer(app, authorization);
    const loaders = app.makeLoaders(currentUser?.id ?? null);
    return { app, loaders, currentUser };
  }

  const yoga = createYoga<Record<string, never>, GqlContext>({
    schema,
    graphqlEndpoint: "/graphql",
    landingPage: false,
    graphiql: false,
    maskedErrors: false,
    cors: {
      origin: envOr("CORS_ORIGIN", "*"),
      methods: ["GET", "POST", "OPTIONS"],
      allowedHeaders: ["Content-Type", "Authorization"],
    },
    context: ({ request }) => makeContext(request.headers.get("authorization") ?? undefined),
  });

  const server = http.createServer(async (req, res) => {
    try {
      const url = new URL(req.url ?? "/", `http://${req.headers.host || "localhost"}`);
      const path = url.pathname;

      if (req.method === "OPTIONS") {
        res.writeHead(204, corsHeaders());
        return void res.end();
      }
      if (path === "/healthz") return send(res, 200, "ok");
      if (path === "/internal/forge/diagnostics") {
        if (req.method !== "GET") return send(res, 405, "method not allowed");
        if (!process.env.OPS_TOKEN) return send(res, 404, "not found");
        if (!hasOpsToken(req)) return send(res, 403, "forbidden");
        const [runtime, scheduler] = await Promise.all([
          app.forge.diagnostics(2),
          app.forge.schedulerDiagnostics(),
        ]);
        return sendJson(res, 200, { runtime, scheduler });
      }
      if (path === "/graphql") return void yoga.handle(req, res);
      if (path.startsWith("/api/files")) {
        const key = url.searchParams.get("key");
        const expires = parseInt(url.searchParams.get("expires") || "", 10);
        const maxBytes = parseInt(url.searchParams.get("max_bytes") || "0", 10) || 0;
        const sig = url.searchParams.get("sig");
        if (!key || !sig || !Number.isFinite(expires)) return send(res, 400, "missing params");

        const method = req.method === "PUT" ? "PUT" : req.method === "GET" ? "GET" : null;
        if (!method) {
          res.writeHead(405, corsHeaders());
          return void res.end();
        }

        let ok: boolean;
        try {
          ok = await app.forge.blobVerifyPresign(method, key, expires, maxBytes, sig);
        } catch (e) {
          return send(res, 403, "presign check failed: " + (e as Error).message);
        }
        if (!ok) return send(res, 403, "forbidden");

        if (method === "PUT") {
          // Enforce the signed max_bytes while reading, so an oversized body is
          // rejected before it is buffered.
          const cap = maxBytes > 0 ? Math.min(maxBytes, BLOB_BODY_LIMIT) : BLOB_BODY_LIMIT;
          let body: Buffer;
          try {
            body = await readBody(req, cap);
          } catch {
            return send(res, 413, "upload exceeds signed max_bytes");
          }
          const contentType = req.headers["content-type"] || "application/octet-stream";
          try {
            await app.forge.blobPutBytes(key, body, contentType);
          } catch (e) {
            return send(res, 500, "upload failed: " + (e as Error).message);
          }
          res.writeHead(200, corsHeaders());
          res.end();
          return;
        }

        // One head() gives both the content type and the ETag for conditional
        // requests, matching the Rust router.
        let contentType = "application/octet-stream";
        let etag: string | null = null;
        try {
          const info = await app.forge.blobHead(key);
          if (info) {
            if (info.contentType) contentType = info.contentType;
            etag = httpEtag(info.etag);
          }
        } catch {
          /* keep defaults; a head failure just means no conditional request support */
        }

        const inm = req.headers["if-none-match"];
        if (etag && typeof inm === "string" && etagMatches(inm, etag)) {
          res.writeHead(304, {
            etag,
            "cache-control": CACHE_CONTROL,
            ...corsHeaders(),
          });
          return void res.end();
        }

        let bytes: Buffer | null;
        try {
          bytes = await app.forge.blobGetBytes(key);
        } catch (e) {
          return send(res, 500, "download failed: " + (e as Error).message);
        }
        if (bytes === null) return send(res, 404, "not found");

        // Match Forge's own blob router: never let a served blob render inline or
        // be MIME-sniffed, so uploaded HTML/SVG cannot run on the backend origin.
        const headers: Record<string, string> = {
          "content-type": contentType,
          "content-length": String(bytes.length),
          "content-disposition": "attachment",
          "x-content-type-options": "nosniff",
          "cache-control": CACHE_CONTROL,
          ...corsHeaders(),
        };
        if (etag) headers.etag = etag;
        res.writeHead(200, headers);
        res.end(bytes);
        return;
      }
      return send(res, 404, "not found");
    } catch (e) {
      console.error("request error:", e);
      if (!res.headersSent) send(res, 500, "internal error");
      else res.end();
    }
  });

  // WS authenticates from connectionParams.authorization (Bearer), per the contract.
  // A provided-but-invalid token is rejected here rather than opening an anonymous
  // socket; an absent token still connects anonymously (unchanged).
  const wsServer = new WebSocketServer({ server, path: "/graphql" });
  useServer(
    {
      schema,
      context: async (wsCtx): Promise<GqlContext> => {
        const params = (wsCtx.connectionParams ?? {}) as Record<string, unknown>;
        const auth = params.authorization ?? params.Authorization;
        const header = typeof auth === "string" ? auth : undefined;
        const gqlCtx = await makeContext(header);
        if (header && !gqlCtx.currentUser) {
          throw new Error("UNAUTHENTICATED: invalid token");
        }
        return gqlCtx;
      },
    },
    wsServer,
  );

  const host = envOr("HOST", "127.0.0.1");
  await new Promise<void>((resolve) => server.listen(port, host, resolve));
  const addr = server.address();
  const boundPort = typeof addr === "object" && addr ? addr.port : port;
  // The leading token is parsed by the integration suite to learn the bound port.
  console.log(`chatapp-node-be listening on http://${host}:${boundPort}`);
  console.log(`READY ${boundPort}`);

  const close = async (): Promise<void> => {
    stopping = true;
    await new Promise<void>((resolve) => wsServer.close(() => resolve()));
    await new Promise<void>((resolve) => server.close(() => resolve()));
    // The application pool may point at Forge's embedded PostgreSQL. Release every
    // dependent application connection before Forge tears that server down.
    await app.pool.end();
    await app.forge.close(30);
  };

  return { server, app, port: boundPort, close };
}

const isMain = process.argv[1] && import.meta.url === `file://${process.argv[1]}`;
if (isMain) {
  startServer()
    .then((running) => {
      const shutdown = (): void => {
        void running.close().then(() => process.exit(0));
        setTimeout(() => process.exit(0), 2000);
      };
      process.on("SIGINT", shutdown);
      process.on("SIGTERM", shutdown);
    })
    .catch((e) => {
      console.error("fatal:", e);
      process.exit(1);
    });
}

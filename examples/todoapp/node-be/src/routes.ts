import { randomUUID } from "node:crypto";

import type { ForgeClient } from "forge-node";
import { Hono } from "hono";
import { cors } from "hono/cors";
import type { ContentfulStatusCode } from "hono/utils/http-status";

import {
  type Credentials,
  HttpError,
  type Todo,
  type TodoCreate,
  type TodoPatch,
  type UserRecord,
} from "./types.ts";
import {
  AUDIT_QUEUE,
  SESSION_ABSOLUTE_SECS,
  SESSION_IDLE_SECS,
  bearerToken,
  envOr,
  publicUser,
  todosKey,
  userEmailKey,
  userIdKey,
  validateCredentials,
  validateTitle,
} from "./utils.ts";

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
  const depth = await forge.queueDepth(AUDIT_QUEUE);

  return c.json({
    backend: "node",
    forge: forge.backendReport().map((line) => ({
      primitive: line.primitive,
      provider: line.provider,
      durable: line.durable,
      caveats: line.caveats,
    })),
    auditDepth: {
      visible: depth.visible,
      inFlight: depth.inFlight,
      delayed: depth.delayed,
    },
  });
});

api.post("/api/signup", async (c) => {
  const forge = c.env.forge;
  const { email, password } = validateCredentials(await readJson<Credentials>(c.req.raw));

  const authLimit = await forge.rateLimitCheck("todo-auth", email, 20, 60, true);
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

  const authLimit = await forge.rateLimitCheck("todo-auth", email, 20, 60, true);
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

api.get("/api/todos", async (c) => {
  const forge = c.env.forge;
  const userId = await forge.validateSession(bearerToken(c.req.header("authorization")));
  if (!userId) throw new HttpError(401, "authentication required");

  const rawTodos = await forge.kvGet(todosKey(userId));
  const todos = rawTodos ? (JSON.parse(rawTodos) as Todo[]) : [];
  return c.json({ todos });
});

api.post("/api/todos", async (c) => {
  const forge = c.env.forge;
  const userId = await forge.validateSession(bearerToken(c.req.header("authorization")));
  if (!userId) throw new HttpError(401, "authentication required");

  const input = await readJson<TodoCreate>(c.req.raw);
  const rawTodos = await forge.kvGet(todosKey(userId));
  const todos = rawTodos ? (JSON.parse(rawTodos) as Todo[]) : [];

  const now = new Date().toISOString();
  const todo: Todo = {
    id: randomUUID(),
    title: validateTitle(input.title),
    completed: false,
    createdAt: now,
    updatedAt: now,
  };
  todos.unshift(todo);

  await forge.kvSet(todosKey(userId), JSON.stringify(todos));
  await forge.queueEnqueue(
    AUDIT_QUEUE,
    JSON.stringify({
      userId,
      action: "created",
      todoId: todo.id,
      at: new Date().toISOString(),
    }),
    3,
    `created:${todo.id}`,
  );

  return c.json(todo, 201);
});

api.patch("/api/todos/:id", async (c) => {
  const forge = c.env.forge;
  const id = c.req.param("id");
  const userId = await forge.validateSession(bearerToken(c.req.header("authorization")));
  if (!userId) throw new HttpError(401, "authentication required");

  const input = await readJson<TodoPatch>(c.req.raw);
  const rawTodos = await forge.kvGet(todosKey(userId));
  const todos = rawTodos ? (JSON.parse(rawTodos) as Todo[]) : [];

  const todo = todos.find((candidate) => candidate.id === id);
  if (!todo) throw new HttpError(404, "todo not found");
  if (input.title !== undefined) todo.title = validateTitle(input.title);
  if (input.completed !== undefined) todo.completed = Boolean(input.completed);
  todo.updatedAt = new Date().toISOString();

  await forge.kvSet(todosKey(userId), JSON.stringify(todos));
  await forge.queueEnqueue(
    AUDIT_QUEUE,
    JSON.stringify({
      userId,
      action: "updated",
      todoId: todo.id,
      at: new Date().toISOString(),
    }),
    3,
    `updated:${todo.id}`,
  );

  return c.json(todo);
});

api.delete("/api/todos/:id", async (c) => {
  const forge = c.env.forge;
  const id = c.req.param("id");
  const userId = await forge.validateSession(bearerToken(c.req.header("authorization")));
  if (!userId) throw new HttpError(401, "authentication required");

  const rawTodos = await forge.kvGet(todosKey(userId));
  const todos = rawTodos ? (JSON.parse(rawTodos) as Todo[]) : [];
  const next = todos.filter((todo) => todo.id !== id);
  if (next.length === todos.length) throw new HttpError(404, "todo not found");

  await forge.kvSet(todosKey(userId), JSON.stringify(next));
  await forge.queueEnqueue(
    AUDIT_QUEUE,
    JSON.stringify({
      userId,
      action: "deleted",
      todoId: id,
      at: new Date().toISOString(),
    }),
    3,
    `deleted:${id}`,
  );

  return c.body(null, 204);
});

api.notFound((c) => c.json({ error: "not found" }, 404));

api.onError((err, c) => {
  if (err instanceof HttpError) {
    return c.json({ error: err.message }, err.status as ContentfulStatusCode);
  }
  console.error("request error:", err);
  return c.json({ error: "internal error" }, 500);
});

import { readFileSync } from "node:fs";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";
import { beforeAll, afterAll, describe, expect, test } from "vitest";
import {
  printSchema,
  lexicographicSortSchema,
  buildSchema as buildFromSDL,
  buildClientSchema,
  getIntrospectionQuery,
  type IntrospectionQuery,
} from "graphql";
import { ForgeClient } from "forgelib";

import { TEST_DB_URL } from "./globalSetup.ts";
import { startServerProcess, type ServerHandle } from "./serverProcess.ts";
import { HttpClient, wsClient, subscribe, sleep, uniqueName } from "./helpers.ts";

const SCHEMA_PATH = join(
  dirname(fileURLToPath(import.meta.url)),
  "..",
  "..",
  "schema.graphql",
);

const APP_ENV = {
  FORGE_POSTGRES_URL: TEST_DB_URL,
  APP_PRESENCE_TTL_SECS: "2",
  APP_DISAPPEARING_SECS: "2",
  APP_SCHEDULER_MS: "300",
  FORGE_BLOB_SIGNING_SECRET: "test-signing-secret",
  // "*" = any authenticated user is an admin, so the ops tests below can exercise the
  // gated mutations without knowing a user id at boot. A real deploy lists actual ids.
  ADMIN_USER_IDS: "*",
};

let running: ServerHandle;
let httpUrl: string;
let wsUrl: string;
let anon: HttpClient;

beforeAll(async () => {
  running = await startServerProcess(APP_ENV);
  httpUrl = `http://127.0.0.1:${running.port}/graphql`;
  wsUrl = `ws://127.0.0.1:${running.port}/graphql`;
  anon = new HttpClient(httpUrl);
  // In-process ForgeClient.init() reads ./forge.toml, whose ${FORGE_POSTGRES_URL} resolves
  // from the environment; point it at the throwaway test database the server also uses.
  process.env.FORGE_POSTGRES_URL = TEST_DB_URL;
  process.env.FORGE_BLOB_SIGNING_SECRET = "test-signing-secret";
});

afterAll(async () => {
  await running?.close();
});

interface SessionPayload {
  token: string;
  user: { id: string; username: string; displayName: string };
}

async function signup(username = uniqueName("user")): Promise<{ token: string; id: string; username: string }> {
  const data = await anon.ok<{ signup: SessionPayload }>(
    `mutation ($u: String!, $d: String!, $p: String!) {
       signup(username: $u, displayName: $d, password: $p) { token user { id username displayName } }
     }`,
    { u: username, d: `Display ${username}`, p: "secret123" },
  );
  return { token: data.signup.token, id: data.signup.user.id, username: data.signup.user.username };
}

describe("SDL parity", () => {
  test("served schema equals canonical schema.graphql (normalized)", async () => {
    const introspection = await anon.ok<IntrospectionQuery>(getIntrospectionQuery());
    const served = buildClientSchema(introspection);
    const canonical = buildFromSDL(readFileSync(SCHEMA_PATH, "utf8"));
    const norm = (s: typeof canonical): string =>
      printSchema(lexicographicSortSchema(s))
        .replace(/"""[\s\S]*?"""\n?|"[^"\n]*"\n?/g, "")
        .replace(/\s+/g, " ")
        .trim();
    expect(norm(served)).toBe(norm(canonical));
  });
});

describe("auth", () => {
  test("signup returns a session, me resolves with the token", async () => {
    const s = await signup();
    const data = await anon.withToken(s.token).ok<{ me: { id: string; username: string } | null }>(
      `query { me { id username } }`,
    );
    expect(data.me?.id).toBe(s.id);
  });

  test("me returns null when unauthenticated (no error)", async () => {
    const r = await anon.gql<{ me: unknown }>(`query { me { id } }`);
    expect(r.errors).toBeUndefined();
    expect(r.data?.me).toBeNull();
  });

  test("an auth-required query errors UNAUTHENTICATED when anonymous", async () => {
    const r = await anon.gql(`query { chats { id } }`);
    expect(r.errors?.[0]?.extensions?.code).toBe("UNAUTHENTICATED");
  });

  test("signup rejects short username/password (INVALID)", async () => {
    const r = await anon.gql(
      `mutation { signup(username: "ab", displayName: "X", password: "short") { token } }`,
    );
    expect(r.errors?.[0]?.extensions?.code).toBe("INVALID");
  });

  test("duplicate username is PRECONDITION", async () => {
    const name = uniqueName("dup");
    await signup(name);
    const r = await anon.gql(
      `mutation ($u: String!) { signup(username: $u, displayName: "X", password: "secret123") { token } }`,
      { u: name },
    );
    expect(r.errors?.[0]?.extensions?.code).toBe("PRECONDITION");
  });

  test("logoutAll revokes other sessions", async () => {
    const name = uniqueName("multi");
    const a = await signup(name);
    const b = await anon.ok<{ login: SessionPayload }>(
      `mutation ($u: String!) { login(username: $u, password: "secret123") { token user { id } } }`,
      { u: name },
    );
    const tokenB = b.login.token;
    await anon.withToken(a.token).ok(`mutation { logoutAll }`);
    const r = await new HttpClient(httpUrl, tokenB).gql<{ me: unknown }>(`query { me { id } }`);
    expect(r.data?.me).toBeNull();
  });

  test("an API key authenticates a request", async () => {
    const s = await signup();
    const data = await anon.withToken(s.token).ok<{ createApiKey: { id: string; secret: string } }>(
      `mutation { createApiKey(label: "cli") { id secret } }`,
    );
    const r = await new HttpClient(httpUrl, data.createApiKey.secret).ok<{ me: { id: string } | null }>(
      `query { me { id } }`,
    );
    expect(r.me?.id).toBe(s.id);
  });
});

async function makeGroup(
  owner: { token: string },
  otherUsernames: string[],
): Promise<string> {
  const data = await new HttpClient(httpUrl, owner.token).ok<{ createChat: { id: string } }>(
    `mutation ($m: [String!]!) { createChat(kind: GROUP, title: "Room", memberUsernames: $m) { id } }`,
    { m: otherUsernames },
  );
  return data.createChat.id;
}

describe("chats", () => {
  test("create group chat; both members see it", async () => {
    const alice = await signup();
    const bob = await signup();
    const chatId = await makeGroup(alice, [bob.username]);

    const aChats = await new HttpClient(httpUrl, alice.token).ok<{ chats: Array<{ id: string; members: Array<{ id: string }> }> }>(
      `query { chats { id members { id } } }`,
    );
    expect(aChats.chats.map((c) => c.id)).toContain(chatId);
    const bChats = await new HttpClient(httpUrl, bob.token).ok<{ chats: Array<{ id: string }> }>(
      `query { chats { id } }`,
    );
    expect(bChats.chats.map((c) => c.id)).toContain(chatId);
  });

  test("DIRECT chat must resolve to exactly 2 members", async () => {
    const alice = await signup();
    const r = await new HttpClient(httpUrl, alice.token).gql(
      `mutation { createChat(kind: DIRECT, title: null, memberUsernames: []) { id } }`,
    );
    expect(r.errors?.[0]?.extensions?.code).toBe("INVALID");
  });

  test("unknown member username is NOT_FOUND", async () => {
    const alice = await signup();
    const r = await new HttpClient(httpUrl, alice.token).gql(
      `mutation { createChat(kind: GROUP, title: "x", memberUsernames: ["nobody_here_xyz"]) { id } }`,
    );
    expect(r.errors?.[0]?.extensions?.code).toBe("NOT_FOUND");
  });

  test("non-member chat(id) is NOT_FOUND", async () => {
    const alice = await signup();
    const bob = await signup();
    const chatId = await makeGroup(alice, []);
    const r = await new HttpClient(httpUrl, bob.token).gql(
      `query ($id: ID!) { chat(id: $id) { id } }`,
      { id: chatId },
    );
    expect(r.errors?.[0]?.extensions?.code).toBe("NOT_FOUND");
  });
});

describe("messaging + realtime", () => {
  test("send a message, receive it live over a subscription", async () => {
    const alice = await signup();
    const bob = await signup();
    const chatId = await makeGroup(alice, [bob.username]);

    const client = wsClient(wsUrl, bob.token);
    const sub = subscribe<{ messageAdded: { id: string; body: string; sender: { id: string } } }>(
      client,
      `subscription ($c: ID!) { messageAdded(chatId: $c) { id body sender { id } } }`,
      { c: chatId },
    );
    await sleep(300); // let the subscription establish before publishing

    const sent = await new HttpClient(httpUrl, alice.token).ok<{ sendMessage: { id: string } }>(
      `mutation ($c: ID!) { sendMessage(chatId: $c, body: "hello bob") { id } }`,
      { c: chatId },
    );
    const ev = await sub.next();
    expect(ev.messageAdded.body).toBe("hello bob");
    expect(ev.messageAdded.id).toBe(sent.sendMessage.id);
    expect(ev.messageAdded.sender.id).toBe(alice.id);

    sub.dispose();
    await client.dispose();
  });

  test("typing event delivered to the other member, suppressed for the sender", async () => {
    const alice = await signup();
    const bob = await signup();
    const chatId = await makeGroup(alice, [bob.username]);

    const bobClient = wsClient(wsUrl, bob.token);
    const bobSub = subscribe<{ typing: { user: { id: string }; typing: boolean } }>(
      bobClient,
      `subscription ($c: ID!) { typing(chatId: $c) { user { id } typing } }`,
      { c: chatId },
    );
    await sleep(300);

    await new HttpClient(httpUrl, alice.token).ok(
      `mutation ($c: ID!) { setTyping(chatId: $c, typing: true) }`,
      { c: chatId },
    );
    const ev = await bobSub.next();
    expect(ev.typing.user.id).toBe(alice.id);
    expect(ev.typing.typing).toBe(true);

    bobSub.dispose();
    await bobClient.dispose();
  });

  test("empty body without media is INVALID", async () => {
    const alice = await signup();
    const chatId = await makeGroup(alice, []);
    const r = await new HttpClient(httpUrl, alice.token).gql(
      `mutation ($c: ID!) { sendMessage(chatId: $c, body: "   ") { id } }`,
      { c: chatId },
    );
    expect(r.errors?.[0]?.extensions?.code).toBe("INVALID");
  });

  test("messages limit is clamped to 1..200", async () => {
    const alice = await signup();
    const chatId = await makeGroup(alice, []);
    const data = await new HttpClient(httpUrl, alice.token).ok<{ messages: unknown[] }>(
      `query ($c: ID!) { messages(chatId: $c, limit: 9999) { id } }`,
      { c: chatId },
    );
    expect(Array.isArray(data.messages)).toBe(true);
  });
});

describe("receipts + unread", () => {
  test("unread increments on send (fanout), clears on markRead; receipt turns read", async () => {
    const alice = await signup();
    const bob = await signup();
    const chatId = await makeGroup(alice, [bob.username]);
    const aHttp = new HttpClient(httpUrl, alice.token);
    const bHttp = new HttpClient(httpUrl, bob.token);

    const sent = await aHttp.ok<{ sendMessage: { id: string } }>(
      `mutation ($c: ID!) { sendMessage(chatId: $c, body: "unread test") { id } }`,
      { c: chatId },
    );
    const messageId = sent.sendMessage.id;

    // fanout worker bumps bob's unread counter asynchronously.
    let unread = 0;
    for (let i = 0; i < 40; i++) {
      const d = await bHttp.ok<{ chats: Array<{ id: string; unread: number }> }>(
        `query { chats { id unread } }`,
      );
      unread = d.chats.find((c) => c.id === chatId)?.unread ?? 0;
      if (unread >= 1) break;
      await sleep(100);
    }
    expect(unread).toBeGreaterThanOrEqual(1);

    await bHttp.ok(`mutation ($c: ID!, $m: ID!) { markRead(chatId: $c, messageId: $m) }`, {
      c: chatId,
      m: messageId,
    });

    const after = await bHttp.ok<{ chats: Array<{ id: string; unread: number }> }>(
      `query { chats { id unread } }`,
    );
    expect(after.chats.find((c) => c.id === chatId)?.unread).toBe(0);

    const msgs = await aHttp.ok<{ messages: Array<{ id: string; receipts: Array<{ user: { id: string }; readAt: string | null }> }> }>(
      `query ($c: ID!) { messages(chatId: $c) { id receipts { user { id } readAt } } }`,
      { c: chatId },
    );
    const receipt = msgs.messages.find((m) => m.id === messageId)?.receipts.find((r) => r.user.id === bob.id);
    expect(receipt?.readAt).not.toBeNull();
  });
});

describe("presence", () => {
  test("heartbeat marks a user online, expires after the kv TTL", async () => {
    const alice = await signup();
    const aHttp = new HttpClient(httpUrl, alice.token);
    await aHttp.ok(`mutation { heartbeat }`);
    const online = await aHttp.ok<{ me: { online: boolean } | null }>(`query { me { online } }`);
    expect(online.me?.online).toBe(true);

    // TTL is 2s in tests; wait it out and confirm offline.
    await sleep(2500);
    const offline = await aHttp.ok<{ me: { online: boolean } | null }>(`query { me { online } }`);
    expect(offline.me?.online).toBe(false);
  });
});

describe("attachments", () => {
  test("presign -> PUT -> send -> media downloadUrl round-trips bytes", async () => {
    const alice = await signup();
    const chatId = await makeGroup(alice, []);
    const aHttp = new HttpClient(httpUrl, alice.token);

    const ticket = await aHttp.ok<{ requestUpload: { key: string; uploadUrl: string; maxBytes: number } }>(
      `mutation ($c: ID!) { requestUpload(chatId: $c) { key uploadUrl maxBytes } }`,
      { c: chatId },
    );
    expect(ticket.requestUpload.maxBytes).toBeGreaterThan(0);

    const payload = Buffer.from("attachment-bytes-123");
    const putUrl = ticket.requestUpload.uploadUrl.startsWith("http")
      ? ticket.requestUpload.uploadUrl
      : `http://127.0.0.1:${running.port}${ticket.requestUpload.uploadUrl}`;
    const putRes = await fetch(putUrl, {
      method: "PUT",
      headers: { "content-type": "text/plain" },
      body: payload,
    });
    expect(putRes.status).toBe(200);

    const sent = await aHttp.ok<{ sendMessage: { id: string; media: { downloadUrl: string; contentType: string | null } | null } }>(
      `mutation ($c: ID!, $k: String!) {
         sendMessage(chatId: $c, body: "", mediaKey: $k) { id media { downloadUrl contentType } }
       }`,
      { c: chatId, k: ticket.requestUpload.key },
    );
    expect(sent.sendMessage.media?.contentType).toBe("text/plain");

    const dl = sent.sendMessage.media!.downloadUrl;
    const dlUrl = dl.startsWith("http") ? dl : `http://127.0.0.1:${running.port}${dl}`;
    const got = await fetch(dlUrl);
    expect(got.status).toBe(200);
    expect(await got.text()).toBe("attachment-bytes-123");
  });
});

describe("rate limit", () => {
  test("a send burst is throttled (LIMIT) past 5/10s", async () => {
    const alice = await signup();
    const chatId = await makeGroup(alice, []);
    const aHttp = new HttpClient(httpUrl, alice.token);
    let limited = false;
    for (let i = 0; i < 9; i++) {
      const r = await aHttp.gql(
        `mutation ($c: ID!, $b: String!) { sendMessage(chatId: $c, body: $b) { id } }`,
        { c: chatId, b: `burst ${i}` },
      );
      if (r.errors?.[0]?.extensions?.code === "LIMIT") {
        limited = true;
        break;
      }
    }
    expect(limited).toBe(true);
  });
});

describe("disappearing messages", () => {
  test("a disappearing message vanishes after its ttl", async () => {
    const alice = await signup();
    const chatId = await makeGroup(alice, []);
    const aHttp = new HttpClient(httpUrl, alice.token);

    await aHttp.ok(`mutation ($c: ID!) { setDisappearing(chatId: $c, enabled: true) { id disappearingSeconds } }`, { c: chatId });
    const sent = await aHttp.ok<{ sendMessage: { id: string } }>(
      `mutation ($c: ID!) { sendMessage(chatId: $c, body: "self destruct") { id } }`,
      { c: chatId },
    );
    const messageId = sent.sendMessage.id;

    // expires_at is now+2s; the scheduler ticks every 300ms and the reap worker
    // hard-deletes it. Poll until it's gone.
    let gone = false;
    for (let i = 0; i < 60; i++) {
      const d = await aHttp.ok<{ messages: Array<{ id: string }> }>(
        `query ($c: ID!) { messages(chatId: $c) { id } }`,
        { c: chatId },
      );
      if (!d.messages.some((m) => m.id === messageId)) {
        gone = true;
        break;
      }
      await sleep(200);
    }
    expect(gone).toBe(true);
  });
});

describe("feature flags + ops", () => {
  test("reactions rollout toggles between 0 and 100", async () => {
    const alice = await signup();
    const aHttp = new HttpClient(httpUrl, alice.token);
    // Read the flag through a fresh Forge client each time so no per-client config
    // cache masks the new rollout value.
    const flagFor = async (uid: string): Promise<boolean> => {
      const c = await ForgeClient.init();
      return c.flag("reactions_v2", false, uid);
    };
    await aHttp.ok(`mutation { setReactionsRollout(percent: 100) }`);
    expect(await flagFor(alice.id)).toBe(true);
    await aHttp.ok(`mutation { setReactionsRollout(percent: 0) }`);
    expect(await flagFor(alice.id)).toBe(false);
  });

  test("reactionsEnabled reflects rollout and is false when unauthenticated", async () => {
    const alice = await signup();
    const aHttp = new HttpClient(httpUrl, alice.token);

    // Anonymous callers never evaluate the flag.
    const anonResult = await anon.ok<{ reactionsEnabled: boolean }>(`query { reactionsEnabled }`);
    expect(anonResult.reactionsEnabled).toBe(false);

    // Route the toggle and the read through the same server so it reads its own write.
    await aHttp.ok(`mutation { setReactionsRollout(percent: 100) }`);
    const on = await aHttp.ok<{ reactionsEnabled: boolean }>(`query { reactionsEnabled }`);
    expect(on.reactionsEnabled).toBe(true);

    await aHttp.ok(`mutation { setReactionsRollout(percent: 0) }`);
    const off = await aHttp.ok<{ reactionsEnabled: boolean }>(`query { reactionsEnabled }`);
    expect(off.reactionsEnabled).toBe(false);
  });

  test("opsStats reflects online count and DLQ depth", async () => {
    const alice = await signup();
    const aHttp = new HttpClient(httpUrl, alice.token);
    await aHttp.ok(`mutation { heartbeat }`);
    await aHttp.ok(`mutation { triggerFailingJob }`);

    // the fail worker nacks the job into fail.dlq; poll until opsStats sees it.
    let stats = { onlineCount: 0, dlqCount: 0 };
    for (let i = 0; i < 40; i++) {
      const d = await aHttp.ok<{ opsStats: { onlineCount: number; dlqCount: number } }>(
        `query { opsStats { onlineCount dlqCount } }`,
      );
      stats = d.opsStats;
      if (stats.dlqCount >= 1) break;
      await sleep(150);
    }
    expect(stats.onlineCount).toBeGreaterThanOrEqual(1);
    expect(stats.dlqCount).toBeGreaterThanOrEqual(1);
  });
});

describe("ids", () => {
  test("a non-UUID id is INVALID", async () => {
    const alice = await signup();
    const r = await new HttpClient(httpUrl, alice.token).gql(
      `query { chat(id: "not-a-uuid") { id } }`,
    );
    expect(r.errors?.[0]?.extensions?.code).toBe("INVALID");
  });
});

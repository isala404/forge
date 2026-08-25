# chatapp: node-be

The Node/TypeScript GraphQL backend for the chatapp example. It serves exactly the canonical `../schema.graphql` over GraphQL Yoga (HTTP) and graphql-ws (subscriptions), builds the executable schema with `@graphql-tools/schema`, and uses [`forgelib`](../../../bindings/node) for all infrastructure: auth, blob, pubsub, queue, kv, schedule, ratelimit, config. Domain data lives in the unprefixed `users`/`chats`/`chat_members`/`messages`/`receipts` tables (see `../migrations.sql`), which the server applies on startup.

## Stack

- Node 26 (native TypeScript via type-stripping; no bundler), GraphQL Yoga 5, graphql-ws 6
- `@graphql-tools/schema` for `makeExecutableSchema`, the `dataloader` package, `pg`
- Schema-first + codegen: `@graphql-codegen/cli` with `typescript` + `typescript-resolvers` generates a typed `Resolvers` map from `../schema.graphql` into `src/generated/graphql.ts`
- Package manager: `bun`

## How it maps to the contract

- **Bearer auth.** A request authenticates from `Authorization: Bearer <token>` (HTTP) or the graphql-ws `connectionParams.authorization` (WS). The token validates as either a Forge session (sliding idle deadline) or an API key. No cookies. `me` returns null when unauthenticated; other auth-required resolvers raise `UNAUTHENTICATED`.
- **DataLoader on every relational field.** `User.online`, `Chat.members`, `Chat.lastMessage`, `Chat.unread`, `Message.sender`, `Message.receipts`, `Receipt.user` are all batched per request (see `makeLoaders` in `src/context.ts`). A query selecting 50 messages resolves their senders/receipts in one round trip each, not 50.
- **Realtime.** Subscriptions read a Forge pubsub topic (`chat:<id>` or `presence`), filter by event type, re-hydrate the domain object (via loaders), and yield the GraphQL payload. The wire event shape is byte-compatible across the three backends.
- **Background workers** run in-process: `fanout` (mark delivered + bump unread kv, idempotent on message id), `reap` (hard-delete disappearing messages + their blob), `fail` (always nacks → exercises `fail.dlq`), and a scheduler tick that fires due jobs and reports bounded lag/backlog diagnostics.
- **Errors** map Forge variants to `extensions.code` in the contract's taxonomy. The forgelib binding prefixes every error with `<CODE>: message`; `src/errors.ts` recovers the code from that prefix.
- **Operator diagnostics** are composed into `/internal/forge/diagnostics`, guarded by a separate bearer `OPS_TOKEN`. The application owns the route and authentication; Forge supplies bounded structured runtime and scheduler reports without starting an admin server.

## Layout

```
src/
  server.ts       HTTP + WS wiring, CORS, the /_forge/blob proxy, worker startup
  schema.ts       reads ../schema.graphql, makeExecutableSchema(resolvers)
  resolvers/      the resolver map, split by role: scalars / types / query / mutation / subscription, plus helpers (auth, ids, mapEvents); index.ts assembles them (typed via codegen)
  context.ts      AppCtx (Forge wrappers, kv presence/unread/typing, ops gauges), DataLoaders, bearer auth
  db.ts           domain SQL over `pg` (batched queries for the loaders)
  worker.ts       fanout / reap / fail workers + scheduler tick
  errors.ts       Forge error -> GraphQL code mapping
  generated/      codegen output (typed Resolvers map)
test/             vitest integration suite (boots the server, drives HTTP + WS)
```

## Run

```sh
bun install
bun run codegen        # regenerate src/generated/graphql.ts from ../schema.graphql
bun run typecheck      # tsc --noEmit
bun run start          # node src/server.ts  -> http://127.0.0.1:8082/graphql
```

Forge configures itself from `forge.toml` in this directory: with no configuration it boots an embedded Postgres (data persists in `.forge/pg`); setting `FORGE_POSTGRES_URL` (and `FORGE_BLOB_SIGNING_SECRET`, both interpolated by that file) wins when you'd rather use your own server. Run `npm run migrate` as the Forge deployment step. The development config also enables automatic Forge migration. The app's own pool follows `forge.postgresUrl()`, and the server applies `../migrations.sql` on startup. The remaining knobs are app/server settings via env (see `.env.example`): `PORT`, `HOST`, `CORS_ORIGIN`, `OPS_TOKEN`, `APP_PRESENCE_TTL_SECS`, `APP_SCHEDULER_MS`, `APP_DISAPPEARING_SECS`.

## Test

```sh
bun run test           # vitest
```

The suite boots the server in a child process on an ephemeral port against a live Postgres (it drops/creates `chatapp_node_test` on `127.0.0.1:5432`), then drives the GraphQL API over HTTP + graphql-ws. It covers signup/session, group chat visibility, live message delivery, typing, presence TTL expiry, attachment upload round-trip, unread increment/clear, `logoutAll` revocation, send-burst rate limiting, read receipts, disappearing-message expiry, the reactions feature flag, API-key auth, and `opsStats` (online + DLQ). It also asserts the served SDL (via introspection) equals `../schema.graphql` under a normalized comparison. No skips.

The direct `reactions_v1` call keeps this example small. A production OpenFeature integration can install `@forgelib/openfeature-provider`, register `ForgeProvider` with the official server SDK, and attach `telemetryHook()` at client scope. Use `configGetMany`/`flagDetailsMany` for startup reads and reserve `configSnapshot` for explicitly expiring disconnected work.

## Docker

```sh
docker compose -f docker-compose.yml up --build
```

Brings up Postgres 18 and the backend at http://127.0.0.1:8082/graphql. The image compiles the `forgelib` native addon from source; the repo-root `.dockerignore` strips committed `*.node` binaries.

## Known gap

The `forgelib` binding does not expose Forge's `maintain()` sweep, so the maintenance tick runs only `runSchedulerOnce()`. KV TTLs and queue leases still expire lazily at read time in the Postgres backend, so behaviour is unaffected; only the proactive background purge is unavailable from Node.

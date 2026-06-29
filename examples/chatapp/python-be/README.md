# chatapp: Python backend

A pure GraphQL API for the chatapp example: FastAPI + Strawberry (code-first schema,
HTTP + `graphql-transport-ws`), asyncpg for the chat tables, and forgelib for every
infra primitive (auth, blob, pubsub, queue, kv, schedule, ratelimit, config).

It serves exactly the canonical `../schema.graphql`. Strawberry is code-first, so parity is
guaranteed by a test (`tests/test_sdl_match.py`) that compares the emitted SDL to the
canonical file under a normalized comparison (sorted types + fields, descriptions ignored,
an absent default treated as `= null`).

## Layout

```
app/
  main.py            FastAPI app: lifespan, migrations, CORS, GraphQL + blob routes, workers
  gql/               Strawberry schema split by role: types / query / mutation / subscription,
                     helpers (auth + error mapping), schema.py (assembly); exports `schema`
  context.py         Bearer-auth context for HTTP + WS, lazy/cached principal resolution
  loaders.py         per-request DataLoaders for every relational field
  db.py              asyncpg queries against users/chats/chat_members/messages/receipts
  workers.py         fanout / reap / fail queue workers + scheduler tick (asyncio tasks)
  blob_router.py     serves Forge presigned blob URLs (forgelib exposes no router)
  sdl.py             normalized SDL comparison used by the parity test
  schema.graphql     copy of the canonical SDL (parity target)
  migrations.sql     copy of the canonical domain tables (applied on startup)
tests/               integration suite over real HTTP + WS against a live Postgres
```

## Design notes

- **Bearer auth, no cookies.** HTTP sends `Authorization: Bearer <token>`; the
  graphql-transport-ws socket sends `{"authorization": "Bearer <token>"}` in the
  `connection_init` payload. A token authenticates as either a Forge session
  (`validate_session`, which slides the idle deadline) or a Forge API key
  (`verify_api_key`). `me` returns null when unauthenticated; other auth'd resolvers raise
  `UNAUTHENTICATED`.
- **DataLoader on every relational field** (`Chat.members/lastMessage/unread`,
  `Message.sender/receipts`, `Receipt.user`, `User.online`). Loaders are built fresh per
  request in the context, so a query selecting N messages issues one batched query per
  field, not N. The DB-backed loaders use `= ANY($1)`; the kv-backed ones (online, unread)
  fan out concurrently in a single dispatch.
- **Realtime** rides Forge pubsub via the binding's `Subscription` async iterator. Topics:
  `chat:<id>` carries `message`/`typing`/`receipt`; `presence` carries `presence`. Each
  subscription filters by event type, re-hydrates the domain object, and (for typing)
  suppresses the caller's own events.
- **Workers** run in-process as asyncio tasks: a fanout worker (marks receipts delivered +
  bumps unread kv, idempotent on message id), a reap worker (deletes a disappearing
  message's row + blob when its scheduled job fires), a fail worker (always nacks to drive
  the DLQ demo), and a scheduler loop (`run_scheduler_once` fires due `at` jobs).
- **Presigned blob URLs.** forgelib does not expose `blob_router()`, so `blob_router.py`
  mounts the equivalent route at the default presign prefix (`/_forge/blob`). It verifies
  the HMAC signature + expiry with `blob_verify_presign` (the exact check the Rust router
  performs), then does the get/put. Upload flow: `requestUpload(chatId)` → PUT to the
  signed URL → `sendMessage(mediaKey)` → `Message.media.downloadUrl`.

## Running

Requires a Postgres and a Rust toolchain (uv builds the editable `forgelib` wheel via
maturin on first `uv sync`).

Forge configures itself from `forge.toml` in this directory, which references
`FORGE_POSTGRES_URL` and `FORGE_BLOB_SIGNING_SECRET` from the environment (`${VAR:-default}`),
so the exports below override the toml defaults rather than passing values to Forge directly.

```sh
uv sync
# point at your database; it is created/migrated on startup
export FORGE_POSTGRES_URL=postgres://postgres:forge@127.0.0.1:5432/chatapp_python_be
export FORGE_BLOB_SIGNING_SECRET=dev-secret-change-me
uv run uvicorn app.main:app --host 0.0.0.0 --port 8083
```

GraphQL is at `/graphql` (POST for queries/mutations, WS upgrade for subscriptions).
`/healthz` returns `ok`.

### Environment

`FORGE_POSTGRES_URL` and `FORGE_BLOB_SIGNING_SECRET` are referenced by `forge.toml` (as
`${VAR:-default}`) rather than read by Forge directly; the rest configure the app loops and CORS.

| var | default | meaning |
| --- | --- | --- |
| `FORGE_POSTGRES_URL` | `postgres://postgres:forge@127.0.0.1:5432/chatapp_python_be` | shared by Forge + asyncpg |
| `FORGE_BLOB_SIGNING_SECRET` | `dev-secret-change-me` | enables presigned blob URLs |
| `APP_PRESENCE_TTL_SECS` | `30` | `online:` kv TTL refreshed by heartbeat |
| `APP_SCHEDULER_MS` | `30000` | scheduler tick cadence |
| `APP_DISAPPEARING_SECS` | `86400` | lifetime set when disappearing is enabled |
| `CORS_ORIGIN` | `*` | permissive in dev |

## Tests

The suite boots the real ASGI app under uvicorn on a free port and drives the GraphQL API
over real HTTP (httpx) and WS (websockets) against a live Postgres. It creates its own
database `chatapp_python_be_test` and shortens TTL/scheduler timers so TTL-driven scenarios
finish in seconds. No skips.

```sh
uv run pytest          # needs Postgres at postgres://postgres:forge@127.0.0.1:5432
uv run ruff check .
```

## Docker

```sh
docker compose -f docker-compose.yml up --build
```

Brings up Postgres, this backend (8083), and the shared React SPA (5173). The build context
is the repo root because of the `forgelib` path dependency.

# chatapp

A WhatsApp-style group chat built on [Forge](../../), implemented as three interchangeable
GraphQL backends (Rust, Node, Python) plus one React SPA that runs against any of them.
The point: dogfood every Forge primitive in a real app, and use GraphQL the way it's meant to be
used: one typed schema, client-driven field selection via fragments + codegen, DataLoader
batching, and live subscriptions.

If you are learning Forge from scratch, start with [`../todoapp`](../todoapp) first. This chatapp
is intentionally the full real-app version: it keeps shared shapes in `types` files and common
GraphQL/auth helpers in `helpers`/`context` files so the primitive-heavy flows stay readable
without hiding the production concerns.

```
chatapp/
  schema.graphql     canonical GraphQL contract (source of truth)
  migrations.sql     domain tables (users, chats, chat_members, messages, receipts)
  rust-be/           axum + async-graphql + forge crate              :8081
  node-be/           GraphQL Yoga + graphql-ws + forgelib          :8082
  python-be/         FastAPI + Strawberry + forgelib                 :8083
  react-fe/          Vite + React + urql (shared across backends)    :5173 dev
```

## Parity model

`schema.graphql` is the single source of truth. node-be consumes it directly (schema-first via
graphql-codegen `typescript-resolvers`); rust-be (async-graphql) and python-be (Strawberry) are
code-first and each ship a test asserting their emitted SDL matches it (normalized). react-fe runs
graphql-codegen against the same file for a fully typed urql client. Because all three serve the
identical schema, the one React app works against any backend by pointing two env vars at it.

## GraphQL, properly

- Every relational field resolves through a per-request **DataLoader** (`= ANY($1)` batch queries),
  so selecting 50 messages never fans out into per-row lookups.
- The frontend builds every operation from named fragments as generated typed documents: no
  hand-written query strings anywhere.
- Realtime (new messages, typing, presence, receipts) runs over `graphql-transport-ws`
  subscriptions backed by Forge pubsub.

## Forge primitives → features

auth → sessions, API keys, password hashing · blob → attachments (presigned direct upload) ·
pubsub → all realtime · queue → fan-out delivery + DLQ demo · kv → presence, unread, typing ·
schedule → disappearing messages · ratelimit → login/signup + send throttling · config →
`reactions_v2` feature flag + `max_upload_bytes`.

## Auth

Bearer tokens. `signup`/`login` return a Forge session token; the SPA sends it as
`Authorization: Bearer <token>` on HTTP and in the graphql-ws `connectionParams` on the socket.
A bearer validates as a session or an API key. Backends enable permissive CORS for the
cross-origin SPA.

## Run a backend (dev)

No database setup is required for local dev. Each backend reads its own
`forge.toml` at `<lang>-be/forge.toml`, which boots an embedded Postgres by
default (data persists in that backend's `.forge/pg`). Set
`FORGE_POSTGRES_URL` to use a dedicated server instead; the app's own domain
tables follow Forge's resolved database.

Start one backend:

```sh
# Rust
cd rust-be && cargo run

# Node
cd node-be && bun install && bun run start

# Python
cd python-be && uv sync && uv run uvicorn app.main:app --port 8083
```

Or point a backend at your own Postgres:

```sh
docker compose up -d db
createdb -h 127.0.0.1 -U postgres chatapp_node
cd node-be && FORGE_POSTGRES_URL=postgres://postgres:forge@127.0.0.1:5432/chatapp_node bun run start
```

Then run the frontend against it:

```sh
cd react-fe && bun install && bun run codegen
VITE_GRAPHQL_HTTP=http://localhost:8082/graphql VITE_GRAPHQL_WS=ws://localhost:8082/graphql bun run dev
```

## Run a full stack (docker)

`docker compose up` inside any backend folder brings up Postgres + that backend + the React SPA
pointed at it:

```sh
cd node-be && docker compose up --build   # SPA on :8092, backend on :8082
```

(rust-be → SPA :8091, python-be → SPA :8093.)

## Tests

Each backend owns an integration suite that drives its running GraphQL API (HTTP + WS) against
a real Postgres: `cd rust-be && cargo test` · `cd node-be && bun run test` · `cd python-be &&
uv run pytest`. Each covers signup/session, group chat, live delivery, typing, presence,
attachments, unread, receipts, disappearing messages, rate limiting, the feature flag, API-key
auth, and ops gauges.

The shared React app also has one Playwright suite in [`react-fe/e2e`](react-fe/e2e). Run
`docker compose up --build` from this directory, then `cd react-fe && bun run test:e2e` to exercise
the same browser flows against the Rust, Node, and Python stacks. On OrbStack, use
`docker compose --env-file .env.orb up --build` and
`cd react-fe && bun run test:e2e --config=playwright.orb.config.ts`.

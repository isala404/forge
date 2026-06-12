# chatapp

A WhatsApp-style group chat built on [Forge](../../), implemented as **three interchangeable
GraphQL backends** (Rust, Node, Python) plus **one** React SPA that runs against any of them.
The point: dogfood every Forge primitive in a real app, and use GraphQL the way it's meant to be
used — one typed schema, client-driven field selection via fragments + codegen, DataLoader
batching, and live subscriptions.

```
chatapp/
  schema.graphql     canonical GraphQL contract (source of truth)
  migrations.sql     domain tables (users, chats, chat_members, messages, receipts)
  SPEC.md            the build contract all four projects conform to
  rust-be/           axum + async-graphql + forge crate              :8081
  node-be/           GraphQL Yoga + graphql-ws + forge-node          :8082
  python-be/         FastAPI + Strawberry + forge-py                 :8083
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
- The frontend builds **every** operation from named fragments as generated typed documents — no
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
A bearer validates as a session **or** an API key. Backends enable permissive CORS for the
cross-origin SPA.

## Run a backend (dev)

Each backend needs Postgres. From the repo root: `docker compose up -d db` (postgres:18, user
`postgres`, password `forge`). Then create the per-app database and start one backend — for example:

```sh
# Rust
createdb -h 127.0.0.1 -U postgres chatapp_rust
cd rust-be && FORGE_POSTGRES_URL=postgres://postgres:forge@127.0.0.1:5432/chatapp_rust cargo run

# Node
cd node-be && bun install && FORGE_POSTGRES_URL=postgres://postgres:forge@127.0.0.1:5432/chatapp_node bun run start

# Python
cd python-be && uv sync && FORGE_POSTGRES_URL=postgres://postgres:forge@127.0.0.1:5432/chatapp_python \
  uv run uvicorn app.main:app --port 8083
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

Each backend owns an integration suite that drives its **running** GraphQL API (HTTP + WS) against
a real Postgres — no shared cross-stack suite. `cd rust-be && cargo test` · `cd node-be && bun run
test` · `cd python-be && uv run pytest`. Each covers signup/session, group chat, live delivery,
typing, presence, attachments, unread, receipts, disappearing messages, rate limiting, the feature
flag, API-key auth, and ops gauges.

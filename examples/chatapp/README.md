# chatapp

A group chat backend built on [Forge](../../). The release-gated Node service uses every Forge primitive with one typed GraphQL schema, DataLoader batching, and live subscriptions.

If you are learning Forge from scratch, start with [`../todoapp`](../todoapp) first. This example keeps shared shapes in `types` files and common GraphQL and auth helpers in `helpers` and `context` files so the primitive-heavy flows stay readable.

```
chatapp/
  schema.graphql     canonical GraphQL contract (source of truth)
  migrations.sql     domain tables (users, chats, chat_members, messages, receipts)
  node-be/           GraphQL Yoga + graphql-ws + forgelib          :8082
```

## Parity model

`schema.graphql` is the single source of truth. The Node backend consumes it through graphql-codegen.

## GraphQL, properly

- Every relational field resolves through a per-request **DataLoader** (`= ANY($1)` batch queries), so selecting 50 messages does not fan out into per-row lookups.
- Realtime (new messages, typing, presence, receipts) runs over `graphql-transport-ws` subscriptions backed by Forge pubsub.

## Forge primitives → features

auth → sessions, API keys, password hashing · blob → attachments (presigned direct upload) · pubsub → all realtime · queue → fan-out delivery + DLQ demo · kv → presence, unread, typing · schedule → disappearing messages · ratelimit → login/signup + send throttling · config → `reactions_v1` feature flag + `max_upload_bytes`.

## Auth

`signup` and `login` return a Forge session token. HTTP callers send it as `Authorization: Bearer <token>`; GraphQL WebSocket callers use `connectionParams.authorization`. A bearer validates as a session or an API key. The application owns CORS, origin checks, connection limits, and client token storage.

## Run a backend (dev)

No database setup is required for local dev. Each backend reads its own `forge.toml` at `<lang>-be/forge.toml`, which boots an embedded Postgres by default (data persists in that backend's `.forge/pg`). Set `FORGE_POSTGRES_URL` to use a dedicated server instead; the app's own domain tables follow Forge's resolved database.

Start the canonical backend:

```sh
cd node-be && bun install && bun run start
```

Or point a backend at your own Postgres:

```sh
docker compose up -d db
createdb -h 127.0.0.1 -U postgres chatapp_node
cd node-be && FORGE_POSTGRES_URL=postgres://postgres:forge@127.0.0.1:5432/chatapp_node bun run start
```

## Run with Docker

`docker compose up` brings up Postgres and the backend:

```sh
cd node-be && docker compose up --build   # backend on :8082
```

## Tests

The Node backend integration suite drives its running GraphQL API over HTTP and WebSocket against a real PostgreSQL database: `cd node-be && bun run test`. It covers signup/session, group chat, live delivery, typing, presence, attachments, unread, receipts, disappearing messages, rate limiting, the feature flag, API-key auth, and ops gauges.

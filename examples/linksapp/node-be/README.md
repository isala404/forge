# linksapp: Node/Hono backend

URL shortener backend for linksapp. Runs on port **9092** and stores all data in
Forge KV/blob/queue/pubsub, no separate app database.

## Start

```sh
bun install        # or npm install
node src/index.ts  # or bun run start
```

Forge configures itself from `forge.toml` in this directory: with no configuration it
boots an embedded Postgres (data persists in `.forge/pg`), and a set
`FORGE_POSTGRES_URL` (interpolated by that file) wins when you'd rather use your own
server, e.g. `postgres://postgres:forge@127.0.0.1:5432/linksapp_node`.

## Background workers

Three loops start with the server:

- **clicks worker** drains `clicks` queue; publishes updated totals to `clicks:{slug}` pubsub topic.
- **expire worker** drains `link-expire` queue; hard-deletes links whose scheduled TTL has fired.
- **scheduler loop** fires `scheduleAt` jobs every 30 s and runs Forge housekeeping.

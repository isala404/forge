# linksapp: Node/Hono backend

URL shortener backend implementing the [linksapp spec](../SPEC.md). Runs on port **9092**, uses `linksapp_node` as its Postgres database, and stores all data in Forge KV/blob/queue/pubsub, no separate database.

## Start

```sh
bun install        # or npm install
node src/index.ts  # or bun run start
```

Set `FORGE_POSTGRES_URL` to override the default (`postgres://postgres:forge@127.0.0.1:5432/linksapp_node`).

## Background workers

Three loops start with the server:

- **clicks worker** drains `clicks` queue; publishes updated totals to `clicks:{slug}` pubsub topic.
- **expire worker** drains `link-expire` queue; hard-deletes links whose scheduled TTL has fired.
- **scheduler loop** fires `scheduleAt` jobs every 30 s and runs Forge housekeeping.

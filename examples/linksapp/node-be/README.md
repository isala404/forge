# linksapp: Node/Hono backend

URL shortener backend for linksapp. Runs on port **9092**, uses `linksapp_node` as its Postgres database, and stores all data in Forge KV/blob/queue/pubsub, no separate database.

## Start

```sh
bun install        # or npm install
node src/index.ts  # or bun run start
```

Forge configures itself from `forge.toml` in this directory. That file references
`FORGE_POSTGRES_URL` (`${FORGE_POSTGRES_URL:-postgres://postgres:forge@127.0.0.1:5432/linksapp_node}`),
so setting the env var overrides the default rather than passing the URL to Forge directly.

## Background workers

Three loops start with the server:

- **clicks worker** drains `clicks` queue; publishes updated totals to `clicks:{slug}` pubsub topic.
- **expire worker** drains `link-expire` queue; hard-deletes links whose scheduled TTL has fired.
- **scheduler loop** fires `scheduleAt` jobs every 30 s and runs Forge housekeeping.

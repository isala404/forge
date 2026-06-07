# Linkly (SvelteKit)

The same URL shortener as [`apps/linkly`](../linkly), but the backend is reached
from **SvelteKit** through the **Node bindings** ([`bindings/forge-node`](../../bindings/forge-node))
instead of Rust+axum. It demonstrates Forge driving a JS app:

- `src/lib/server/forge.js` — a singleton `ForgeClient` plus a background worker
  that drains the `clicks` queue (JS `dequeue`/`ack` loop) and aggregates into `kv`.
- `src/routes/api/*` — JSON endpoints (`kv` storage + `queue` enqueue).
- `src/routes/r/[code]` — redirect that enqueues a click analytics event.
- `src/routes/+page.svelte` — the axios frontend.

## Run

```sh
# 1. start Postgres (from the repo root)
docker compose up -d db

# 2. build the native binding (once)
cd ../../bindings/forge-node && npm install && npm run build:debug && cd -

# 3. run the app
npm install
npm run build && FORGE_POSTGRES_URL=postgres://postgres:forge@localhost:5432/forge_dev PORT=5555 node build
#   ...or for dev: npm run dev
```

Then open http://127.0.0.1:5555.

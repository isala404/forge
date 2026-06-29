# linksapp

A small URL shortener built on Forge, implemented as three interchangeable REST backends
plus one shared React frontend. Where [`todoapp`](../todoapp) keeps the domain tiny to show
five primitives, `linksapp` exercises **all eight**. The architecture, not extra
features, is what pulls them in.

```
linksapp/
  SPEC.md            shared REST contract (normative)
  rust-be/           Rocket + forge crate                     :9091
  node-be/           Hono + forge-node                         :9092
  python-be/         FastAPI + forgelib                        :9093
  react-fe/          Vite + React shared across all backends   :5175
```

One entity (a link), one mental model: shorten a URL, redirect fast, count clicks. The
redirect is the lesson: do the minimum synchronously, push the rest to a worker.

## Every primitive earns its place

| Primitive    | Feature                                                                   |
| ------------ | ------------------------------------------------------------------------- |
| `auth`       | signup / login / bearer sessions                                          |
| `kv`         | the data store (users, links) **and** the atomic `clicks:<slug>` counter  |
| `ratelimit`  | throttle auth attempts; throttle redirects per slug                       |
| `queue`      | every redirect enqueues a click event, drained off the hot path           |
| `pubsub`     | the worker publishes the new count → the dashboard updates live over SSE  |
| `schedule`   | a link with a TTL schedules a one-shot that deletes it at expiry          |
| `blob`       | a QR-code SVG generated per link, stored in blob, served back             |
| `config`     | flag `custom_slugs` (pick your own slug); value `max_links_per_user`      |

### The redirect hot path

```
GET /:slug
  1. rec = kv.get(link:slug:{slug})          # kv is the store
  2. ratelimit.check("redirect", slug, …)    # fail-open
  3. kv.incr(clicks:{slug})                   # atomic counter
  4. queue.enqueue("clicks", {slug})          # hand off the rest
  5. 302 → rec.url
```

A background **clicks worker** drains that queue and `pubsub.publish`es `{slug, clicks}` to
`clicks:{slug}`; the `/api/links/:slug/live` SSE endpoint forwards it to the open dashboard.
A second **expire worker** drains the `link-expire` queue (fed by `schedule.at`) and deletes
expired links. A third loop runs `run_scheduler_once` + `maintain` every 30s. This three-loop
shape mirrors [`chatapp`](../chatapp)'s workers.

## How to read it

Start with a backend's route file, then its worker file:

- [`rust-be/src/routes.rs`](rust-be/src/routes.rs) · [`rust-be/src/worker.rs`](rust-be/src/worker.rs)
- [`node-be/src/routes.ts`](node-be/src/routes.ts) · [`node-be/src/worker.ts`](node-be/src/worker.ts)
- [`python-be/app/routes.py`](python-be/app/routes.py) · [`python-be/app/worker.py`](python-be/app/worker.py)

The route files call Forge directly inside the handlers; the neighboring `types` and
`util`/`utils` files hold only DTOs and pure helpers. The exact JSON API is in
[`SPEC.md`](SPEC.md).

## Run

Start Postgres from the repo root:

```sh
docker compose up -d db
```

Each backend reads its own `forge.toml` at `<app>/<lang>-be/forge.toml`, which is where
Forge gets its config now. The `FORGE_POSTGRES_URL=... <run cmd>` prefix below still works
because that toml interpolates the variable (`${FORGE_POSTGRES_URL:-...}`).

Then create each backend's database and run it:

```sh
# Rust
createdb -h 127.0.0.1 -U postgres linksapp_rust
cd examples/linksapp/rust-be
FORGE_POSTGRES_URL=postgres://postgres:forge@127.0.0.1:5432/linksapp_rust cargo run

# Node
createdb -h 127.0.0.1 -U postgres linksapp_node
cd examples/linksapp/node-be
bun install
FORGE_POSTGRES_URL=postgres://postgres:forge@127.0.0.1:5432/linksapp_node bun run start

# Python
createdb -h 127.0.0.1 -U postgres linksapp_python
cd examples/linksapp/python-be
uv sync
FORGE_POSTGRES_URL=postgres://postgres:forge@127.0.0.1:5432/linksapp_python uv run uvicorn app.main:app --port 9093
```

Run the frontend once and point it at any backend:

```sh
cd examples/linksapp/react-fe
bun install
bun run dev --host 127.0.0.1 --port 5175
```

Open `http://127.0.0.1:5175/?api=http://127.0.0.1:9091` for Rust, `:9092` for Node, or
`:9093` for Python. Create a link, open it (the short URL is `<backend>/<slug>`), and watch
the click count tick up live.

## Feature flag

`custom_slugs` defaults off, so new links get a random 7-char slug and the frontend hides
the custom-slug field. The `config` primitive is exercised on the read path: `/api/meta`
reports `features.customSlugs` and `POST /api/links` evaluates the flag per user. To turn it
on, set the flag in the config store (e.g. a one-off `set_flag_on("custom_slugs")`); `/api/meta`
picks it up within the config cache window and the UI reveals the field.

## Test

The Playwright suite drives the same browser flow against all three backends:

```sh
cd examples/linksapp/react-fe
bun run test:e2e
```

It expects the three backends and the Vite frontend to already be running on the ports above.

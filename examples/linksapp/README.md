# linksapp

A small URL shortener backend built on Forge. The release-gated Python service uses every Forge primitive.

```
linksapp/
  python-be/         FastAPI + forgelib                        :9093
```

One entity (a link), one mental model: shorten a URL, redirect fast, count clicks. The redirect is the lesson: do the minimum synchronously, push the rest to a worker.

## Every primitive earns its place

| Primitive    | Feature                                                                   |
| ------------ | ------------------------------------------------------------------------- |
| `auth`       | signup / login / bearer sessions                                          |
| `kv`         | the data store (users, links) **and** the atomic `clicks:<slug>` counter  |
| `ratelimit`  | throttle auth attempts; throttle redirects per slug                       |
| `queue`      | every redirect enqueues a click event, drained off the hot path           |
| `pubsub`     | the worker publishes a bounded hint for an application-owned SSE route    |
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

A background **clicks worker** drains that queue and publishes an invalidation hint to `clicks:{slug}`. The `/api/links/:slug/live` SSE endpoint forwards it, but the hint never carries the authoritative count. Consumers must fetch `/api/links/:slug/state` after connecting or reconnecting. A second **expire worker** drains the `link-expire` queue, which is fed by `schedule.at`, and deletes expired links. A third loop runs `run_scheduler_once` and `maintain` every 30 seconds.

## How to read it

Start with a backend's route file, then its worker file:

- [`python-be/app/routes.py`](python-be/app/routes.py) · [`python-be/app/worker.py`](python-be/app/worker.py)

The route files call Forge directly inside the handlers; the neighboring `types` and `util`/`utils` files hold only DTOs and pure helpers.

## Run

No database setup is required for local dev. Each backend reads its own `forge.toml` at `<app>/<lang>-be/forge.toml`, which boots an embedded Postgres by default (data persists in that backend's `.forge/pg`). Set `FORGE_POSTGRES_URL` to use a dedicated server instead.

Run the canonical backend:

```sh
cd examples/linksapp/python-be
uv sync
uv run uvicorn app.main:app --port 9093
```

The service owns its HTTP and SSE routes. A separate application can connect them to any frontend framework or client cache, but those pieces do not ship with Forge.

## Feature flag

`custom_slugs` defaults off, so new links get a random seven-character slug. The `config` primitive is exercised on the read path: `/api/meta` reports `features.customSlugs` and `POST /api/links` evaluates the flag per user. To turn it on, set the flag in the config store, for example with a one-off `set_flag_on("custom_slugs")`. `/api/meta` picks it up within the config cache window.

## Check

```sh
cd examples/linksapp/python-be
uv run ruff check
```

# linksapp: Rust/Rocket backend

URL shortener backend built on [Forge](../../../). Runs on port **9091**.

## Prerequisites

- Rust toolchain (edition 2024). No database needed: with no configuration the app
  boots an embedded Postgres (data persists in `.forge/pg`).

## Run

```sh
cargo run --release
# ...or against your own server (a set FORGE_POSTGRES_URL wins over embedded):
FORGE_POSTGRES_URL=postgres://postgres:forge@127.0.0.1:5432/linksapp_rust \
  cargo run --release
```

## Environment variables

Forge configures itself from `forge.toml` in this directory. The variables below are read
from the environment: `FORGE_POSTGRES_URL` is referenced by `forge.toml`
(`${FORGE_POSTGRES_URL:-}`) as an override rather than passed to Forge directly; the rest
configure the HTTP server.

| Variable              | Default                                                       |
| --------------------- | ------------------------------------------------------------- |
| `FORGE_POSTGRES_URL`  | unset (embedded Postgres)                                     |
| `PORT`                | `9091`                                                        |
| `BIND`                | `127.0.0.1`                                                   |
| `CORS_ORIGIN`         | `*`                                                           |

## Forge primitives used

| Primitive   | Purpose                                             |
| ----------- | --------------------------------------------------- |
| `auth`      | Signup / login / bearer sessions                    |
| `kv`        | Users, link records, owner lists, click counters    |
| `ratelimit` | Throttle auth (20/min) and redirects (600/min/slug) |
| `queue`     | Click events → worker; expire jobs → worker         |
| `pubsub`    | Worker publishes click counts for SSE live feed     |
| `schedule`  | One-shot deletion of links with a TTL               |
| `blob`      | Per-link QR code SVG                                |
| `config`    | `custom_slugs` flag; `max_links_per_user` value     |

## API summary

The same REST contract is implemented by all three backends.

# linksapp: Rust/Rocket backend

URL shortener backend built on [Forge](../../../). Runs on port **9091**, uses database `linksapp_rust`.

## Prerequisites

- Postgres running with a `linksapp_rust` database (or set `FORGE_POSTGRES_URL`)
- Rust toolchain (edition 2024)

## Run

```sh
FORGE_POSTGRES_URL=postgres://postgres:forge@127.0.0.1:5432/linksapp_rust \
  cargo run --release
```

## Environment variables

| Variable              | Default                                                       |
| --------------------- | ------------------------------------------------------------- |
| `FORGE_POSTGRES_URL`  | `postgres://postgres:forge@127.0.0.1:5432/linksapp_rust`     |
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

See [SPEC.md](../SPEC.md) for the normative contract shared across all three backends.

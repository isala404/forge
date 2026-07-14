# todoapp Node backend

A Hono REST API using `forgelib`.

```sh
bun install
bun run start      # no database needed: boots an embedded Postgres
```

Forge configures itself from `forge.toml` in this directory: with no configuration it boots an embedded Postgres (data persists in `.forge/pg`), and `FORGE_POSTGRES_URL` — interpolated by that file — wins when you'd rather use your own server:

```sh
FORGE_POSTGRES_URL=postgres://postgres:forge@127.0.0.1:5432/todoapp_node bun run start
```

Default port: `9082`.

Read `src/routes.ts` first for the Hono handlers, then `src/index.ts` for startup. Forge calls stay inline in the route handlers; `src/types.ts` is only data shapes, and `src/utils.ts` is only small validation/key helpers.

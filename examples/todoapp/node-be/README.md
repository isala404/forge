# todoapp Node backend

A Hono REST API using `forge-node`.

```sh
bun install
createdb -h 127.0.0.1 -U postgres todoapp_node
FORGE_POSTGRES_URL=postgres://postgres:forge@127.0.0.1:5432/todoapp_node bun run start
```

Forge configures itself from `forge.toml` in this directory. `FORGE_POSTGRES_URL` is read
from the environment by that file (`${FORGE_POSTGRES_URL:-...}`), so the command above still
works; it is no longer passed to Forge directly.

Default port: `9082`.

Read `src/routes.ts` first for the Hono handlers, then `src/index.ts` for startup. Forge
calls stay inline in the route handlers; `src/types.ts` is only data shapes, and
`src/utils.ts` is only small validation/key helpers.

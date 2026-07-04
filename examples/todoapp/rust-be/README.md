# todoapp Rust backend

A Rocket REST API using the `forge` crate directly.

```sh
cargo run          # no database needed: boots an embedded Postgres
```

Forge configures itself from `forge.toml` in this directory: with no configuration it
boots an embedded Postgres (data persists in `.forge/pg`), and `FORGE_POSTGRES_URL` —
interpolated by that file — wins when you'd rather use your own server:

```sh
FORGE_POSTGRES_URL=postgres://postgres:forge@127.0.0.1:5432/todoapp_rust cargo run
```

Default port: `9081`.

Read `src/routes.rs` first for the Rocket handlers, then `src/main.rs` for startup. Forge
calls stay inline in the route handlers; `src/types.rs` is only data shapes, and
`src/util.rs` is only small validation/key helpers.

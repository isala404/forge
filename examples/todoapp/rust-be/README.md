# todoapp Rust backend

A Rocket REST API using the `forge` crate directly.

```sh
createdb -h 127.0.0.1 -U postgres todoapp_rust
FORGE_POSTGRES_URL=postgres://postgres:forge@127.0.0.1:5432/todoapp_rust cargo run
```

Default port: `9081`.

Read `src/routes.rs` first for the Rocket handlers, then `src/main.rs` for startup. Forge
calls stay inline in the route handlers; `src/types.rs` is only data shapes, and
`src/util.rs` is only small validation/key helpers.

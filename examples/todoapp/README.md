# todoapp

A small REST todo backend built on Forge. The Rust service is release-gated.

```
todoapp/
  rust-be/           Rocket + forge crate                     :9081
```

The example keeps the domain deliberately compact so the Forge calls are easy to read:

- `auth` hashes passwords and owns bearer sessions.
- `kv` stores user profiles and todo lists.
- `queue` records an audit event for every todo mutation.
- `ratelimit` throttles signup/login attempts.
- `backend_capabilities` powers the deployment/meta panel; `probe` is the live readiness API.

## How to read it

Start with a backend's route file:

- [`rust-be/src/routes.rs`](rust-be/src/routes.rs)
Those files intentionally call Forge directly inside the route handlers. The neighboring `types` files hold request/response shapes, and the neighboring `util`/`utils` files hold only pure HTTP/key/validation helpers.

## Run

No database setup is needed. The backend reads `rust-be/forge.toml`, which boots an embedded Postgres by default and persists data in `.forge/pg`. Set `FORGE_POSTGRES_URL` when you want to use your own server.

Run the canonical backend:

```sh
cd examples/todoapp/rust-be
cargo run
```

## Test

```sh
cd examples/todoapp/rust-be
cargo test
```

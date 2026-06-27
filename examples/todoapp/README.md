# todoapp

A small REST todo app built on Forge, implemented as three interchangeable backends
plus one shared React frontend.

```
todoapp/
  SPEC.md            shared REST contract
  rust-be/           Rocket + forge crate                     :9081
  node-be/           Hono + forge-node                         :9082
  python-be/         FastAPI + forge-py                        :9083
  react-fe/          Vite + React shared across all backends   :5174
```

The example keeps the domain deliberately compact so the Forge calls are easy to read:

- `auth` hashes passwords and owns bearer sessions.
- `kv` stores user profiles and todo lists.
- `queue` records an audit event for every todo mutation.
- `ratelimit` throttles signup/login attempts.
- `backend_report` powers the health/meta panel in the UI.

## How to read it

Start with a backend's route file:

- [`rust-be/src/routes.rs`](rust-be/src/routes.rs)
- [`node-be/src/routes.ts`](node-be/src/routes.ts)
- [`python-be/app/routes.py`](python-be/app/routes.py)

Those files intentionally call Forge directly inside the route handlers. The neighboring
`types` files hold request/response shapes, and the neighboring `util`/`utils` files hold
only pure HTTP/key/validation helpers.

## REST contract

All three backends serve the same JSON API. See [`SPEC.md`](SPEC.md) for the exact
routes and payloads.

## Run

Start Postgres from the repo root:

```sh
docker compose up -d db
```

Then run any backend with its own database name:

```sh
# Rust
createdb -h 127.0.0.1 -U postgres todoapp_rust
cd examples/todoapp/rust-be
FORGE_POSTGRES_URL=postgres://postgres:forge@127.0.0.1:5432/todoapp_rust cargo run

# Node
createdb -h 127.0.0.1 -U postgres todoapp_node
cd examples/todoapp/node-be
bun install
FORGE_POSTGRES_URL=postgres://postgres:forge@127.0.0.1:5432/todoapp_node bun run start

# Python
createdb -h 127.0.0.1 -U postgres todoapp_python
cd examples/todoapp/python-be
uv sync
FORGE_POSTGRES_URL=postgres://postgres:forge@127.0.0.1:5432/todoapp_python uv run uvicorn app.main:app --port 9083
```

Run the frontend once and point it at any backend:

```sh
cd examples/todoapp/react-fe
bun install
bun run dev --host 127.0.0.1 --port 5174
```

Open `http://127.0.0.1:5174/?api=http://127.0.0.1:9081` for Rust, `:9082` for Node,
or `:9083` for Python.

## Test

The Playwright suite runs the same browser flow against all three backends:

```sh
cd examples/todoapp/react-fe
bun run test:e2e
```

The tests expect the Rust, Node, Python backends and the Vite frontend to already be
running on the ports above.

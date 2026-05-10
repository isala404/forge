# todo-dioxus

Built with [FORGE](https://tryforge.dev). One Rust binary, one PostgreSQL database, and a Dioxus frontend generated from the same backend schema/functions as the Svelte todo example.

## Development

```bash
docker compose up --build
```

Starts PostgreSQL, the Rust backend, and the Dioxus web frontend.

- Frontend: http://localhost:9080
- Backend: http://localhost:9081
- PostgreSQL: localhost:5432

### iOS Simulator

The same sample can run in the iOS simulator. The basic flow is:

```bash
rustup target add aarch64-apple-ios aarch64-apple-ios-sim
open /Applications/Xcode.app/Contents/Developer/Applications/Simulator.app
cd frontend
dx serve --ios
```

You may need to boot a specific simulator first:

```bash
xcrun simctl list
xcrun simctl boot "iPhone 15 Pro"
```

```bash
cd frontend
dx serve --ios
```

The Forge Dioxus runtime now uses native HTTP + SSE on non-wasm targets.
### Useful Commands

```bash
forge generate              # regenerate Dioxus bindings from Rust models/functions
forge check                 # validate config, migrations, and project health
forge migrate status        # check which migrations have run
forge migrate up            # apply pending migrations (forward-only)
```

### Running Tests

```bash
TEST_DATABASE_URL=postgres://postgres:forge@localhost:5432/todo-dioxus cargo test
```

The frontend uses generated Dioxus subscription hooks, so CRUD updates appear without manual refetching.

### Production Build

```bash
cd frontend && dx build --web --release && cd ..
cargo build --release
```

The release binary embeds the compiled Dioxus app from `frontend/dist`.

## Project Structure

```
todo-dioxus/
├── src/
│   ├── main.rs              # Entry point
│   ├── schema/              # Data models (Rust types that generate frontend types)
│   └── functions/           # Queries + mutations
├── migrations/              # SQL migrations
├── frontend/                # Dioxus web app + generated forge bindings
├── forge.toml               # Runtime configuration
├── docker-compose.yml       # Development environment
└── Dockerfile               # Production image
```

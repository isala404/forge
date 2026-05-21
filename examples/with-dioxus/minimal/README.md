# minimal

Built with [FORGE](https://tryforge.dev). Rust backend, Dioxus frontend, one PostgreSQL dependency.

## Development

Start the backend and database:

```bash
docker compose up --build
```

- Backend: http://localhost:9081
- PostgreSQL: localhost:5432

### Running the Frontend

The Dioxus frontend runs natively outside Docker. Install the [Dioxus CLI](https://dioxuslabs.com/learn/0.6/getting_started) first:

```bash
cargo install dioxus-cli --locked
```

Then from the `frontend/` directory:

```bash
# Web (default)
cd frontend && dx serve

# Desktop
cd frontend && dx serve --platform desktop

# iOS
cd frontend && dx serve --platform ios

# Android
cd frontend && dx serve --platform android
```

The frontend connects to the backend at `http://localhost:9081` by default.

### Useful Commands

```bash
forge generate              # regenerate Dioxus bindings from Rust models/functions
forge check                 # validate config, migrations, and project health
forge migrate status        # check which migrations have run
forge migrate up            # apply pending migrations (forward-only)
```

### Production Build

```bash
cd frontend && dx build --web --release && cd ..
cargo build --release
```

The release binary embeds the compiled Dioxus app from `frontend/dist`.

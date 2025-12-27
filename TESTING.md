# FORGE Local Testing Guide

This guide explains how to build the FORGE CLI from source, scaffold a test application, and link it to your local repository for end-to-end testing.

## 1. Prerequisites

- **Rust** (latest stable): `curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh`
- **Bun** (frontend runtime): `curl -fsSL https://bun.sh/install | bash`
- **Docker** (for PostgreSQL)
- **Git**
- **macOS only**: `brew install libiconv` (required for sqlx tests)

## 2. Build and Install the CLI

```bash
# From the root of the forge repo
cargo install --path crates/forge

# Verify installation
forge --version
```

## 3. Start PostgreSQL

Start a PostgreSQL container before creating the project:

```bash
docker run -d \
  --name forge-dev-db \
  -e POSTGRES_DB=forge_dev \
  -e POSTGRES_USER=forge \
  -e POSTGRES_PASSWORD=forge \
  -p 5432:5432 \
  postgres:16-alpine
```

## 4. Create a Test Project

Create a new directory for your test app. For this guide, we'll create it on the Desktop:

```bash
cd ~/Desktop
forge new demo-app
cd demo-app
```

This creates:
```
demo-app/
├── Cargo.toml
├── forge.toml
├── migrations/
│   └── 0001_create_users.sql
├── src/
│   ├── main.rs
│   ├── schema/
│   │   ├── mod.rs
│   │   └── user.rs
│   └── functions/
│       ├── mod.rs
│       └── users.rs
└── frontend/
    ├── package.json
    ├── svelte.config.js
    ├── vite.config.ts
    ├── tsconfig.json
    └── src/
        ├── app.html
        ├── routes/
        │   ├── +layout.svelte
        │   ├── +layout.ts
        │   └── +page.svelte
        └── lib/forge/
            ├── types.ts
            ├── api.ts
            └── index.ts
```

## 5. Link to Local FORGE Source

Since FORGE isn't published yet, you must patch dependencies to use your local source.

### Backend (`Cargo.toml`)

Open `demo-app/Cargo.toml` and replace the forge dependency:

```toml
[dependencies]
# Replace this line:
# forge = "0.1"
# With path to your local forge:
forge = { path = "/Users/YOUR_USERNAME/Projects/forge/crates/forge" }

# Keep everything else as-is
tokio = { version = "1", features = ["full"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
uuid = { version = "1", features = ["v4", "serde"] }
chrono = { version = "0.4", features = ["serde"] }
sqlx = { version = "0.8", features = ["runtime-tokio", "postgres", "chrono", "uuid"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
```

### Frontend (`frontend/package.json`)

Open `demo-app/frontend/package.json` and add the @forge/svelte dependency:

```json
{
  "dependencies": {
    "@forge/svelte": "file:/Users/YOUR_USERNAME/Projects/forge/frontend"
  }
}
```

### Install Frontend Dependencies

```bash
cd frontend
bun install
cd ..
```

## 6. Configure Environment

Create a `.env` file in `demo-app/`:

```bash
echo 'DATABASE_URL=postgres://forge:forge@localhost:5432/forge_dev' > .env
```

Update `forge.toml` to use the environment variable:

```toml
[database]
url = "${DATABASE_URL}"
```

## 7. Run the Application

You need **two terminal windows**.

### Terminal 1: Backend

```bash
# In demo-app/
cargo run
```

Expected output:
```
  ⚒️  FORGE v0.1.0

  🌐 Listening on http://127.0.0.1:8080
  📊 Dashboard at http://127.0.0.1:8080/_dashboard
```

The backend:
- Applies migrations automatically from `migrations/` directory
- Starts the HTTP gateway on port 8080
- Enables WebSocket for real-time subscriptions
- Serves the dashboard at `/_dashboard`

### Terminal 2: Frontend

```bash
# In demo-app/frontend/
bun run dev
```

Expected output:
```
  VITE v7.x.x  ready in 200 ms

  ➜  Local:   http://localhost:5173/
```

## 8. Verify Everything Works

### Check the Dashboard
Open **http://localhost:8080/_dashboard**
- Overview shows system stats
- Schema tab shows the `users` table
- Migrations tab shows applied migrations

### Check the Frontend
Open **http://localhost:5173**
- You should see the "Welcome to FORGE" page
- Create a user using the form
- The user list updates in real-time (WebSocket subscription)

### Test the API Directly
```bash
# Health check
curl http://localhost:8080/health

# Call a query
curl -X POST http://localhost:8080/rpc/get_users \
  -H "Content-Type: application/json"

# Create a user
curl -X POST http://localhost:8080/rpc/create_user \
  -H "Content-Type: application/json" \
  -d '{"email": "test@example.com", "name": "Test User"}'
```

## 9. Development Workflow

### Adding Components

Use the CLI to scaffold new components:

```bash
# Add a new model
forge add model Product

# Add a query function
forge add query get_products

# Add a mutation function
forge add mutation create_product

# Add an action (external API calls)
forge add action send_notification

# Add a background job
forge add job process_order

# Add a scheduled task
forge add cron daily_cleanup

# Add a workflow (multi-step process)
forge add workflow order_fulfillment
```

### Regenerate TypeScript Types

If schema changes and types get out of sync:

```bash
forge generate
```

This regenerates:
- `frontend/src/lib/forge/types.ts` - TypeScript interfaces from Rust models
- `frontend/src/lib/forge/api.ts` - Type-safe API bindings

### Adding a Migration

Create a new SQL file in `migrations/`:

```bash
cat > migrations/0002_create_products.sql << 'EOF'
CREATE TABLE IF NOT EXISTS products (
    id UUID PRIMARY KEY,
    name VARCHAR(255) NOT NULL,
    price DECIMAL(10, 2) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

SELECT forge_enable_reactivity('products');
EOF
```

Restart the backend to apply the new migration.

## CLI Reference

| Command | Description |
|---------|-------------|
| `forge new <name>` | Create a new project |
| `forge new <name> --minimal` | Create without frontend |
| `forge init` | Initialize in current directory |
| `forge add model <name>` | Add a new model |
| `forge add query <name>` | Add a query function |
| `forge add mutation <name>` | Add a mutation function |
| `forge add action <name>` | Add an action |
| `forge add job <name>` | Add a background job |
| `forge add cron <name>` | Add a cron task |
| `forge add workflow <name>` | Add a workflow |
| `forge generate` | Generate TypeScript client |
| `forge run` | Run the server |
| `forge run --port 3000` | Run on custom port |

## Troubleshooting

### "Dependency not found" errors
Check the paths in `Cargo.toml` and `package.json`. They must point to your local forge source:
- Backend: `forge = { path = "/absolute/path/to/forge/crates/forge" }`
- Frontend: `"@forge/svelte": "file:/absolute/path/to/forge/frontend"`

### Database connection refused
```bash
# Check if container is running
docker ps

# Start if not running
docker start forge-dev-db

# Or recreate
docker rm -f forge-dev-db
docker run -d --name forge-dev-db \
  -e POSTGRES_DB=forge_dev \
  -e POSTGRES_USER=forge \
  -e POSTGRES_PASSWORD=forge \
  -p 5432:5432 postgres:16-alpine
```

### Frontend WebSocket errors
The frontend connects to `http://localhost:8080` by default. Make sure:
1. Backend is running on port 8080
2. No firewall blocking the connection
3. Check `+layout.svelte` has correct URL in `ForgeProvider`

### TypeScript errors about @forge/svelte
Make sure you:
1. Added the dependency to `package.json`
2. Ran `bun install` in the frontend directory
3. Path points to the `frontend` directory of your forge repo (not `frontend/src`)

### Migrations not applying
- Check `migrations/` directory exists
- SQL files must be numbered: `0001_xxx.sql`, `0002_xxx.sql`
- Check backend logs for migration errors

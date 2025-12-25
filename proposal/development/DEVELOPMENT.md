# Local Development

> *Simple dev experience using cargo and bun*

---

## Philosophy

FORGE's local development is deliberately simple:

- **`forge` CLI** — Scaffolding only (create project, add models, generate code)
- **`cargo`** — Build and run the backend
- **`bun`** — Build and run the frontend
- **Dashboard** — Everything else (migrations, jobs, logs, debugging)

No complex dev server. No magic. Just standard Rust and JavaScript tooling.

---

## Quick Start

### 1. Create Project

```bash
# Scaffold a new project
forge new my-app
cd my-app
```

This creates:

```
my-app/
├── Cargo.toml
├── forge.toml
├── src/
│   ├── main.rs
│   ├── schema/
│   │   └── mod.rs
│   └── functions/
│       └── mod.rs
├── frontend/
│   ├── package.json
│   ├── src/
│   │   ├── lib/
│   │   │   └── forge/        # Generated client
│   │   └── routes/
│   └── vite.config.ts
└── migrations/
```

### 2. Start Development

**Terminal 1 — Backend:**

```bash
# Watch mode with cargo-watch
cargo watch -x run

# Or just run once
cargo run
```

**Terminal 2 — Frontend:**

```bash
cd frontend
bun install
bun run dev
```

That's it. Backend on `localhost:8080`, frontend on `localhost:5173`.

---

## Development Database

### Option 1: Local PostgreSQL (Recommended)

```bash
# Using Docker
docker run -d \
  --name forge-dev-db \
  -e POSTGRES_DB=forge_dev \
  -e POSTGRES_USER=forge \
  -e POSTGRES_PASSWORD=forge \
  -p 5432:5432 \
  postgres:16

# Or install natively
# macOS: brew install postgresql@16
# Ubuntu: sudo apt install postgresql-16
```

```toml
# forge.toml
[database]
url = "postgres://forge:forge@localhost:5432/forge_dev"
```

### Option 2: SQLite for Zero-Dependency Dev

For quick prototyping without PostgreSQL:

```toml
# forge.toml
[database]
url = "sqlite://./dev.db"
```

**Caveats with SQLite:**
- No `LISTEN/NOTIFY` — subscriptions poll instead of push (slower)
- No advisory locks — leader election uses file locks
- Some PostgreSQL-specific features disabled

SQLite is for local dev only. Always test with PostgreSQL before deploying.

---

## Hot Reload

### Backend (Rust)

Use `cargo-watch` for automatic recompilation:

```bash
# Install once
cargo install cargo-watch

# Run with watch
cargo watch -x run

# Ignore test files, faster rebuilds
cargo watch -x run --ignore tests/
```

Typical rebuild time: 2-5 seconds for incremental changes.

### Frontend (Svelte)

Vite provides instant HMR:

```bash
cd frontend
bun run dev
```

Changes reflect in <100ms.

### Code Generation

When you change schema or functions, regenerate the TypeScript client:

```bash
# Regenerate after schema changes
cargo run -- generate

# Or use watch mode
cargo watch -s "cargo run -- generate"
```

---

## Project Structure

```
my-app/
├── Cargo.toml              # Rust dependencies
├── forge.toml              # FORGE configuration
├── src/
│   ├── main.rs             # Entry point
│   ├── schema/
│   │   ├── mod.rs          # Schema exports
│   │   ├── user.rs         # User model
│   │   └── project.rs      # Project model
│   └── functions/
│       ├── mod.rs          # Function exports
│       ├── queries/
│       │   └── projects.rs # Query functions
│       ├── mutations/
│       │   └── projects.rs # Mutation functions
│       └── jobs/
│           └── email.rs    # Background jobs
├── frontend/
│   ├── package.json
│   ├── src/
│   │   ├── lib/
│   │   │   └── forge/      # Auto-generated client
│   │   ├── routes/
│   │   └── app.html
│   └── vite.config.ts
└── migrations/
    ├── 0001_initial.sql
    └── 0002_add_projects.sql
```

---

## Adding Features

### Add a Model

```bash
# Scaffold a new model
forge add model Task
```

Creates `src/schema/task.rs`:

```rust
use forge::prelude::*;

#[forge::model]
pub struct Task {
    pub id: Uuid,
    pub title: String,
    pub completed: bool,
    pub created_at: Timestamp,
}
```

### Add a Function

```bash
# Scaffold a query
forge add query get_tasks

# Scaffold a mutation
forge add mutation create_task

# Scaffold a job
forge add job send_reminder
```

### Generate Migration

After changing models, generate a migration via the dashboard:

1. Open `http://localhost:8080/_forge/` (dashboard)
2. Go to **Migrations** tab
3. Click **Generate Migration**
4. Review the SQL diff
5. Click **Apply**

Or via cargo:

```bash
cargo run -- migrate generate
cargo run -- migrate apply
```

---

## The Dashboard

Access at `http://localhost:8080/_forge/` when running locally.

### Features

| Tab | What it does |
|-----|--------------|
| **Schema** | View models, fields, relationships |
| **Functions** | List queries, mutations, actions |
| **Migrations** | Generate, review, apply migrations |
| **Jobs** | View queued/running/failed jobs, retry failed |
| **Crons** | See scheduled tasks, trigger manually |
| **Workflows** | Inspect workflow state, retry steps |
| **Logs** | Real-time logs with filtering |
| **Metrics** | Request latency, DB queries, job throughput |
| **Traces** | Distributed traces (when enabled) |
| **SQL** | Run ad-hoc queries (dev mode only) |

### Why Dashboard Over CLI?

1. **No auth complexity** — Dashboard uses the same session as your app
2. **Visual diffs** — See migration changes side-by-side
3. **Real-time** — Logs and metrics update live
4. **Context** — See related info (job failed? click to see logs)

The CLI remains for:
- `forge new` — Create projects
- `forge add` — Scaffold models/functions
- `forge generate` — Regenerate TypeScript client

Everything else: use the dashboard.

---

## Testing

### Unit Tests

```bash
# Run all tests
cargo test

# Run specific test
cargo test test_create_project

# Watch mode
cargo watch -x test
```

### Integration Tests

FORGE provides test utilities:

```rust
#[cfg(test)]
mod tests {
    use forge::testing::*;

    #[tokio::test]
    async fn test_create_project() {
        // Spin up test database (uses transactions, auto-rollback)
        let ctx = TestContext::new().await;

        // Call your mutation
        let project = ctx.mutate(create_project, CreateProjectInput {
            name: "Test".into(),
        }).await.unwrap();

        assert_eq!(project.name, "Test");

        // Query it back
        let found = ctx.query(get_project, project.id).await.unwrap();
        assert_eq!(found.id, project.id);
    }

    #[tokio::test]
    async fn test_job_execution() {
        let ctx = TestContext::new().await;

        // Dispatch job
        let job_id = ctx.dispatch(send_email, SendEmailInput {
            to: "test@example.com".into(),
            subject: "Test".into(),
        }).await.unwrap();

        // Run jobs synchronously in tests
        ctx.run_jobs().await;

        // Check job completed
        assert!(ctx.job_completed(job_id).await);
    }
}
```

### Frontend Tests

```bash
cd frontend

# Unit tests with vitest
bun run test

# E2E with Playwright
bun run test:e2e
```

---

## Environment Variables

```bash
# .env (local development)
DATABASE_URL=postgres://forge:forge@localhost:5432/forge_dev
FORGE_SECRET=dev-secret-change-in-prod

# Optional
LOG_LEVEL=debug
RUST_BACKTRACE=1
```

Load in development:

```bash
# Using dotenv
source .env && cargo run

# Or use cargo-dotenv
cargo install cargo-dotenv
cargo dotenv run
```

---

## Common Tasks

### Reset Database

```bash
# Drop and recreate
dropdb forge_dev && createdb forge_dev

# Or via dashboard: Migrations → Reset Database
```

### Seed Data

Create `src/seed.rs`:

```rust
pub async fn seed(ctx: &Context) -> Result<()> {
    // Create test user
    ctx.db.insert(User {
        id: Uuid::new_v4(),
        email: "test@example.com".into(),
        name: "Test User".into(),
    }).await?;

    // Create sample projects
    for i in 1..=5 {
        ctx.db.insert(Project {
            id: Uuid::new_v4(),
            name: format!("Project {}", i),
            owner_id: user.id,
        }).await?;
    }

    Ok(())
}
```

Run via dashboard: **SQL** → **Run Seed**.

### Debug a Request

1. Open dashboard **Traces** tab
2. Find the request by timestamp or path
3. Click to see full trace with timing breakdown

### Profile Performance

```toml
# forge.toml (dev)
[observability.logging]
slow_query_threshold = "10ms"  # Log queries slower than this
```

Check dashboard **Metrics** → **Slow Queries**.

---

## IDE Setup

### VS Code

Recommended extensions:
- `rust-analyzer` — Rust language support
- `svelte.svelte-vscode` — Svelte support
- `bradlc.vscode-tailwindcss` — Tailwind (if using)

```json
// .vscode/settings.json
{
  "rust-analyzer.cargo.features": ["dev"],
  "editor.formatOnSave": true,
  "[rust]": {
    "editor.defaultFormatter": "rust-lang.rust-analyzer"
  }
}
```

### JetBrains (RustRover/WebStorm)

- Install Rust plugin
- Enable "Run on save" for `cargo fmt`

---

## Troubleshooting

### "Connection refused" to database

```bash
# Check PostgreSQL is running
pg_isready -h localhost -p 5432

# Check connection string
psql $DATABASE_URL -c "SELECT 1"
```

### "Port already in use"

```bash
# Find what's using port 8080
lsof -i :8080

# Kill it or change port in forge.toml
[gateway]
port = 8081
```

### Slow rebuilds

```bash
# Use mold linker (Linux)
sudo apt install mold
RUSTFLAGS="-C link-arg=-fuse-ld=mold" cargo build

# Use lld (macOS/Windows)
# Add to .cargo/config.toml:
[target.x86_64-apple-darwin]
linker = "clang"
rustflags = ["-C", "link-arg=-fuse-ld=lld"]
```

### TypeScript types out of sync

```bash
# Regenerate client
cargo run -- generate

# If still wrong, clean and regenerate
rm -rf frontend/src/lib/forge
cargo run -- generate
```

---

## Related Documentation

- [Configuration](../reference/CONFIGURATION.md) — forge.toml reference
- [Schema](../core/SCHEMA.md) — Model definitions
- [Functions](../core/FUNCTIONS.md) — Query/mutation patterns
- [Migrations](MIGRATIONS.md) — Schema evolution

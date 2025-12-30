# FORGE Documentation

> *From Schema to Ship in a Single Day*

---

## What is FORGE?

FORGE is a full-stack application framework that combines developer experience with production reliability. It provides a single Rust binary containing everything you need to build and run modern web applications, backed by PostgreSQL as the only external dependency.

```
┌─────────────────────────────────────────────────────────────┐
│                      FORGE STACK                             │
│                                                              │
│   Frontend: Svelte 5 + Auto-generated TypeScript Client      │
│   Backend:  Rust (compiled to single binary)                 │
│   Database: PostgreSQL (the only dependency)                 │
│                                                              │
│   Jobs, Crons, Workflows, Dashboard: Built-in                │
└─────────────────────────────────────────────────────────────┘
```

---

## Core Concepts

| Concept | Description | Documentation |
|---------|-------------|---------------|
| **Schema** | Define Rust models, generate SQL and TypeScript | [Schema Guide](core/SCHEMA.md) |
| **Functions** | Queries, Mutations, Actions for your API | [Functions Guide](core/FUNCTIONS.md) |
| **Jobs** | Background tasks with retry and progress tracking | [Jobs Guide](core/JOBS.md) |
| **Crons** | Scheduled tasks with timezone support | [Crons Guide](core/CRONS.md) |
| **Workflows** | Multi-step durable processes with compensation | [Workflows Guide](core/WORKFLOWS.md) |
| **Reactivity** | Real-time subscriptions via WebSocket | [Reactivity Guide](core/REACTIVITY.md) |

---

## Quick Start

```bash
# Install FORGE CLI
cargo install --path crates/forge

# Create new project
forge new my-app
cd my-app

# Start PostgreSQL (Docker)
docker run -d --name forge-db -p 5432:5432 \
  -e POSTGRES_PASSWORD=postgres postgres:alpine

# Set database URL
echo 'DATABASE_URL=postgres://postgres:postgres@localhost/my_app' > .env

# Run backend
cargo run

# In another terminal: Run frontend
cd frontend && bun install && bun run dev
```

Open http://localhost:5173 for the app, http://localhost:8080/_dashboard/ for the dashboard.

---

## Project Structure

```
my-app/
├── Cargo.toml                    # Rust dependencies
├── forge.toml                    # FORGE configuration
├── .env                          # Environment variables (DATABASE_URL)
├── migrations/
│   └── 0001_initial.sql          # Database migrations (-- @up / -- @down)
├── src/
│   ├── main.rs                   # Entry point, register functions
│   ├── schema/
│   │   └── mod.rs                # Data models
│   └── functions/
│       ├── mod.rs
│       ├── queries/              # Read operations (#[forge::query])
│       ├── mutations/            # Write operations (#[forge::mutation])
│       ├── actions/              # External API calls (#[forge::action])
│       ├── jobs/                 # Background tasks (#[forge::job])
│       ├── crons/                # Scheduled tasks (#[forge::cron])
│       └── workflows/            # Multi-step processes (#[forge::workflow])
└── frontend/
    ├── package.json
    ├── src/
    │   ├── lib/
    │   │   └── forge/            # Generated TypeScript client
    │   └── routes/
    │       └── +page.svelte
    └── svelte.config.js
```

---

## Architecture

FORGE runs as a single binary containing all components:

```
┌─────────────────────────────────────────────────────────────────┐
│                        FORGE BINARY                              │
│                                                                  │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐              │
│  │   Gateway   │  │  Function   │  │   Worker    │              │
│  │  (HTTP/WS)  │  │  Executor   │  │   (Jobs)    │              │
│  └─────────────┘  └─────────────┘  └─────────────┘              │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐              │
│  │  Scheduler  │  │   Reactor   │  │ Observability│             │
│  │  (Crons)    │  │ (Real-time) │  │ (Metrics)   │              │
│  └─────────────┘  └─────────────┘  └─────────────┘              │
│                                                                  │
└──────────────────────────┬──────────────────────────────────────┘
                           │
                           ▼
┌─────────────────────────────────────────────────────────────────┐
│                       POSTGRESQL                                 │
│   App Tables │ Jobs │ Workflows │ Sessions │ Metrics │ Logs     │
└─────────────────────────────────────────────────────────────────┘
```

See [Architecture Overview](architecture/OVERVIEW.md) for details.

---

## Documentation Index

### Core

- [Schema](core/SCHEMA.md) - Define models with `#[forge::model]` and `#[forge::forge_enum]`
- [Functions](core/FUNCTIONS.md) - Queries, Mutations, Actions with context objects
- [Jobs](core/JOBS.md) - Background tasks with SKIP LOCKED pattern
- [Crons](core/CRONS.md) - Scheduled tasks with leader-only execution
- [Workflows](core/WORKFLOWS.md) - Durable multi-step processes with compensation
- [Reactivity](core/REACTIVITY.md) - Real-time subscriptions via PostgreSQL NOTIFY

### Architecture

- [Overview](architecture/OVERVIEW.md) - Single binary design and component wiring

### Database

- [PostgreSQL Schema](database/POSTGRES_SCHEMA.md) - All `forge_*` system tables
- [Migrations](database/MIGRATIONS.md) - Up/down migrations with CLI commands

### Cluster

- [Clustering](cluster/CLUSTERING.md) - Node registry, leader election, health checks

### Frontend

- [Frontend](frontend/FRONTEND.md) - Svelte 5 client, stores, job/workflow trackers

### Observability

- [Observability](observability/OBSERVABILITY.md) - Metrics, logs, traces
- [Dashboard](observability/DASHBOARD.md) - Built-in web dashboard

### Reference

- [CLI](reference/CLI.md) - Command-line interface reference

---

## Key Patterns

### Function Registration

Functions must be registered with the ForgeBuilder before running:

```rust
use forge::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    let config = ForgeConfig::from_env()?;

    Forge::builder()
        .function_registry_mut()
        .register_query::<ListUsersQuery>()
        .register_mutation::<CreateUserMutation>()
        .register_job::<ExportUsersJob>()
        .register_cron::<CleanupCron>()
        .register_workflow::<OnboardingWorkflow>()
        .config(config)
        .build()?
        .run()
        .await
}
```

### Database Access

All contexts provide database access via `ctx.db()`:

```rust
#[forge::query]
pub async fn list_users(ctx: &QueryContext) -> Result<Vec<User>> {
    sqlx::query_as::<_, User>("SELECT * FROM users")
        .fetch_all(ctx.db())
        .await
        .map_err(Into::into)
}
```

### Job Dispatch

Dispatch jobs from mutations or actions:

```rust
#[forge::mutation]
pub async fn create_user(ctx: &MutationContext, input: CreateUserInput) -> Result<User> {
    let user = /* create user */;

    ctx.dispatch_job::<WelcomeEmailJob>(WelcomeEmailArgs {
        user_id: user.id,
    }).await?;

    Ok(user)
}
```

### Real-time Subscriptions

Frontend subscribes to queries for live updates:

```typescript
// One-time fetch
const users = await query('list_users', {});

// Real-time subscription (auto-updates when data changes)
const usersStore = subscribe('list_users', {});
```

---

## Built-in Dashboard

Access the dashboard at `/_dashboard/` to:

- View cluster health and node status
- Monitor metrics, logs, and traces
- Manage jobs and workflows with progress tracking
- Browse cron execution history
- Set up alerts

---

## Migration System

Migrations use `-- @up` and `-- @down` markers:

```sql
-- @up
CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email VARCHAR(255) UNIQUE NOT NULL,
    name VARCHAR(255) NOT NULL
);
SELECT forge_enable_reactivity('users');

-- @down
DROP TABLE users;
```

CLI commands:
```bash
forge migrate up      # Apply pending migrations
forge migrate down 1  # Rollback last migration
forge migrate status  # Show migration status
```

---

## Technology Stack

| Layer | Technology | Purpose |
|-------|------------|---------|
| Backend | Rust | Performance, safety, single binary |
| Database | PostgreSQL | Data, jobs, events, metrics |
| Frontend | Svelte 5 | Reactive UI with runes |
| Protocol | HTTP + WebSocket | RPC + real-time subscriptions |

---

## License

FORGE is open source under the MIT License.

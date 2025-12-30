# CLI Reference

The `forge` CLI provides scaffolding and code generation for FORGE projects. It handles project creation, component generation, TypeScript codegen, server execution, and database migrations.

---

## Installation

```bash
# Install from source
cargo install --path crates/forge

# Or install from crates.io (when published)
cargo install forge
```

---

## Commands

| Command | Description |
|---------|-------------|
| `forge new <name>` | Create a new project |
| `forge init` | Initialize in existing directory |
| `forge add <type> <name>` | Add a component |
| `forge generate` | Generate TypeScript client code |
| `forge run` | Start the FORGE server |
| `forge migrate <action>` | Manage database migrations |

---

## `forge new`

Creates a new FORGE project with full scaffolding.

```bash
forge new <name> [OPTIONS]
```

### Arguments

| Argument | Description |
|----------|-------------|
| `<name>` | Project name (used for directory and package names) |

### Options

| Option | Description |
|--------|-------------|
| `--minimal` | Create without frontend (backend only) |
| `-o, --output <dir>` | Output directory (defaults to project name) |

### Generated Structure

```
<name>/
├── Cargo.toml              # Rust project manifest
├── forge.toml              # FORGE configuration
├── .env                    # Environment variables (DATABASE_URL)
├── .gitignore              # Git ignore rules
├── migrations/
│   └── 0001_initial.sql    # Initial migration (users, app_stats tables)
├── src/
│   ├── main.rs             # Application entry point
│   ├── schema/
│   │   ├── mod.rs
│   │   └── user.rs         # User model
│   └── functions/
│       ├── mod.rs
│       ├── users.rs        # get_users, get_user, create_user, update_user, delete_user
│       ├── app_stats.rs    # get_app_stats query
│       ├── export_users_job.rs         # Background job example
│       ├── heartbeat_stats_cron.rs     # Cron task example
│       └── account_verification_workflow.rs  # Workflow example
└── frontend/               # (unless --minimal)
    ├── package.json
    ├── svelte.config.js
    ├── vite.config.ts
    ├── tsconfig.json
    ├── .env.example
    ├── .forge/             # Generated @forge/svelte runtime
    │   ├── version
    │   └── svelte/
    │       ├── package.json
    │       ├── index.ts
    │       ├── types.ts
    │       ├── client.ts
    │       ├── context.ts
    │       ├── stores.ts
    │       ├── api.ts
    │       └── ForgeProvider.svelte
    └── src/
        ├── app.html
        ├── lib/
        │   └── forge/
        │       ├── types.ts   # TypeScript model types
        │       ├── api.ts     # Function bindings
        │       └── index.ts   # Re-exports
        └── routes/
            ├── +layout.ts
            ├── +layout.svelte
            └── +page.svelte
```

### Example

```bash
# Create a new project
forge new my-app

# Create backend-only project
forge new my-api --minimal

# Create project in specific directory
forge new my-app --output ~/projects/my-app
```

---

## `forge init`

Initializes FORGE in an existing directory. Creates the same structure as `forge new` but in the current directory.

```bash
forge init [OPTIONS]
```

### Options

| Option | Description |
|--------|-------------|
| `-n, --name <name>` | Project name (defaults to directory name) |
| `--minimal` | Create without frontend (backend only) |

### Example

```bash
mkdir my-project && cd my-project
forge init

# With custom name
forge init --name my-custom-name
```

---

## `forge add`

Adds a new component to an existing project. Must be run from a FORGE project directory (where `src/schema` or `src/functions` exists).

```bash
forge add <type> <name>
```

### Component Types

| Type | File Created | Description |
|------|--------------|-------------|
| `model` | `src/schema/<name>.rs` | Database model with `#[forge::model]` |
| `query` | `src/functions/<name>.rs` | Read-only function with `#[forge::query]` |
| `mutation` | `src/functions/<name>.rs` | Write function with `#[forge::mutation]` |
| `action` | `src/functions/<name>.rs` | External API function with `#[forge::action]` |
| `job` | `src/functions/<name>_job.rs` | Background job with `#[forge::job]` |
| `cron` | `src/functions/<name>_cron.rs` | Scheduled task with `#[forge::cron]` |
| `workflow` | `src/functions/<name>_workflow.rs` | Multi-step workflow with `#[forge::workflow]` |

### Name Conventions

- Model names: PascalCase (e.g., `OrderItem`) - converted automatically
- Function names: snake_case (e.g., `get_orders`) - converted automatically

### Example

```bash
# Add a model
forge add model Task
forge add model OrderItem

# Add functions
forge add query get_tasks
forge add mutation create_task
forge add action sync_inventory

# Add background processing
forge add job send_notification
forge add cron daily_report
forge add workflow order_fulfillment
```

### Generated Templates

#### Model (`forge add model Task`)

```rust
use forge::prelude::*;

#[forge::model]
pub struct Task {
    #[id]
    pub id: Uuid,

    // Add your fields here

    #[default = "now()"]
    pub created_at: Timestamp,

    #[updated_at]
    pub updated_at: Timestamp,
}
```

#### Query (`forge add query get_tasks`)

```rust
use forge::prelude::*;

#[forge::query]
pub async fn get_tasks(ctx: &QueryContext) -> Result<Vec<()>> {
    // Fetch data from database
    Ok(vec![])
}
```

#### Mutation (`forge add mutation create_task`)

```rust
use forge::prelude::*;

#[forge::mutation]
pub async fn create_task(ctx: &MutationContext) -> Result<()> {
    // Insert or update data
    Ok(())
}
```

#### Action (`forge add action sync_inventory`)

```rust
use forge::prelude::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncInventoryResult {
    pub success: bool,
}

#[forge::action]
pub async fn sync_inventory(ctx: &ActionContext) -> Result<SyncInventoryResult> {
    // Call external APIs
    Ok(SyncInventoryResult { success: true })
}
```

#### Job (`forge add job send_notification`)

```rust
use forge::prelude::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendNotificationInput { /* fields */ }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendNotificationOutput { pub success: bool }

#[forge::job]
#[timeout = "5m"]
#[retry(max_attempts = 3, backoff = "exponential")]
pub async fn send_notification(
    ctx: &JobContext,
    _input: SendNotificationInput
) -> Result<SendNotificationOutput> {
    let _ = ctx.progress(50, "Processing...");
    Ok(SendNotificationOutput { success: true })
}
```

#### Cron (`forge add cron daily_report`)

```rust
use forge::prelude::*;

#[forge::cron("0 0 * * *")]  // Daily at midnight UTC
#[timezone = "UTC"]
pub async fn daily_report(ctx: &CronContext) -> Result<()> {
    tracing::info!(run_id = %ctx.run_id, "Running daily_report");
    Ok(())
}
```

#### Workflow (`forge add workflow order_fulfillment`)

```rust
use forge::prelude::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderFulfillmentInput { /* fields */ }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrderFulfillmentOutput { pub success: bool }

#[forge::workflow]
#[version = 1]
#[timeout = "1h"]
pub async fn order_fulfillment(
    ctx: &WorkflowContext,
    _input: OrderFulfillmentInput
) -> Result<OrderFulfillmentOutput> {
    // Step 1
    if !ctx.is_step_completed("validate") {
        ctx.record_step_start("validate");
        // validation logic
        ctx.record_step_complete("validate", serde_json::json!({"status": "ok"}));
    }

    // Step 2
    if !ctx.is_step_completed("process") {
        ctx.record_step_start("process");
        // processing logic
        ctx.record_step_complete("process", serde_json::json!({"status": "ok"}));
    }

    Ok(OrderFulfillmentOutput { success: true })
}
```

---

## `forge generate`

Generates TypeScript client code from your Rust schema and functions. Scans `src/` for `#[forge::model]` and `#[forge::forge_enum]` definitions.

```bash
forge generate [OPTIONS]
```

### Options

| Option | Description |
|--------|-------------|
| `--force` | Regenerate all files even if they exist |
| `-o, --output <dir>` | Output directory (default: `frontend/src/lib/forge`) |
| `-s, --src <dir>` | Source directory to scan (default: `src`) |
| `--skip-runtime` | Skip @forge/svelte runtime regeneration |
| `-y, --yes` | Auto-accept prompts (for CI) |

### Generated Files

| File | Description |
|------|-------------|
| `types.ts` | TypeScript interfaces from Rust models/enums |
| `api.ts` | Function bindings (`createQuery`, `createMutation`) |
| `stores.ts` | Re-exports from @forge/svelte |
| `index.ts` | Barrel exports |

### Runtime Management

The command also manages the `.forge/svelte/` runtime package:
- Detects version mismatches between project and CLI
- Prompts for update when versions differ
- Migrates legacy embedded runtimes to the new structure

### Example

```bash
# Standard generation
forge generate

# Force regeneration
forge generate --force

# Custom directories
forge generate --src backend/src --output frontend/src/forge

# CI mode (no prompts)
forge generate --yes
```

---

## `forge run`

Starts the FORGE server with all components (gateway, workers, scheduler).

```bash
forge run [OPTIONS]
```

### Options

| Option | Description |
|--------|-------------|
| `-c, --config <file>` | Configuration file (default: `forge.toml`) |
| `-p, --port <port>` | Port to listen on (overrides config) |
| `--host <host>` | Host to bind to (overrides config) |
| `--dev` | Enable development mode (verbose logging) |

### What It Starts

- HTTP gateway on configured port (default: 8080)
- RPC endpoints at `/rpc` and `/rpc/:function`
- WebSocket endpoint at `/ws` for real-time subscriptions
- Dashboard at `/_dashboard`
- Background job workers
- Cron scheduler (leader-elected)
- Health check at `/health`

### Example

```bash
# Start with defaults
forge run

# Custom port
forge run --port 3000

# Development mode with custom config
forge run --dev --config config/dev.toml

# Bind to all interfaces
forge run --host 0.0.0.0
```

---

## `forge migrate`

Manages database migrations. Migrations are stored in the `migrations/` directory as numbered SQL files.

```bash
forge migrate <action> [OPTIONS]
```

### Actions

| Action | Description |
|--------|-------------|
| `up` | Run all pending migrations |
| `down [N]` | Rollback the last N migrations (default: 1) |
| `status` | Show migration status |

### Global Options

| Option | Description |
|--------|-------------|
| `-c, --config <file>` | Configuration file (default: `forge.toml`) |
| `-m, --migrations-dir <dir>` | Migrations directory (default: `migrations`) |

### Migration File Format

Migrations use `-- @up` and `-- @down` markers to separate forward and rollback SQL:

```sql
-- @up
CREATE TABLE tasks (
    id UUID PRIMARY KEY,
    title VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_tasks_created_at ON tasks(created_at);

SELECT forge_enable_reactivity('tasks');

-- @down
SELECT forge_disable_reactivity('tasks');
DROP INDEX IF EXISTS idx_tasks_created_at;
DROP TABLE IF EXISTS tasks;
```

### Example

```bash
# Apply all pending migrations
forge migrate up

# Check migration status
forge migrate status

# Rollback last migration
forge migrate down

# Rollback last 3 migrations
forge migrate down 3

# Use custom directories
forge migrate up --config config/prod.toml --migrations-dir db/migrations
```

### Status Output

```
  ✓ Applied:
    ↓ 0001_initial at 2024-01-15 10:30:45
    ↓ 0002_add_tasks at 2024-01-16 09:15:22

  ○ Pending:
    → 0003_add_comments

  ℹ 2 applied, 1 pending

  ↓ = has down migration, - = no down migration
```

---

## Global Options

These options work with all commands:

| Option | Description |
|--------|-------------|
| `--help` | Show help for command |
| `--version` | Show CLI version |

---

## Environment Variables

| Variable | Description | Used By |
|----------|-------------|---------|
| `DATABASE_URL` | PostgreSQL connection string | `forge run`, `forge migrate` |
| `RUST_LOG` | Log level filter (e.g., `debug`, `info`) | `forge run` |
| `VITE_API_URL` | Backend URL for frontend | Generated frontend |

The CLI automatically loads `.env` files in the project directory.

---

## Common Workflows

### Starting a New Feature

```bash
# 1. Add the model
forge add model Comment

# 2. Edit src/schema/comment.rs with your fields

# 3. Create a migration
# migrations/0002_add_comments.sql

# 4. Add functions
forge add query get_comments
forge add mutation create_comment

# 5. Apply migration
forge migrate up

# 6. Generate TypeScript
forge generate

# 7. Restart server
forge run
```

### After Pulling Changes

```bash
# 1. Apply any new migrations
forge migrate up

# 2. Regenerate TypeScript
forge generate

# 3. Rebuild backend
cargo build

# 4. Install frontend deps (if changed)
cd frontend && bun install
```

### Adding Background Processing

```bash
# Add a job
forge add job process_order

# Add a cron
forge add cron nightly_cleanup

# Add a workflow
forge add workflow user_onboarding

# Register in main.rs:
# builder.job_registry_mut().register::<functions::ProcessOrderJob>();
# builder.cron_registry_mut().register::<functions::NightlyCleanupCron>();
# builder.workflow_registry_mut().register::<functions::UserOnboardingWorkflow>();
```

---

## Dashboard

When running `forge run`, the built-in dashboard is available at `/_dashboard`. It provides:

- **Overview**: System health, request stats, active connections
- **Metrics**: Real-time metrics with time-series charts
- **Logs**: HTTP request logs with filtering
- **Traces**: Distributed tracing with waterfall view
- **Jobs**: Background job status, progress, retry management
- **Workflows**: Multi-step workflow progress tracking
- **Crons**: Scheduled task history and controls
- **Cluster**: Node health and leader election status

---

## Troubleshooting

### "Not in a FORGE project" Error

The `add` command requires being in a project directory. Ensure:
- `src/schema/` exists (for models)
- `src/functions/` exists (for functions)

### Migration Errors

If migrations fail:
1. Check `forge migrate status` to see current state
2. Verify DATABASE_URL is correct in `.env`
3. Check SQL syntax in migration files
4. Use `forge migrate down` to rollback if needed

### TypeScript Generation Issues

If `forge generate` produces unexpected output:
1. Ensure `#[forge::model]` is on your structs
2. Check that fields use supported types
3. Use `--force` to regenerate all files
4. Check `--src` points to the correct directory

### WebSocket Connection Issues

If real-time subscriptions don't work:
1. Verify the server is running with `forge run`
2. Check browser console for WebSocket errors
3. Ensure `VITE_API_URL` matches the backend URL
4. Check CORS configuration in `forge.toml`

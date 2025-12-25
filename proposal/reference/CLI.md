# CLI Reference

> *Scaffolding commands for FORGE*

---

## Philosophy

The `forge` CLI is for **scaffolding only**:

- Creating new projects
- Adding models and functions
- Generating TypeScript client code

**Everything else** (migrations, jobs, logs, debugging) is done via the **built-in dashboard** at `http://localhost:8080/_forge/`.

Why? CLI authentication is annoying to manage. The dashboard uses your app's existing session.

---

## Installation

```bash
curl -fsSL https://forge.dev/install.sh | sh
```

---

## Commands

### Project Scaffolding

```bash
# Create new project
forge new <name>
forge new my-app

# Initialize in existing directory
forge init
```

Creates the project structure:

```
my-app/
├── Cargo.toml
├── forge.toml
├── src/
│   ├── main.rs
│   ├── schema/
│   └── functions/
├── frontend/
│   ├── package.json
│   └── src/
└── migrations/
```

### Adding Models

```bash
# Add a new model
forge add model <name>
forge add model Task
forge add model OrderItem
```

Creates `src/schema/<name>.rs`:

```rust
use forge::prelude::*;

#[forge::model]
pub struct Task {
    pub id: Uuid,
    pub created_at: Timestamp,
}
```

### Adding Functions

```bash
# Add a query
forge add query <name>
forge add query get_tasks

# Add a mutation
forge add mutation <name>
forge add mutation create_task

# Add an action
forge add action <name>
forge add action sync_external

# Add a job
forge add job <name>
forge add job send_notification

# Add a cron
forge add cron <name>
forge add cron daily_cleanup
```

Creates the function file with the appropriate boilerplate.

### Code Generation

```bash
# Generate TypeScript client from schema
forge generate

# Regenerate all (useful if generated files get corrupted)
forge generate --force
```

This updates `frontend/src/lib/forge/` with:
- `types.ts` — TypeScript types from Rust models
- `api.ts` — Function bindings
- `stores.ts` — Reactive Svelte stores

---

## Running the App

Use standard Rust and JavaScript tooling, NOT the forge CLI:

### Backend

```bash
# Development with hot reload
cargo watch -x run

# Or just run once
cargo run

# Production build
cargo build --release
```

### Frontend

```bash
cd frontend

# Install dependencies
bun install

# Development
bun run dev

# Production build
bun run build
```

### Migrations & Everything Else

Use the **Dashboard** at `http://localhost:8080/_forge/`:

| Task | Where |
|------|-------|
| Apply migrations | Dashboard → Migrations |
| View/retry jobs | Dashboard → Jobs |
| Trigger crons | Dashboard → Crons |
| View logs | Dashboard → Logs |
| Query metrics | Dashboard → Metrics |
| Debug traces | Dashboard → Traces |
| Run SQL | Dashboard → SQL |

---

## Global Options

| Option | Description |
|--------|-------------|
| `--help` | Show help |
| `--version` | Show version |

---

## Environment Variables

| Variable | Description |
|----------|-------------|
| `DATABASE_URL` | Database connection (for `cargo run`) |
| `FORGE_SECRET` | Encryption key |

---

## Common Workflows

### Starting a New Feature

```bash
# 1. Add the model
forge add model Comment

# 2. Edit the model fields
# src/schema/comment.rs

# 3. Add functions
forge add query get_comments
forge add mutation create_comment

# 4. Generate TypeScript
forge generate

# 5. Apply migration (in dashboard)
# http://localhost:8080/_forge/ → Migrations → Generate → Apply
```

### After Pulling Changes

```bash
# 1. Rebuild backend
cargo build

# 2. Regenerate frontend types
forge generate

# 3. Apply any new migrations (in dashboard)
# http://localhost:8080/_forge/ → Migrations → Apply Pending
```

---

## Related Documentation

- [Development](../development/DEVELOPMENT.md) — Local development workflow
- [Migrations](../development/MIGRATIONS.md) — Schema evolution
- [Configuration](CONFIGURATION.md) — forge.toml reference

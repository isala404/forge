# Local Development

> *World-class developer experience*

---

## Quick Start

```bash
# Create new project
forge new my-app
cd my-app

# Start development
forge dev
```

That's it. You now have:
- Backend running at `http://localhost:8080`
- Frontend running at `http://localhost:5173`
- Dashboard at `http://localhost:8080/_dashboard`
- Hot reload for both backend and frontend

---

## What `forge dev` Does

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                        forge dev                                             │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  1. Starts PostgreSQL (if not running)                                       │
│     - Uses local Postgres, or                                                │
│     - Starts embedded Postgres, or                                           │
│     - Uses SQLite for quick start                                            │
│                                                                              │
│  2. Runs migrations                                                          │
│     - Applies any pending schema changes                                     │
│                                                                              │
│  3. Generates code                                                           │
│     - TypeScript types from Rust schema                                      │
│     - Svelte stores                                                          │
│     - API client                                                             │
│                                                                              │
│  4. Starts backend (with hot reload)                                         │
│     - Watches schema/ and functions/                                         │
│     - Recompiles on change (~500ms)                                          │
│     - Preserves connections during reload                                    │
│                                                                              │
│  5. Starts frontend (Vite)                                                   │
│     - Hot module replacement                                                 │
│     - Instant updates                                                        │
│                                                                              │
│  6. Starts dashboard                                                         │
│     - Built-in observability UI                                              │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Hot Reload

### Backend Hot Reload

When you save a file in `schema/` or `functions/`:

1. **Incremental compilation** — Only changed modules recompile
2. **State preservation** — WebSocket connections stay open
3. **Feedback loop** — Changes compile and reload

```bash
# Watching...
[12:00:01] Changed: functions/mutations/projects.rs
[12:00:01] Compiling...
[12:00:08] ✓ Reloaded (7.2s)
```

### Realistic Compilation Times

**Be aware:** Rust compile times depend heavily on project size and the types of changes made:

| Change Type | Small Project (<20 files) | Medium (50+ models) | Large (200+ functions) |
|-------------|---------------------------|---------------------|------------------------|
| Single function body | 2-5s | 5-10s | 8-15s |
| Add new model | 5-10s | 10-20s | 15-30s |
| Change macro attribute | 10-20s | 20-40s | 30-60s |
| Clean rebuild | 30-60s | 2-5 min | 5-10 min |

**Why Rust is slower than Go/Node/Elixir for hot reload:**
- `sqlx` performs compile-time SQL verification (queries are checked against your database schema)
- Procedural macros (`#[forge::model]`, `#[forge::query]`) must re-expand on changes
- `serde` derive macros add to compile overhead
- Rust's borrow checker and type system require full analysis

**Mitigations:**
- Use `cargo watch` with `--ignore` for non-essential files
- Consider `mold` or `lld` linker for faster linking (10-30% improvement)
- Split large projects into workspace crates
- Use `#[cfg(debug_assertions)]` to skip expensive validations in dev

```toml
# .cargo/config.toml - Faster dev builds
[target.x86_64-unknown-linux-gnu]
linker = "clang"
rustflags = ["-C", "link-arg=-fuse-ld=mold"]

[profile.dev]
opt-level = 0
debug = true
incremental = true
```

**Trade-off:** The compile-time verification catches bugs that would be runtime errors in other languages. A 10-second compile that catches a SQL typo saves debugging time later.

### Schema Changes

When you modify `schema/models.rs`:

1. Migration generated automatically
2. Applied to local database
3. TypeScript types regenerated
4. Frontend notified to refresh types

```bash
[12:00:05] Changed: schema/models.rs
[12:00:05] Generating migration...
[12:00:05] ✓ Added column: users.avatar_url
[12:00:06] ✓ TypeScript types updated
[12:00:06] ✓ Reloaded
```

---

## Database Options

### Option 1: Local PostgreSQL (Recommended)

```bash
# macOS
brew install postgresql
brew services start postgresql

# Ubuntu
sudo apt install postgresql
sudo systemctl start postgresql
```

```toml
# forge.toml
[database]
url = "postgres://localhost/my_app_dev"
```

### Option 2: Embedded (Zero Setup)

```toml
# forge.toml
[database]
embedded = true  # Uses embedded Postgres
```

### Option 3: SQLite (Quick Start)

```toml
# forge.toml
[database]
url = "sqlite://./dev.db"
```

Note: SQLite lacks some PostgreSQL features (LISTEN/NOTIFY, some indexes).

---

## Project Structure

```
my-app/
├── forge.toml              # Configuration
├── schema/
│   └── models.rs           # Data models
├── functions/
│   ├── queries/            # Read operations
│   ├── mutations/          # Write operations
│   ├── actions/            # External calls
│   ├── jobs/               # Background jobs
│   └── crons/              # Scheduled tasks
├── frontend/
│   ├── src/
│   │   ├── lib/
│   │   │   └── forge/      # Generated client
│   │   └── routes/
│   └── svelte.config.js
├── generated/              # Auto-generated (don't edit)
│   ├── migrations/
│   ├── typescript/
│   └── rust/
└── tests/
```

---

## Useful Commands

```bash
# Start dev server
forge dev

# Run with specific database
forge dev --database-url postgres://...

# Generate without starting server
forge generate

# Run tests
forge test

# Open dashboard
forge dashboard

# View logs
forge logs -f

# Database shell
forge db shell
```

---

## IDE Setup

### VS Code

Install extensions:
- `rust-analyzer` — Rust support
- `Svelte for VS Code` — Svelte support
- `FORGE` — FORGE-specific features

### Recommended Settings

```json
{
  "rust-analyzer.cargo.features": ["dev"],
  "editor.formatOnSave": true,
  "svelte.enable-ts-plugin": true
}
```

---

## Debugging

### Backend Debugging

```bash
# Run with debug logging
RUST_LOG=debug forge dev

# Run with specific module logging
RUST_LOG=forge::functions=trace forge dev
```

### Database Queries

```toml
# forge.toml
[observability.logging]
log_queries = true
slow_query_threshold = "10ms"
```

### Frontend Debugging

Svelte DevTools + browser DevTools work as expected.

---

## Testing

```bash
# Run all tests
forge test

# Run specific test
forge test functions::queries::test_get_projects

# Run with coverage
forge test --coverage
```

---

## Common Issues

### Port Already in Use

```bash
# Kill process on port 8080
lsof -ti:8080 | xargs kill -9

# Or use different port
forge dev --port 3000
```

### Database Connection Failed

```bash
# Check PostgreSQL is running
pg_isready

# Check connection string
forge db check
```

### Types Out of Sync

```bash
# Regenerate all types
forge generate --force
```

---

## Related Documentation

- [CLI](../reference/CLI.md) — All commands
- [Configuration](../reference/CONFIGURATION.md) — forge.toml reference
- [Schema](../core/SCHEMA.md) — Defining models

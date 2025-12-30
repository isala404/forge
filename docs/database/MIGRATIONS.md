# Migration System

This document describes the FORGE migration system for managing database schema changes.

---

## Overview

FORGE uses a file-based migration system with the following features:

- **Up/down migrations** with `-- @up` and `-- @down` markers
- **Mesh-safe deploys** using PostgreSQL advisory locks
- **Built-in FORGE tables** versioned separately from user migrations
- **CLI commands** for applying, rolling back, and checking status

---

## Migration File Format

Migrations are SQL files in the `migrations/` directory, named with a numeric prefix for ordering:

```
migrations/
  0001_create_users.sql
  0002_add_posts.sql
  0003_add_comments.sql
```

### Basic Migration (Up Only)

```sql
-- migrations/0001_create_users.sql
CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email VARCHAR(255) UNIQUE NOT NULL,
    name VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_users_email ON users(email);

SELECT forge_enable_reactivity('users');
```

### Migration with Rollback (Up + Down)

Use `-- @up` and `-- @down` markers to define both directions:

```sql
-- migrations/0001_create_users.sql

-- @up
CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email VARCHAR(255) UNIQUE NOT NULL,
    name VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_users_email ON users(email);

SELECT forge_enable_reactivity('users');

-- @down
SELECT forge_disable_reactivity('users');
DROP INDEX IF EXISTS idx_users_email;
DROP TABLE IF EXISTS users;
```

**Syntax notes:**
- `-- @up` marks the start of the upgrade section (optional if no `-- @down`)
- `-- @down` marks the start of the rollback section
- Both markers are case-insensitive (`-- @DOWN` also works)
- Whitespace around markers is flexible (`--@down` also works)

---

## CLI Commands

### Apply Pending Migrations

```bash
forge migrate up
```

Applies all pending migrations in alphabetical order. Built-in FORGE migrations are applied first, then user migrations.

Output:
```
  FORGE Migrations

  -> Running pending migrations...
  [INFO] Applying migration: 0000_forge_internal
  [INFO] Migration applied: 0000_forge_internal
  [INFO] Applying migration: 0001_create_users
  [INFO] Migration applied: 0001_create_users
  -> Migrations complete
```

### Rollback Migrations

```bash
# Rollback the last migration
forge migrate down

# Rollback the last 3 migrations
forge migrate down 3
```

Rolls back migrations in reverse order (most recent first). Requires the migration to have a `-- @down` section.

Output:
```
  FORGE Migrations

  -> Rolling back 1 migration(s)...
  [INFO] Rolling back migration: 0001_create_users
  [INFO] Rolled back migration: 0001_create_users
  -> Rolled back 1 migration(s)
```

If a migration has no `-- @down` section, only the tracking record is removed:
```
[WARN] Migration '0001_create_users' has no down SQL, removing record only
```

### Check Migration Status

```bash
forge migrate status
```

Shows applied and pending migrations:

```
  FORGE Migration Status

  -> Applied:
    v 0000_forge_internal at 2024-01-15 10:30:00
    v 0001_create_users at 2024-01-15 10:30:01

  o Pending:
    -> 0002_add_posts

  i 2 applied, 1 pending

  v = has down migration, - = no down migration
```

### CLI Options

```bash
# Specify config file (default: forge.toml)
forge migrate up --config path/to/forge.toml

# Specify migrations directory (default: migrations)
forge migrate up --migrations-dir db/migrations
```

---

## Migration Runner

The `MigrationRunner` handles migration execution with mesh-safe locking.

### Location

```
crates/forge-runtime/src/migrations/runner.rs
```

### Key Types

```rust
/// A single migration with up and optional down SQL.
pub struct Migration {
    pub name: String,
    pub up_sql: String,
    pub down_sql: Option<String>,
}

/// Information about an applied migration.
pub struct AppliedMigration {
    pub name: String,
    pub applied_at: DateTime<Utc>,
    pub has_down: bool,
}

/// Status of migrations.
pub struct MigrationStatus {
    pub applied: Vec<AppliedMigration>,
    pub pending: Vec<String>,
}
```

### Creating Migrations Programmatically

```rust
// Up-only migration
let m = Migration::new("0001_test", "CREATE TABLE test (id INT);");

// Up + down migration
let m = Migration::with_down(
    "0001_test",
    "CREATE TABLE test (id INT);",
    "DROP TABLE test;"
);

// Parse from file content (auto-detects @up/@down markers)
let m = Migration::parse("0001_test", file_content);
```

### Loading Migrations from Directory

```rust
use forge_runtime::migrations::load_migrations_from_dir;

let migrations = load_migrations_from_dir(Path::new("migrations"))?;
// Returns Vec<Migration> sorted by name
```

---

## Mesh-Safe Deploys

FORGE uses PostgreSQL advisory locks to ensure only one node runs migrations during rolling deploys.

### Advisory Lock

The migration runner acquires an exclusive lock before running migrations:

```rust
// Lock ID derived from "FORGE" in hex
const MIGRATION_LOCK_ID: i64 = 0x464F524745;

// Acquire lock (blocks until acquired)
sqlx::query("SELECT pg_advisory_lock($1)")
    .bind(MIGRATION_LOCK_ID)
    .execute(&pool)
    .await?;

// Run migrations...

// Release lock
sqlx::query("SELECT pg_advisory_unlock($1)")
    .bind(MIGRATION_LOCK_ID)
    .execute(&pool)
    .await?;
```

### Deployment Behavior

When multiple nodes start simultaneously:

1. First node acquires the advisory lock
2. Other nodes block waiting for the lock
3. First node runs all pending migrations
4. First node releases the lock
5. Other nodes acquire the lock, find no pending migrations, release immediately

This ensures:
- Migrations run exactly once
- No race conditions between nodes
- Safe for Kubernetes rolling deploys

---

## Built-in FORGE Tables

FORGE system tables are versioned in a special migration: `0000_forge_internal`.

### Location

```
crates/forge-runtime/migrations/0000_forge_internal.sql
```

This migration is embedded in the binary and applied automatically before user migrations.

### Version Scheme

The built-in migration uses version `0000` to ensure it always runs first:

```
0000_forge_internal   <- Built-in FORGE tables (always first)
0001_create_users     <- User migration
0002_add_posts        <- User migration
```

### Updating Built-in Schema

When FORGE releases a new version with schema changes, the migration system handles upgrades:

1. New tables/columns are added via new migrations
2. Existing migrations are never modified
3. The runner tracks which migrations have been applied

---

## Migration Tracking Table

Applied migrations are tracked in `forge_migrations`:

```sql
CREATE TABLE forge_migrations (
    id SERIAL PRIMARY KEY,
    name VARCHAR(255) UNIQUE NOT NULL,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    down_sql TEXT
);
```

The `down_sql` column stores the rollback SQL for each migration, enabling `forge migrate down` to work even if the original migration file is no longer present.

---

## SQL Statement Splitting

Migrations can contain multiple SQL statements separated by semicolons. The runner splits and executes them individually.

### Dollar-Quoted Functions

The runner correctly handles PL/pgSQL functions with embedded semicolons:

```sql
CREATE FUNCTION my_trigger() RETURNS TRIGGER AS $$
BEGIN
    -- Semicolons inside $$ are preserved
    SELECT 1;
    SELECT 2;
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

-- This is a separate statement
CREATE TABLE other_table (id INT);
```

The parser respects `$$` delimiters and only splits on semicolons outside dollar-quoted strings.

---

## ForgeBuilder Integration

When building a FORGE application, migrations are configured via the builder:

```rust
use forge::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    let config = ForgeConfig::from_file("forge.toml")?;

    Forge::builder()
        .config(config)
        .migrations_dir("migrations")  // Path to user migrations
        .build()?
        .run()
        .await
}
```

Migrations run automatically on startup before the server begins accepting requests.

### Programmatic Migrations

For testing or special cases, add migrations programmatically:

```rust
Forge::builder()
    .config(config)
    .migration(Migration::new("test", "CREATE TABLE test (id INT);"))
    .build()?
    .run()
    .await
```

---

## Best Practices

### 1. Always Include Down Migrations

```sql
-- @up
CREATE TABLE posts (
    id UUID PRIMARY KEY,
    title TEXT NOT NULL
);

-- @down
DROP TABLE posts;
```

This enables rollback during failed deploys.

### 2. Use Sequential Numbering

```
0001_create_users.sql
0002_add_posts.sql
0003_add_comments.sql
```

Migrations run in alphabetical order. Numeric prefixes ensure correct ordering.

### 3. Enable Reactivity for User Tables

```sql
-- At the end of table creation
SELECT forge_enable_reactivity('posts');

-- In down migration
SELECT forge_disable_reactivity('posts');
```

This enables real-time subscriptions for the table.

### 4. Small, Focused Migrations

Each migration should do one thing:
- Add a table
- Add columns to an existing table
- Add indexes
- Backfill data

Avoid large migrations that do many things.

### 5. Test Rollbacks Locally

```bash
# Apply
forge migrate up

# Verify
forge migrate status

# Rollback
forge migrate down

# Re-apply
forge migrate up
```

Ensure your down migrations work correctly before deploying.

---

## Troubleshooting

### Migration Failed Mid-Way

If a migration fails partway through:

1. Check the error message for the failing statement
2. Fix the issue in the database manually if needed
3. Either:
   - Remove the partially-applied changes and retry
   - Manually insert a record in `forge_migrations` to skip the migration

```sql
-- Mark migration as applied (skip it)
INSERT INTO forge_migrations (name) VALUES ('0001_problem_migration');
```

### Lock Timeout

If migrations hang waiting for the advisory lock:

```sql
-- Check who holds the lock
SELECT * FROM pg_stat_activity
WHERE query LIKE '%pg_advisory_lock%';

-- Force release (use with caution)
SELECT pg_advisory_unlock_all();
```

### Schema Drift

If the database was modified outside FORGE:

1. Create a new migration to match current state
2. Mark it as applied without running:

```sql
INSERT INTO forge_migrations (name, down_sql)
VALUES ('0005_manual_fix', NULL);
```

---

## Related Documentation

- [POSTGRES_SCHEMA.md](POSTGRES_SCHEMA.md) - All FORGE system tables

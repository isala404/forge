# Migrations

> *Schema evolution without tears*

---

## Overview

FORGE automatically generates migrations from your schema definitions. When you change a model, FORGE:

1. Detects the diff from current database state
2. Generates SQL migration
3. Applies it (with your approval)

---

## How It Works

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                     MIGRATION WORKFLOW                                       │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│   1. You modify schema/models.rs                                             │
│      ┌─────────────────────────────────────────────────────────────────┐    │
│      │  #[forge::model]                                                 │    │
│      │  pub struct User {                                               │    │
│      │      pub id: Uuid,                                               │    │
│      │      pub email: Email,                                           │    │
│      │      pub name: String,                                           │    │
│      │  +   pub avatar_url: Option<String>,  // NEW FIELD              │    │
│      │  }                                                               │    │
│      └─────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│   2. Run `forge generate`                                                    │
│      - Compares schema to current database                                   │
│      - Detects: "users table needs avatar_url column"                        │
│                                                                              │
│   3. Migration generated                                                     │
│      ┌─────────────────────────────────────────────────────────────────┐    │
│      │  -- migrations/20240115_120000_add_avatar_to_users.sql          │    │
│      │  ALTER TABLE users ADD COLUMN avatar_url VARCHAR(2048);          │    │
│      └─────────────────────────────────────────────────────────────────┘    │
│                                                                              │
│   4. Run `forge db migrate`                                                  │
│      - Reviews migration                                                     │
│      - Applies to database                                                   │
│      - Records in forge_migrations table                                     │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Commands

### Generate Migrations

```bash
# Generate migrations from schema changes
forge generate

# Output:
# ✓ Detected changes:
#   - Add column: users.avatar_url (VARCHAR)
#   - Add index: idx_projects_owner_status
# 
# Generated: migrations/20240115_120000_schema_update.sql
```

### Apply Migrations

```bash
# Apply pending migrations
forge db migrate

# Output:
# Pending migrations:
#   - 20240115_120000_schema_update.sql
# 
# Apply? [y/N] y
# 
# ✓ Applied: 20240115_120000_schema_update.sql (23ms)
```

### Preview Without Applying

```bash
# See what would be applied
forge db migrate --dry-run

# Output:
# Would apply:
# -- 20240115_120000_schema_update.sql
# ALTER TABLE users ADD COLUMN avatar_url VARCHAR(2048);
```

### Rollback

```bash
# Rollback last migration
forge db rollback

# Rollback specific migration
forge db rollback 20240115_120000

# Rollback all (dangerous!)
forge db reset
```

---

## Migration Types

### Safe Changes (Auto-Applied in Dev)

| Change | SQL Generated | Risk |
|--------|--------------|------|
| Add table | `CREATE TABLE ...` | None |
| Add nullable column | `ALTER TABLE ADD COLUMN ...` | None |
| Add index | `CREATE INDEX CONCURRENTLY ...` | None |
| Add enum value | `ALTER TYPE ADD VALUE ...` | None |

### Attention Required

| Change | SQL Generated | Risk |
|--------|--------------|------|
| Add non-null column | `ALTER TABLE ADD COLUMN ... DEFAULT ...` | May be slow on large tables |
| Drop column | `ALTER TABLE DROP COLUMN ...` | Data loss |
| Drop table | `DROP TABLE ...` | Data loss |
| Rename column | `ALTER TABLE RENAME COLUMN ...` | May break queries |
| Change column type | `ALTER TABLE ALTER COLUMN TYPE ...` | May fail |

### Dangerous (Requires Confirmation)

```bash
forge db migrate

# ⚠️  WARNING: This migration will:
#   - DROP COLUMN users.legacy_field (contains 50,000 values)
#   - DROP TABLE old_logs (contains 1,000,000 rows)
# 
# This will result in DATA LOSS.
# 
# Type "I understand" to proceed: 
```

---

## Migration File Format

```sql
-- migrations/20240115_120000_add_projects_table.sql

-- Up migration
CREATE TABLE projects (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    owner_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    name VARCHAR(255) NOT NULL,
    status project_status NOT NULL DEFAULT 'draft',
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_projects_owner ON projects(owner_id);
CREATE INDEX idx_projects_status ON projects(status) WHERE status != 'archived';

-- Add change tracking trigger
CREATE TRIGGER projects_notify_changes
    AFTER INSERT OR UPDATE OR DELETE ON projects
    FOR EACH ROW EXECUTE FUNCTION forge_notify_change();

-- Down migration (optional, for rollback)
-- @down
DROP TRIGGER IF EXISTS projects_notify_changes ON projects;
DROP TABLE IF EXISTS projects;
```

---

## Migration Tracking

```sql
-- FORGE tracks applied migrations
CREATE TABLE forge_migrations (
    id SERIAL PRIMARY KEY,
    version VARCHAR(255) UNIQUE NOT NULL,
    name VARCHAR(255),
    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    checksum VARCHAR(64),  -- To detect tampering
    execution_time_ms INTEGER
);

-- Example contents:
-- | version              | name                    | applied_at          |
-- |----------------------|-------------------------|---------------------|
-- | 20240115_120000      | add_projects_table      | 2024-01-15 12:00:00 |
-- | 20240116_090000      | add_avatar_to_users     | 2024-01-16 09:00:00 |
```

---

## Development Workflow

### Hot Reload in Development

```bash
# Start dev server with auto-migrations
forge dev

# When you save schema/models.rs:
# 1. Changes detected
# 2. Migration generated
# 3. Applied automatically
# 4. Types regenerated
# 5. Server reloaded
```

### Production Workflow

```bash
# 1. Generate migration locally
forge generate

# 2. Review the generated SQL
cat migrations/20240115_120000_*.sql

# 3. Test on staging
forge db migrate --database-url $STAGING_DB

# 4. Deploy to production
forge db migrate --database-url $PROD_DB
```

---

## Handling Complex Migrations

### Adding a Non-Null Column

```rust
// Bad: Will fail if table has rows
pub struct User {
    pub avatar_url: String,  // NOT NULL, no default
}

// Good: Add as nullable first, then backfill
pub struct User {
    pub avatar_url: Option<String>,  // Start nullable
}

// Then later, after backfilling:
pub struct User {
    #[default = "https://default-avatar.png"]
    pub avatar_url: String,  // Now safe to make non-null
}
```

### Renaming Columns

```bash
# FORGE detects renames intelligently
# Before: pub email: String,
# After:  pub email_address: String,

forge generate
# Detected: Rename column users.email -> users.email_address
# 
# Generated SQL:
# ALTER TABLE users RENAME COLUMN email TO email_address;
```

### Data Migrations

For data transformations, create a custom migration:

```bash
# Create empty migration
forge db migration create backfill_user_slugs

# Edit the generated file
```

```sql
-- migrations/20240116_100000_backfill_user_slugs.sql

-- Backfill slugs from names
UPDATE users 
SET slug = lower(regexp_replace(name, '[^a-zA-Z0-9]', '-', 'g'))
WHERE slug IS NULL;

-- @down
UPDATE users SET slug = NULL;
```

---

## Zero-Downtime Migrations

For production deployments without downtime:

### 1. Expand Phase (Before Deploy)

```sql
-- Add new column (nullable)
ALTER TABLE users ADD COLUMN new_email VARCHAR(255);

-- Start writing to both columns
-- (application code writes to old and new)
```

### 2. Migrate Data

```sql
-- Backfill new column
UPDATE users SET new_email = email WHERE new_email IS NULL;
```

### 3. Contract Phase (After Deploy)

```sql
-- Application now reads from new column
-- Drop old column
ALTER TABLE users DROP COLUMN email;
ALTER TABLE users RENAME COLUMN new_email TO email;
```

---

## Schema Introspection

FORGE compares your Rust schema against the live database:

```rust
// What FORGE does internally:

struct SchemaDiff {
    tables_to_add: Vec<TableDef>,
    tables_to_drop: Vec<String>,
    columns_to_add: Vec<ColumnDef>,
    columns_to_drop: Vec<ColumnRef>,
    columns_to_modify: Vec<ColumnChange>,
    indexes_to_add: Vec<IndexDef>,
    indexes_to_drop: Vec<String>,
}

impl SchemaDiff {
    fn from_comparison(schema: &RustSchema, database: &DatabaseSchema) -> Self {
        // Compare tables
        // Compare columns
        // Compare indexes
        // Compare constraints
        // Generate diff
    }
    
    fn to_sql(&self) -> Vec<Migration> {
        // Convert diff to SQL statements
    }
}
```

---

## Best Practices

### 1. Small, Incremental Changes

```bash
# Good: One change per migration
forge generate  # Add avatar_url column
# ... deploy ...
forge generate  # Add projects table
# ... deploy ...

# Bad: Big bang migration
forge generate  # 50 changes at once
```

### 2. Test Migrations

```bash
# Test on copy of production data
pg_dump prod_db | psql test_db
forge db migrate --database-url $TEST_DB
```

### 3. Always Have Down Migrations

```sql
-- Up
CREATE TABLE projects (...);

-- @down
DROP TABLE projects;
```

### 4. Be Careful with Indexes

```sql
-- Bad: Blocks writes on large tables
CREATE INDEX idx_users_email ON users(email);

-- Good: Non-blocking
CREATE INDEX CONCURRENTLY idx_users_email ON users(email);
```

---

## Troubleshooting

### Migration Failed Mid-Way

```bash
# Check current state
forge db status

# If partially applied, manual intervention needed:
psql $DATABASE_URL
# Fix the issue manually
# Then mark migration as applied:
INSERT INTO forge_migrations (version, name) VALUES ('20240115_120000', 'manual_fix');
```

### Schema Drift

```bash
# Database was modified outside FORGE
forge db diff

# Output:
# ⚠️  Schema drift detected:
#   - Column users.temp_field exists in database but not in schema
#   - Index idx_manual_index exists in database but not in schema
# 
# Options:
#   1. Add to schema to keep
#   2. Run `forge db pull` to update schema from database
#   3. Run `forge db push` to force database to match schema
```

---

## Raw SQL Escape Hatch

While FORGE generates migrations from schema changes, you sometimes need direct SQL control:

### When to Use Raw SQL

- **Complex data migrations**: Backfilling with custom logic
- **Performance-critical indexes**: Partial indexes, covering indexes, custom expressions
- **PostgreSQL-specific features**: Full-text search, custom operators, extensions
- **Denormalization**: Materialized views, computed columns
- **Escape the abstraction**: When FORGE's model doesn't fit

### Creating Raw Migrations

```bash
# Create an empty migration file
forge db migration create my_custom_migration

# Or just create the file directly
touch migrations/20240120_150000_custom_optimization.sql
```

### Raw Migration Format

```sql
-- migrations/20240120_150000_custom_optimization.sql

-- @raw: true
-- This marker tells FORGE to apply this migration as-is
-- without attempting to parse or validate it against the schema

-- Add a full-text search index
CREATE INDEX CONCURRENTLY idx_projects_search
ON projects USING gin(to_tsvector('english', name || ' ' || description));

-- Create a materialized view for analytics
CREATE MATERIALIZED VIEW project_stats AS
SELECT
    owner_id,
    count(*) as project_count,
    count(*) FILTER (WHERE status = 'active') as active_count
FROM projects
GROUP BY owner_id;

CREATE UNIQUE INDEX idx_project_stats_owner ON project_stats(owner_id);

-- Refresh policy (manual, via cron)
COMMENT ON MATERIALIZED VIEW project_stats IS 'refresh:manual';

-- @down
DROP MATERIALIZED VIEW IF EXISTS project_stats;
DROP INDEX CONCURRENTLY IF EXISTS idx_projects_search;
```

### Mixing Schema and Raw Migrations

FORGE tracks all migrations in order. Schema-generated and raw migrations coexist:

```
migrations/
├── 0001_initial.sql                     # Generated from schema
├── 0002_add_projects.sql                # Generated from schema
├── 0003_custom_search_index.sql         # Raw SQL (your custom)
├── 0004_add_tasks.sql                   # Generated from schema
└── 0005_performance_tuning.sql          # Raw SQL (your custom)
```

### Schema Drift Warning

Raw migrations can create "drift" between your Rust schema and the database:

```bash
forge db diff

# ⚠️  Schema drift detected:
#   - Index idx_projects_search exists in database but not in schema
#   - Materialized view project_stats exists in database but not in schema
#
# These were likely created by raw migrations. This is expected.
# Use `forge db drift ignore idx_projects_search` to suppress this warning.
```

To acknowledge intentional drift:

```toml
# forge.toml
[database.drift]
# Objects created by raw migrations (don't warn about these)
ignore = [
    "index:idx_projects_search",
    "materialized_view:project_stats",
    "function:my_custom_function"
]
```

### Raw SQL in Schema (Advanced)

For small customizations, embed SQL directly in your schema:

```rust
#[forge::model]
#[sql_after = "CREATE INDEX CONCURRENTLY idx_users_email_lower ON users(lower(email));"]
pub struct User {
    pub id: Uuid,
    pub email: String,
}
```

Or for constraints:

```rust
#[forge::model]
#[sql_constraint = "CHECK (end_date > start_date)"]
pub struct Event {
    pub id: Uuid,
    pub start_date: Date,
    pub end_date: Date,
}
```

### Accessing Raw PostgreSQL Features

```rust
// In your functions, you can always drop to raw SQL
#[forge::query]
pub async fn search_projects(ctx: &QueryContext, query: String) -> Result<Vec<Project>> {
    // FORGE's query builder
    let basic = ctx.query(get_projects_by_owner, owner_id).await?;

    // Or raw SQL when needed
    let results = sqlx::query_as!(
        Project,
        r#"
        SELECT * FROM projects
        WHERE to_tsvector('english', name || ' ' || description) @@ plainto_tsquery($1)
        ORDER BY ts_rank(to_tsvector('english', name || ' ' || description), plainto_tsquery($1)) DESC
        LIMIT 20
        "#,
        query
    )
    .fetch_all(&ctx.pool)
    .await?;

    Ok(results)
}
```

### Best Practices for Raw SQL

1. **Document why**: Always comment explaining why raw SQL was needed
2. **Test thoroughly**: Raw migrations bypass FORGE's validation
3. **Use CONCURRENTLY**: For indexes on production tables
4. **Provide @down**: Always include rollback SQL
5. **Check drift periodically**: Run `forge db diff` to catch unexpected changes

---

## Related Documentation

- [Schema](../core/SCHEMA.md) — Model definitions
- [PostgreSQL Schema](POSTGRES_SCHEMA.md) — Table reference
- [Local Development](../deployment/LOCAL_DEV.md) — Dev workflow

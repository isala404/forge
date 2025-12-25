# Migrations

> *Schema evolution without the pain*

---

## Philosophy

Migrations are:

1. **Generated** — FORGE diffs your schema and generates SQL
2. **Reviewed** — You see exactly what will run before it runs
3. **Versioned** — Stored in git, applied in order
4. **Reversible** — When possible, with auto-generated rollback

---

## Basic Workflow

### 1. Change Your Schema

```rust
// src/schema/project.rs

#[forge::model]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub description: Option<String>,  // ← New field
    pub owner_id: Uuid,
    pub created_at: Timestamp,
}
```

### 2. Generate Migration

Open dashboard: `http://localhost:8080/_forge/`

**Migrations** → **Generate**

FORGE shows you the diff:

```sql
-- Migration: 0003_add_project_description
-- Generated: 2024-01-15 10:30:00

ALTER TABLE projects
ADD COLUMN description TEXT;
```

### 3. Review and Apply

Check the SQL looks right, then click **Apply**.

The migration runs, and the file is saved to `migrations/0003_add_project_description.sql`.

### 4. Commit

```bash
git add migrations/0003_add_project_description.sql
git commit -m "Add project description field"
```

---

## Migration Files

Migrations live in `migrations/`:

```
migrations/
├── 0001_initial.sql
├── 0002_add_users.sql
├── 0003_add_project_description.sql
└── 0004_add_tasks.sql
```

Each file is pure SQL:

```sql
-- migrations/0003_add_project_description.sql

-- Up
ALTER TABLE projects ADD COLUMN description TEXT;

-- Down (optional, for rollback)
ALTER TABLE projects DROP COLUMN description;
```

---

## Rolling Deployments

When deploying to multiple nodes, you need migrations that work with both old and new code running simultaneously.

### The Problem

```
Timeline:
T+0:  All nodes running code v1, schema v1
T+1:  Node 1 gets code v2, runs migration to schema v2
T+2:  Node 2 still running code v1 against schema v2  ← Problem!
T+3:  Node 2 gets code v2
T+4:  All nodes running code v2, schema v2
```

Between T+1 and T+3, you have old code running against new schema.

### The Simple Rule

**Migrations must be backwards-compatible with the previous code version.**

| Change | Safe? | How |
|--------|-------|-----|
| Add nullable column | Yes | Old code ignores it |
| Add column with default | Yes | Old code ignores it, DB fills default |
| Add table | Yes | Old code doesn't query it |
| Remove column | **No** | Old code still queries it |
| Rename column | **No** | Old code uses old name |
| Change column type | **No** | Old code expects old type |

### Two-Phase Migration

For breaking changes, use two deploys:

**Phase 1: Add the new thing**
```sql
-- Migration: 0010_add_email_verified
ALTER TABLE users ADD COLUMN email_verified BOOLEAN DEFAULT false;
```

Deploy. Now both old and new code work (old ignores the column).

**Phase 2: Remove the old thing** (next deploy)
```sql
-- Migration: 0011_remove_legacy_verified
ALTER TABLE users DROP COLUMN legacy_verified_flag;
```

This is only safe after ALL nodes run code that doesn't use `legacy_verified_flag`.

### Dashboard Safety Check

When generating a migration, the dashboard warns about breaking changes:

```
⚠️ WARNING: This migration contains breaking changes

  - DROP COLUMN: users.legacy_email
  - RENAME COLUMN: projects.name → projects.title

These changes will break code that references the old schema.
Ensure all nodes are running code that doesn't use these fields
before applying this migration.

[Apply Anyway] [Cancel]
```

---

## Conflict Resolution

When two developers change the same model:

### Git Handles It

Migrations are timestamped files. Two developers adding different columns:

```
Developer A: 0005_20240115_103000_add_project_status.sql
Developer B: 0005_20240115_103500_add_project_priority.sql
```

Both get merged. No conflict—they're different files.

### When There IS a Conflict

If both developers add a column with the same name:

```sql
-- Developer A
ALTER TABLE projects ADD COLUMN priority INTEGER;

-- Developer B
ALTER TABLE projects ADD COLUMN priority VARCHAR(50);
```

The second migration fails at apply time:

```
ERROR: column "priority" already exists
```

**Resolution:** Talk to your teammate. Decide on the type. One of you deletes their migration.

This is rare and git history makes it obvious who to talk to.

### Migration Ordering

Migrations run in filename order. FORGE uses timestamps:

```
0001_20240110_initial.sql
0002_20240111_add_users.sql
0003_20240115_add_projects.sql
```

If ordering matters, rename files before merging.

---

## Rollback

### Simple Rollback

If a migration has a `-- Down` section:

Dashboard → **Migrations** → **Rollback**

```sql
-- Migration: 0003_add_description
-- Up
ALTER TABLE projects ADD COLUMN description TEXT;

-- Down
ALTER TABLE projects DROP COLUMN description;
```

### No Down Section

If you didn't write a down section, you'll need to write rollback SQL manually:

Dashboard → **SQL** → write and run your rollback → **Migrations** → mark as rolled back

### Rollback in Production

**Be careful.** Rolling back in production:

1. May lose data (DROP COLUMN loses that column's data)
2. May break running code (if code depends on new schema)

For production, prefer **forward fixes**:

```sql
-- Instead of rolling back "ADD COLUMN status"
-- Add another migration to fix the problem
ALTER TABLE projects ALTER COLUMN status SET DEFAULT 'draft';
```

---

## Production Best Practices

### 1. Run Migrations Before Deploy

The safest order:

```
1. Run migration (schema now compatible with new code)
2. Deploy new code (uses new schema)
```

NOT:

```
1. Deploy new code (uses schema that doesn't exist yet!)
2. Run migration
```

### 2. Lock Timeout

Large tables + `ALTER TABLE` = long locks. Set a timeout:

```sql
-- At the top of dangerous migrations
SET lock_timeout = '5s';

ALTER TABLE large_table ADD COLUMN new_col TEXT;
```

If the lock can't be acquired in 5 seconds, the migration fails instead of blocking all queries.

### 3. Concurrent Index Creation

For large tables, create indexes concurrently:

```sql
-- ❌ Locks the table for the duration
CREATE INDEX idx_users_email ON users(email);

-- ✅ Doesn't lock (takes longer but non-blocking)
CREATE INDEX CONCURRENTLY idx_users_email ON users(email);
```

FORGE's migration generator uses `CONCURRENTLY` by default for index creation.

### 4. Backfill Separately

Adding a column with a complex default? Split it:

```sql
-- Migration 1: Add column (fast)
ALTER TABLE projects ADD COLUMN word_count INTEGER;

-- Migration 2: Backfill in batches (separate job, not blocking deploy)
-- Run via dashboard: Jobs → "Backfill word_count"
```

Dashboard → **Jobs** → **Create Backfill Job**:

```rust
#[forge::job]
pub async fn backfill_word_count(ctx: &JobContext) -> Result<()> {
    let projects = ctx.db.query::<Project>()
        .filter(|p| p.word_count.is_none())
        .limit(1000)
        .fetch_all()
        .await?;

    for project in projects {
        let count = count_words(&project.description);
        ctx.db.update::<Project>(project.id)
            .set(|p| p.word_count = Some(count))
            .await?;
    }

    // Re-queue if more rows exist
    if projects.len() == 1000 {
        ctx.dispatch(backfill_word_count, ()).await?;
    }

    Ok(())
}
```

---

## Checking Migration Status

### Dashboard

**Migrations** tab shows:

| Migration | Status | Applied At |
|-----------|--------|------------|
| 0001_initial | Applied | 2024-01-10 |
| 0002_add_users | Applied | 2024-01-11 |
| 0003_add_projects | **Pending** | — |

### Database Table

FORGE tracks applied migrations in `forge_migrations`:

```sql
SELECT * FROM forge_migrations ORDER BY applied_at;

-- name                      | applied_at          | checksum
-- 0001_initial              | 2024-01-10 10:00:00 | abc123...
-- 0002_add_users            | 2024-01-11 09:00:00 | def456...
```

### CI Check

Ensure migrations are applied before tests:

```yaml
# .github/workflows/test.yml
- name: Run migrations
  run: cargo run -- migrate apply

- name: Run tests
  run: cargo test
```

---

## Schema Drift Detection

FORGE can detect when your database schema doesn't match your code:

Dashboard → **Migrations** → **Check Drift**

```
✓ projects table matches schema
✓ users table matches schema
✗ tasks table has drift:
  - Missing column: priority (defined in code, not in DB)
  - Extra column: legacy_status (in DB, not in code)
```

Fix drift by generating a migration or updating your code.

---

## Configuration

```toml
# forge.toml

[migrations]
# Where migration files live
path = "migrations"

# Lock timeout for DDL operations
lock_timeout = "5s"

# Whether to use CONCURRENTLY for indexes
concurrent_indexes = true
```

---

## Related Documentation

- [Development](DEVELOPMENT.md) — Local workflow
- [Schema](../core/SCHEMA.md) — Model definitions
- [Deployment](../deployment/DEPLOYMENT.md) — Production deployment

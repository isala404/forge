# Schema-Driven Development

> *Write once, generate everything*

---

## Philosophy

The schema is the single source of truth. From it, FORGE generates:

- PostgreSQL migrations
- TypeScript types
- Svelte stores
- Validation logic
- OpenAPI documentation
- Database indexes

**You never write SQL. You never write type definitions twice.**

---

## Defining Models

Models are defined in Rust using derive macros:

```rust
// schema/models.rs

use forge::prelude::*;

#[forge::model]
#[table(name = "users")]
pub struct User {
    #[id]
    pub id: Uuid,
    
    #[indexed]
    #[unique]
    pub email: Email,          // Validated email type
    
    pub name: String,
    
    #[encrypted]               // Encrypted at rest
    pub api_key: Option<String>,
    
    #[relation(has_many = "Project", foreign_key = "owner_id")]
    pub projects: Vec<Project>,
    
    #[default = "now()"]
    pub created_at: Timestamp,
    
    #[default = "now()"]
    #[updated_at]
    pub updated_at: Timestamp,
}
```

---

## Attribute Reference

### Table Attributes

| Attribute | Description | Example |
|-----------|-------------|---------|
| `#[table(name = "...")]` | Custom table name | `#[table(name = "users")]` |
| `#[schema(name = "...")]` | PostgreSQL schema | `#[schema(name = "tenant_1")]` |

### Field Attributes

| Attribute | Description | Example |
|-----------|-------------|---------|
| `#[id]` | Primary key (UUID) | `#[id] pub id: Uuid` |
| `#[id(auto)]` | Auto-increment ID | `#[id(auto)] pub id: i64` |
| `#[indexed]` | Create B-tree index | `#[indexed] pub email: String` |
| `#[unique]` | Unique constraint | `#[unique] pub slug: String` |
| `#[nullable]` | Allow NULL | Use `Option<T>` instead |
| `#[default = "..."]` | Default value | `#[default = "now()"]` |
| `#[encrypted]` | Encrypt at rest | `#[encrypted] pub ssn: String` |
| `#[jsonb]` | Store as JSONB | `#[jsonb] pub metadata: Value` |
| `#[updated_at]` | Auto-update on change | `#[updated_at] pub updated_at: Timestamp` |

### Relation Attributes

| Attribute | Description | Example |
|-----------|-------------|---------|
| `#[relation(belongs_to = "...")]` | Foreign key | `#[relation(belongs_to = "User")]` |
| `#[relation(has_many = "...")]` | One-to-many | `#[relation(has_many = "Project")]` |
| `#[relation(has_one = "...")]` | One-to-one | `#[relation(has_one = "Profile")]` |
| `#[relation(many_to_many = "...")]` | Many-to-many via join | See below |

---

## Supported Types

### Scalar Types

| Rust Type | PostgreSQL Type | Notes |
|-----------|-----------------|-------|
| `String` | `VARCHAR(255)` | Default length |
| `String` + `#[max_length = N]` | `VARCHAR(N)` | Custom length |
| `Text` | `TEXT` | Unlimited length |
| `i32` | `INTEGER` | |
| `i64` | `BIGINT` | |
| `f32` | `REAL` | |
| `f64` | `DOUBLE PRECISION` | |
| `bool` | `BOOLEAN` | |
| `Uuid` | `UUID` | |
| `Timestamp` | `TIMESTAMPTZ` | Always with timezone |
| `Date` | `DATE` | |
| `Decimal` | `DECIMAL(19, 4)` | For money |
| `Json<T>` | `JSONB` | Typed JSON |

### Validated Types

FORGE provides validated types that enforce constraints:

```rust
use forge::types::*;

pub struct User {
    pub email: Email,           // Validates email format
    pub phone: PhoneNumber,     // Validates phone format
    pub url: Url,               // Validates URL format
    pub slug: Slug,             // Alphanumeric + hyphens
}
```

### Custom Validated Types

```rust
#[forge::validated_type]
#[pattern = r"^[A-Z]{2,3}$"]
#[error = "Country code must be 2-3 uppercase letters"]
pub struct CountryCode(String);

// Usage
pub struct Address {
    pub country: CountryCode,  // Only accepts "US", "UK", "GBR", etc.
}
```

---

## Enums

Enums become PostgreSQL ENUMs:

```rust
#[forge::enum]
pub enum ProjectStatus {
    Draft,
    Active,
    Paused,
    Completed,
    Archived,
}

#[forge::model]
pub struct Project {
    pub status: ProjectStatus,  // ENUM in PostgreSQL
}
```

Generated SQL:

```sql
CREATE TYPE project_status AS ENUM (
    'draft', 'active', 'paused', 'completed', 'archived'
);

CREATE TABLE projects (
    ...
    status project_status NOT NULL DEFAULT 'draft',
    ...
);
```

### Enum with Values

```rust
#[forge::enum]
pub enum Priority {
    #[value = 1]
    Low,
    #[value = 2]
    Medium,
    #[value = 3]
    High,
    #[value = 4]
    Critical,
}
```

---

## Relations

### One-to-Many

```rust
#[forge::model]
pub struct User {
    #[id]
    pub id: Uuid,
    
    #[relation(has_many = "Project")]
    pub projects: Vec<Project>,
}

#[forge::model]
pub struct Project {
    #[id]
    pub id: Uuid,
    
    #[relation(belongs_to = "User")]
    pub owner_id: Uuid,
    
    #[relation(resolve)]  // Load user when queried
    pub owner: User,
}
```

Generated SQL:

```sql
CREATE TABLE projects (
    id UUID PRIMARY KEY,
    owner_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    ...
);

CREATE INDEX idx_projects_owner_id ON projects(owner_id);
```

### Many-to-Many

```rust
#[forge::model]
pub struct User {
    #[id]
    pub id: Uuid,
    
    #[relation(many_to_many = "Team", through = "team_members")]
    pub teams: Vec<Team>,
}

#[forge::model]
pub struct Team {
    #[id]
    pub id: Uuid,
    
    #[relation(many_to_many = "User", through = "team_members")]
    pub members: Vec<User>,
}

#[forge::join_table]
pub struct TeamMember {
    pub user_id: Uuid,
    pub team_id: Uuid,
    
    #[default = "now()"]
    pub joined_at: Timestamp,
    
    pub role: TeamRole,
}
```

Generated SQL:

```sql
CREATE TABLE team_members (
    user_id UUID NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    team_id UUID NOT NULL REFERENCES teams(id) ON DELETE CASCADE,
    joined_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    role team_role NOT NULL,
    PRIMARY KEY (user_id, team_id)
);

CREATE INDEX idx_team_members_user ON team_members(user_id);
CREATE INDEX idx_team_members_team ON team_members(team_id);
```

---

## Composite Indexes

```rust
#[forge::model]
#[index(fields = ["tenant_id", "created_at"], order = "desc")]
#[index(fields = ["status", "priority"], name = "idx_active_priority")]
pub struct Task {
    #[id]
    pub id: Uuid,
    
    pub tenant_id: Uuid,
    pub status: TaskStatus,
    pub priority: Priority,
    pub created_at: Timestamp,
}
```

Generated SQL:

```sql
CREATE INDEX idx_tasks_tenant_id_created_at ON tasks(tenant_id, created_at DESC);
CREATE INDEX idx_active_priority ON tasks(status, priority);
```

---

## Full-Text Search

```rust
#[forge::model]
pub struct Article {
    #[id]
    pub id: Uuid,
    
    pub title: String,
    
    #[text_search]
    pub content: Text,
    
    #[text_search(weight = "A")]  // Higher weight in search
    pub summary: String,
}
```

Generated SQL:

```sql
CREATE TABLE articles (
    ...
    search_vector TSVECTOR GENERATED ALWAYS AS (
        setweight(to_tsvector('english', COALESCE(summary, '')), 'A') ||
        setweight(to_tsvector('english', COALESCE(content, '')), 'B')
    ) STORED
);

CREATE INDEX idx_articles_search ON articles USING GIN(search_vector);
```

---

## Soft Delete

```rust
#[forge::model]
#[soft_delete]
pub struct Project {
    #[id]
    pub id: Uuid,
    
    pub name: String,
    
    // Automatically added by #[soft_delete]:
    // pub deleted_at: Option<Timestamp>,
}
```

Queries automatically filter out soft-deleted records:

```rust
// This excludes deleted projects
let projects = ctx.db.query::<Project>().fetch_all().await?;

// To include deleted:
let all = ctx.db.query::<Project>().include_deleted().fetch_all().await?;

// To restore:
ctx.db.restore::<Project>(project_id).await?;

// To permanently delete:
ctx.db.hard_delete::<Project>(project_id).await?;
```

---

## Multi-Tenancy

FORGE supports row-level security for multi-tenant applications:

```rust
#[forge::model]
#[tenant(field = "organization_id")]
pub struct Project {
    #[id]
    pub id: Uuid,
    
    pub organization_id: Uuid,  // Tenant identifier
    
    pub name: String,
}
```

All queries are automatically scoped to the current tenant:

```rust
#[forge::query]
pub async fn get_projects(ctx: &QueryContext) -> Result<Vec<Project>> {
    // This automatically adds: WHERE organization_id = <current_org>
    ctx.db.query::<Project>().fetch_all().await
}
```

→ See [Security](../reference/SECURITY.md#multi-tenancy) for setup details.

---

## Generated Code

### PostgreSQL Migrations

When you run `forge generate`, migrations are created:

```sql
-- migrations/20240115120000_create_users.sql

CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email VARCHAR(255) UNIQUE NOT NULL,
    name VARCHAR(255) NOT NULL,
    api_key_encrypted BYTEA,
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);

CREATE INDEX idx_users_email ON users(email);

-- Change tracking trigger
CREATE TRIGGER users_notify_changes
    AFTER INSERT OR UPDATE OR DELETE ON users
    FOR EACH ROW EXECUTE FUNCTION forge_notify_change();
```

### TypeScript Types

```typescript
// generated/types.ts

export interface User {
  id: string;
  email: string;
  name: string;
  createdAt: Date;
  updatedAt: Date;
}

export interface Project {
  id: string;
  ownerId: string;
  name: string;
  status: 'draft' | 'active' | 'paused' | 'completed' | 'archived';
}

export interface CreateUserInput {
  email: string;
  name: string;
}

export interface UpdateUserInput {
  email?: string;
  name?: string;
}
```

### Svelte Stores

```typescript
// generated/stores.ts

import { createForgeStore } from '$lib/forge';

export const users = createForgeStore<User[]>('users');
export const currentUser = createForgeStore<User | null>('currentUser');
export const projects = createForgeStore<Project[]>('projects');

// These stores:
// - Auto-subscribe to real-time updates
// - Update when mutations complete
// - Handle loading and error states
```

→ See [Stores](../frontend/STORES.md) for usage.

---

## Schema Evolution

### Safe Changes (Automatic)

- Adding new tables
- Adding new nullable columns
- Adding new indexes
- Adding new enums
- Adding new enum values (at the end)

### Requires Attention

- Removing columns (data loss warning)
- Removing tables (data loss warning)
- Renaming columns/tables (generates rename migration)
- Changing column types (may fail at runtime)

### Migration Commands

```bash
# Generate migrations from schema changes
forge generate

# Preview migrations without applying
forge db migrate --dry-run

# Apply migrations
forge db migrate

# Rollback last migration
forge db rollback

# Rollback all migrations (dangerous!)
forge db reset
```

→ See [Migrations](../database/MIGRATIONS.md) for details.

---

## Type Safety Across Boundaries

The schema creates type safety from database to UI:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                        TYPE SAFETY CHAIN                                     │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  Schema (Rust)           Generated (Rust)        Generated (TypeScript)     │
│  ┌─────────────────┐     ┌─────────────────┐     ┌─────────────────────┐   │
│  │ pub struct User │ ──► │ impl Queryable  │ ──► │ interface User      │   │
│  │   pub id: Uuid  │     │   for User      │     │   id: string        │   │
│  │   pub email: ...│     │ impl Insertable │     │   email: string     │   │
│  └─────────────────┘     └─────────────────┘     └─────────────────────┘   │
│                                                           │                  │
│                                                           ▼                  │
│                                                  ┌─────────────────────┐    │
│                                                  │ Svelte Component    │    │
│                                                  │ $users.map(user =>  │    │
│                                                  │   user.email // ✓   │    │
│                                                  │   user.foo   // ✗   │    │
│                                                  │ )                   │    │
│                                                  └─────────────────────┘    │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

If you change the schema, TypeScript will show errors where your code needs updating.

---

## Best Practices

### 1. Use Semantic Types

```rust
// ❌ Avoid
pub struct User {
    pub email: String,
    pub phone: String,
}

// ✅ Prefer
pub struct User {
    pub email: Email,
    pub phone: PhoneNumber,
}
```

### 2. Index Query Patterns

```rust
// If you frequently query by status + created_at:
#[index(fields = ["status", "created_at"])]
pub struct Task {
    ...
}
```

### 3. Use Enums for Finite States

```rust
// ❌ Avoid
pub status: String,  // Can be anything

// ✅ Prefer
pub status: TaskStatus,  // Enforced at DB level
```

### 4. Prefer JSONB for Flexible Data

```rust
// For settings, preferences, or schema-flexible data:
#[jsonb]
pub settings: ProjectSettings,
```

---

## Related Documentation

- [Functions](FUNCTIONS.md) — Using models in functions
- [Migrations](../database/MIGRATIONS.md) — Managing schema changes
- [Stores](../frontend/STORES.md) — Generated Svelte stores
- [Security](../reference/SECURITY.md) — Row-level security

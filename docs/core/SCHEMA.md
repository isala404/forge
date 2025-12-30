# Schema System

The FORGE schema system provides type-safe database modeling through Rust proc macros. Define your models once in Rust and FORGE generates SQL migrations, TypeScript types, and provides runtime type mappings.

## Models

### Defining Models with `#[forge::model]`

The `#[forge::model]` attribute macro transforms a Rust struct into a database model with full type metadata:

```rust
use forge::prelude::*;

#[forge::model]
#[table(name = "users")]
pub struct User {
    #[id]
    pub id: Uuid,

    #[indexed]
    #[unique]
    pub email: String,

    pub name: String,

    #[encrypted]
    pub api_key: Option<String>,

    #[default = "now()"]
    pub created_at: Timestamp,

    #[default = "now()"]
    #[updated_at]
    pub updated_at: Timestamp,
}
```

The macro generates:
- `Debug`, `Clone`, `Serialize`, `Deserialize` derives
- Implementation of the `ModelMeta` trait
- Table definition with field metadata

### Table Name Resolution

By default, the table name is derived from the struct name:
- `User` becomes `users` (snake_case + pluralized)
- `ProjectTask` becomes `project_tasks`

Override with the `#[table(name = "...")]` attribute:

```rust
#[forge::model]
#[table(name = "team_members")]
pub struct TeamMember {
    // ...
}
```

Pluralization rules:
- Words ending in s, sh, ch, x, z: add "es" (e.g., `status` -> `statuses`)
- Words ending in consonant + y: change to "ies" (e.g., `category` -> `categories`)
- Other words: add "s" (e.g., `user` -> `users`)

## Field Attributes

### Primary Key: `#[id]`

Marks a field as the primary key:

```rust
#[forge::model]
pub struct User {
    #[id]
    pub id: Uuid,
}
```

Generated SQL: `id UUID PRIMARY KEY DEFAULT gen_random_uuid()`

If no `#[id]` attribute is present, FORGE assumes a field named `id` is the primary key.

### Indexing: `#[indexed]`

Creates a B-tree index on the field:

```rust
#[forge::model]
pub struct Task {
    #[indexed]
    pub status: TaskStatus,
}
```

Generated SQL: `CREATE INDEX idx_tasks_status ON tasks(status);`

### Unique Constraint: `#[unique]`

Adds a unique constraint:

```rust
#[forge::model]
pub struct User {
    #[unique]
    pub email: String,
}
```

Generated SQL: `email VARCHAR(255) NOT NULL UNIQUE`

### Encryption: `#[encrypted]`

Marks a field for encryption at rest:

```rust
#[forge::model]
pub struct User {
    #[encrypted]
    pub api_key: Option<String>,
}
```

Note: Encryption logic must be implemented separately; the attribute provides metadata.

### Auto-Update Timestamp: `#[updated_at]`

Marks a timestamp field to auto-update on row modification:

```rust
#[forge::model]
pub struct User {
    #[updated_at]
    pub updated_at: Timestamp,
}
```

### Default Values: `#[default = "..."]`

Sets the SQL default value:

```rust
#[forge::model]
pub struct User {
    #[default = "now()"]
    pub created_at: Timestamp,

    #[default = "'active'"]
    pub status: String,
}
```

Note: UUID primary keys automatically get `DEFAULT gen_random_uuid()`.

## Type Mappings

### Rust to SQL Type Mappings

| Rust Type | SQL Type | Notes |
|-----------|----------|-------|
| `String` | `VARCHAR(255)` | Default length |
| `Uuid` | `UUID` | From `uuid` crate |
| `i32` | `INTEGER` | 32-bit signed |
| `i64` | `BIGINT` | 64-bit signed |
| `f32` | `REAL` | 32-bit float |
| `f64` | `DOUBLE PRECISION` | 64-bit float |
| `bool` | `BOOLEAN` | |
| `DateTime<Utc>` | `TIMESTAMPTZ` | From `chrono` crate |
| `NaiveDate` | `DATE` | From `chrono` crate |
| `Value` / `Json` | `JSONB` | From `serde_json` |
| `Vec<u8>` | `BYTEA` | Binary data |
| `Option<T>` | (nullable) | Underlying type becomes nullable |
| `Vec<T>` | `T[]` | PostgreSQL array |
| Custom enum | enum name | PostgreSQL ENUM type |

### Rust to TypeScript Type Mappings

| Rust Type | TypeScript Type |
|-----------|----------------|
| `String` | `string` |
| `Uuid` | `string` |
| `i32`, `i64`, `f32`, `f64` | `number` |
| `bool` | `boolean` |
| `DateTime<Utc>`, `NaiveDate` | `Date` |
| `Value` / `Json` | `unknown` |
| `Vec<u8>` | `Uint8Array` |
| `Option<T>` | `T \| null` |
| `Vec<T>` | `T[]` |
| Custom type | same name (preserved) |

## Enums

### Defining Enums with `#[forge::forge_enum]`

The `#[forge::forge_enum]` attribute creates a PostgreSQL ENUM type:

```rust
use forge::prelude::*;

#[forge::forge_enum]
pub enum ProjectStatus {
    Draft,
    Active,
    Paused,
    Completed,
}
```

The macro generates:
- `Debug`, `Clone`, `Copy`, `PartialEq`, `Eq`, `Hash`, `Serialize`, `Deserialize` derives
- `#[serde(rename_all = "snake_case")]` for consistent JSON serialization
- `as_sql_str()` method returning the SQL string value
- `sql_type_name()` static method returning the PostgreSQL type name
- `Display` and `FromStr` implementations
- sqlx `Decode`, `Encode`, and `Type` implementations for PostgreSQL

### SQL Type Name

The SQL type name is derived from the enum name in snake_case:
- `ProjectStatus` becomes `project_status`
- `TaskPriority` becomes `task_priority`

### Variant Values

Variant names are converted to snake_case for SQL storage:
- `Draft` becomes `'draft'`
- `InProgress` becomes `'in_progress'`

Example generated SQL:
```sql
CREATE TYPE project_status AS ENUM (
    'draft',
    'active',
    'paused',
    'completed'
);
```

### Variant Integer Values

You can associate integer values with variants using `#[value = N]`:

```rust
#[forge::forge_enum]
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

Note: The integer values are stored as metadata but PostgreSQL ENUMs still use string values for storage.

### Generated TypeScript

Enums become TypeScript union types:

```typescript
export type ProjectStatus = 'draft' | 'active' | 'paused' | 'completed';
```

## Schema Registry

The `SchemaRegistry` collects all model and enum definitions for code generation:

```rust
use forge::forge_core::schema::{SchemaRegistry, EnumDef, EnumVariant};

let registry = SchemaRegistry::new();

// Register a table
registry.register_table(User::table_def());

// Register an enum
let mut enum_def = EnumDef::new("ProjectStatus");
enum_def.variants.push(EnumVariant::new("Draft"));
enum_def.variants.push(EnumVariant::new("Active"));
registry.register_enum(enum_def);

// Query registered definitions
let all_tables = registry.all_tables();
let all_enums = registry.all_enums();
```

### TableDef Structure

```rust
pub struct TableDef {
    pub name: String,              // Table name in database
    pub schema: Option<String>,    // Optional PostgreSQL schema
    pub struct_name: String,       // Rust struct name
    pub fields: Vec<FieldDef>,     // Field definitions
    pub indexes: Vec<IndexDef>,    // Single-column indexes
    pub composite_indexes: Vec<CompositeIndexDef>,
    pub soft_delete: bool,         // Soft delete support
    pub tenant_field: Option<String>, // Multi-tenancy field
    pub doc: Option<String>,       // Documentation
}
```

### FieldDef Structure

```rust
pub struct FieldDef {
    pub name: String,              // Rust field name
    pub column_name: String,       // SQL column name
    pub rust_type: RustType,       // Rust type enum
    pub sql_type: SqlType,         // SQL type enum
    pub nullable: bool,            // Whether nullable
    pub attributes: Vec<FieldAttribute>,
    pub default: Option<String>,   // SQL default value
    pub doc: Option<String>,       // Documentation
}
```

### FieldAttribute Enum

```rust
pub enum FieldAttribute {
    Id,                // Primary key (UUID)
    IdAuto,            // Auto-incrementing primary key
    Indexed,           // B-tree index
    Unique,            // Unique constraint
    Encrypted,         // Encrypt at rest
    Jsonb,             // Store as JSONB
    UpdatedAt,         // Auto-update timestamp
    MaxLength(u32),    // Maximum string length
    BelongsTo(String), // Foreign key relation
    HasMany(String),   // One-to-many relation
    HasOne(String),    // One-to-one relation
    ManyToMany { target: String, through: String },
}
```

## SQL Generation

### CREATE TABLE

Generate SQL for a table:

```rust
let table = User::table_def();
let sql = table.to_create_table_sql();
```

Example output:
```sql
CREATE TABLE users (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    email VARCHAR(255) NOT NULL UNIQUE,
    name VARCHAR(255) NOT NULL,
    created_at TIMESTAMPTZ NOT NULL DEFAULT now(),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT now()
);

CREATE INDEX idx_users_email ON users(email);

CREATE TRIGGER users_notify_changes
    AFTER INSERT OR UPDATE OR DELETE ON users
    FOR EACH ROW EXECUTE FUNCTION forge_notify_change();
```

### CREATE TYPE (Enums)

Generate SQL for an enum:

```rust
let enum_def = EnumDef::new("ProjectStatus");
enum_def.variants.push(EnumVariant::new("Draft"));
enum_def.variants.push(EnumVariant::new("Active"));
let sql = enum_def.to_create_type_sql();
```

Output:
```sql
CREATE TYPE project_status AS ENUM (
    'draft',
    'active'
);
```

## TypeScript Generation

### Interface Generation

Generate TypeScript interfaces from tables:

```rust
let ts = table.to_typescript_interface();
```

Output:
```typescript
export interface User {
  id: string;
  email: string;
  name: string;
  createdAt: Date;
  updatedAt: Date;
}
```

### Union Type Generation

Generate TypeScript from enums:

```rust
let ts = enum_def.to_typescript();
```

Output:
```typescript
export type ProjectStatus = 'draft' | 'active';
```

## Composite Indexes

Define multi-column indexes using `CompositeIndexDef`:

```rust
use forge::forge_core::schema::{CompositeIndexDef, IndexType, IndexOrder};

let idx = CompositeIndexDef {
    name: Some("idx_tasks_status_priority".to_string()),
    columns: vec!["status".to_string(), "priority".to_string()],
    orders: vec![IndexOrder::Asc, IndexOrder::Desc],
    index_type: IndexType::Btree,
    unique: false,
    where_clause: None,
};

let sql = idx.to_sql("tasks");
// CREATE INDEX idx_tasks_status_priority ON tasks(status, priority DESC);
```

### Index Types

```rust
pub enum IndexType {
    Btree,  // Default, good for equality and range queries
    Hash,   // Equality queries only
    Gin,    // Full-text search, JSONB
    Gist,   // Geometric data, full-text search
}
```

## ModelMeta Trait

All models implement the `ModelMeta` trait:

```rust
pub trait ModelMeta: Sized {
    const TABLE_NAME: &'static str;

    fn table_def() -> TableDef;
    fn primary_key_field() -> &'static str;
}
```

Usage:
```rust
// Get table name
let name = User::TABLE_NAME;  // "users"

// Get full table definition
let def = User::table_def();

// Get primary key field name
let pk = User::primary_key_field();  // "id"
```

## Practical Usage

### Current Recommended Pattern

While `#[forge::model]` generates metadata, the current scaffolded projects use a simpler pattern with sqlx directly:

```rust
use forge::prelude::*;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, sqlx::FromRow)]
pub struct User {
    pub id: Uuid,
    pub email: String,
    pub name: String,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
}
```

This provides:
- Direct sqlx integration with `FromRow`
- Full Serde serialization
- No proc macro compilation overhead

Use `#[forge::model]` when you need:
- Automatic SQL generation
- Schema registry integration
- TypeScript type generation from Rust source

### Combining Both Patterns

```rust
// For schema registry and codegen
#[forge::model]
#[table(name = "projects")]
pub struct ProjectSchema {
    #[id]
    pub id: Uuid,
    pub name: String,
    pub status: ProjectStatus,
}

// For runtime queries
#[derive(Debug, Clone, Serialize, Deserialize, sqlx::FromRow)]
pub struct Project {
    pub id: Uuid,
    pub name: String,
    pub status: ProjectStatus,
}
```

## Related Documentation

- [Functions](./FUNCTIONS.md) - Query, mutation, and action definitions
- [Migrations](../database/MIGRATIONS.md) - Database migration system
- [TypeScript Codegen](../frontend/CODEGEN.md) - Generated TypeScript types

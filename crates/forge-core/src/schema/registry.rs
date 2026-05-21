//! Schema registration system.
//!
//! The [`SchemaRegistry`] collects all schema definitions at startup. Proc macros
//! (`#[forge::model]`, `#[forge::query]`, etc.) register their definitions here.
//!
//! The registry uses `BTreeMap` for deterministic iteration order, which ensures
//! consistent TypeScript generation and migration diffing across runs.
//!
//! # Thread Safety
//!
//! Registration happens during single-threaded startup. After startup, the registry
//! is read-only. `RwLock` provides interior mutability for the registration phase.

use std::collections::BTreeMap;
use std::sync::RwLock;

use super::function::FunctionDef;
use super::model::TableDef;

const LOCK_POISONED: &str = "schema registry lock poisoned";

/// Global registry of all schema definitions.
/// This is populated at compile time by the proc macros.
/// Uses BTreeMap for deterministic iteration order.
pub struct SchemaRegistry {
    /// All registered tables by name.
    tables: RwLock<BTreeMap<String, TableDef>>,

    /// All registered enums by name.
    enums: RwLock<BTreeMap<String, EnumDef>>,

    /// All registered functions by name.
    functions: RwLock<BTreeMap<String, FunctionDef>>,
}

impl SchemaRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            tables: RwLock::new(BTreeMap::new()),
            enums: RwLock::new(BTreeMap::new()),
            functions: RwLock::new(BTreeMap::new()),
        }
    }

    /// Register a table definition.
    pub fn register_table(&self, table: TableDef) {
        let mut tables = self.tables.write().expect(LOCK_POISONED);
        tables.insert(table.name.clone(), table);
    }

    /// Register an enum definition.
    pub fn register_enum(&self, enum_def: EnumDef) {
        let mut enums = self.enums.write().expect(LOCK_POISONED);
        enums.insert(enum_def.name.clone(), enum_def);
    }

    /// Register a function definition.
    pub fn register_function(&self, func: FunctionDef) {
        let mut functions = self.functions.write().expect(LOCK_POISONED);
        functions.insert(func.name.clone(), func);
    }

    /// Get a table by name.
    pub fn get_table(&self, name: &str) -> Option<TableDef> {
        let tables = self.tables.read().expect(LOCK_POISONED);
        tables.get(name).cloned()
    }

    /// Get an enum by name.
    pub fn get_enum(&self, name: &str) -> Option<EnumDef> {
        let enums = self.enums.read().expect(LOCK_POISONED);
        enums.get(name).cloned()
    }

    /// Get a function by name.
    pub fn get_function(&self, name: &str) -> Option<FunctionDef> {
        let functions = self.functions.read().expect(LOCK_POISONED);
        functions.get(name).cloned()
    }

    /// Get all registered tables.
    pub fn all_tables(&self) -> Vec<TableDef> {
        let tables = self.tables.read().expect(LOCK_POISONED);
        tables.values().cloned().collect()
    }

    /// Get all registered enums.
    pub fn all_enums(&self) -> Vec<EnumDef> {
        let enums = self.enums.read().expect(LOCK_POISONED);
        enums.values().cloned().collect()
    }

    /// Get all registered functions.
    pub fn all_functions(&self) -> Vec<FunctionDef> {
        let functions = self.functions.read().expect(LOCK_POISONED);
        functions.values().cloned().collect()
    }

    /// Clear all registrations (useful for testing).
    pub fn clear(&self) {
        self.tables.write().expect(LOCK_POISONED).clear();
        self.enums.write().expect(LOCK_POISONED).clear();
        self.functions.write().expect(LOCK_POISONED).clear();
    }
}

impl Default for SchemaRegistry {
    fn default() -> Self {
        Self::new()
    }
}

/// Enum type definition.
#[derive(Debug, Clone)]
pub struct EnumDef {
    /// Enum name in Rust.
    pub name: String,

    /// Type name in SQL (lowercase).
    pub sql_name: String,

    /// Enum variants.
    pub variants: Vec<EnumVariant>,

    /// Documentation comment.
    pub doc: Option<String>,
}

impl EnumDef {
    /// Create a new enum definition.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            sql_name: to_snake_case(name),
            variants: Vec::new(),
            doc: None,
        }
    }

    /// Generate CREATE TYPE SQL.
    pub fn to_create_type_sql(&self) -> String {
        let values: Vec<String> = self
            .variants
            .iter()
            .map(|v| format!("'{}'", v.sql_value))
            .collect();

        format!(
            "CREATE TYPE {} AS ENUM (\n    {}\n);",
            self.sql_name,
            values.join(",\n    ")
        )
    }
}

/// Enum variant definition.
#[derive(Debug, Clone)]
pub struct EnumVariant {
    /// Variant name in Rust.
    pub name: String,

    /// Value in SQL (lowercase).
    pub sql_value: String,

    /// Optional integer discriminant value.
    pub int_value: Option<i32>,

    /// Documentation comment.
    pub doc: Option<String>,
}

impl EnumVariant {
    /// Create a new variant.
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            sql_value: to_snake_case(name),
            int_value: None,
            doc: None,
        }
    }
}

use crate::util::to_snake_case;

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use crate::schema::field::FieldDef;
    use crate::schema::model::TableDef;
    use crate::schema::types::RustType;

    #[test]
    fn test_registry_basic() {
        let registry = SchemaRegistry::new();

        let mut table = TableDef::new("users", "User");
        table.fields.push(FieldDef::new("id", RustType::Uuid));

        registry.register_table(table.clone());

        let retrieved = registry.get_table("users").unwrap();
        assert_eq!(retrieved.name, "users");
        assert_eq!(retrieved.struct_name, "User");
    }

    #[test]
    fn test_enum_def() {
        let mut enum_def = EnumDef::new("ProjectStatus");
        enum_def.variants.push(EnumVariant::new("Draft"));
        enum_def.variants.push(EnumVariant::new("Active"));
        enum_def.variants.push(EnumVariant::new("Completed"));

        let sql = enum_def.to_create_type_sql();
        assert!(sql.contains("CREATE TYPE project_status AS ENUM"));
        assert!(sql.contains("'draft'"));
        assert!(sql.contains("'active'"));
    }
}

use std::collections::HashMap;
use std::sync::RwLock;

use super::function::FunctionDef;
use super::model::TableDef;

/// Global registry of all schema definitions.
/// This is populated at compile time by the proc macros.
pub struct SchemaRegistry {
    /// All registered tables by name.
    tables: RwLock<HashMap<String, TableDef>>,

    /// All registered enums by name.
    enums: RwLock<HashMap<String, EnumDef>>,

    /// All registered functions by name.
    functions: RwLock<HashMap<String, FunctionDef>>,
}

impl SchemaRegistry {
    /// Create a new empty registry.
    pub fn new() -> Self {
        Self {
            tables: RwLock::new(HashMap::new()),
            enums: RwLock::new(HashMap::new()),
            functions: RwLock::new(HashMap::new()),
        }
    }

    /// Register a table definition.
    pub fn register_table(&self, table: TableDef) {
        let mut tables = self.tables.write().unwrap();
        tables.insert(table.name.clone(), table);
    }

    /// Register an enum definition.
    pub fn register_enum(&self, enum_def: EnumDef) {
        let mut enums = self.enums.write().unwrap();
        enums.insert(enum_def.name.clone(), enum_def);
    }

    /// Register a function definition.
    pub fn register_function(&self, func: FunctionDef) {
        let mut functions = self.functions.write().unwrap();
        functions.insert(func.name.clone(), func);
    }

    /// Get a table by name.
    pub fn get_table(&self, name: &str) -> Option<TableDef> {
        let tables = self.tables.read().unwrap();
        tables.get(name).cloned()
    }

    /// Get an enum by name.
    pub fn get_enum(&self, name: &str) -> Option<EnumDef> {
        let enums = self.enums.read().unwrap();
        enums.get(name).cloned()
    }

    /// Get a function by name.
    pub fn get_function(&self, name: &str) -> Option<FunctionDef> {
        let functions = self.functions.read().unwrap();
        functions.get(name).cloned()
    }

    /// Get all registered tables.
    pub fn all_tables(&self) -> Vec<TableDef> {
        let tables = self.tables.read().unwrap();
        tables.values().cloned().collect()
    }

    /// Get all registered enums.
    pub fn all_enums(&self) -> Vec<EnumDef> {
        let enums = self.enums.read().unwrap();
        enums.values().cloned().collect()
    }

    /// Get all registered functions.
    pub fn all_functions(&self) -> Vec<FunctionDef> {
        let functions = self.functions.read().unwrap();
        functions.values().cloned().collect()
    }

    /// Clear all registrations (useful for testing).
    pub fn clear(&self) {
        self.tables.write().unwrap().clear();
        self.enums.write().unwrap().clear();
        self.functions.write().unwrap().clear();
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

    /// Generate TypeScript union type.
    pub fn to_typescript(&self) -> String {
        let values: Vec<String> = self
            .variants
            .iter()
            .map(|v| format!("'{}'", v.sql_value))
            .collect();

        format!("export type {} = {};", self.name, values.join(" | "))
    }
}

/// Enum variant definition.
#[derive(Debug, Clone)]
pub struct EnumVariant {
    /// Variant name in Rust.
    pub name: String,

    /// Value in SQL (lowercase).
    pub sql_value: String,

    /// Optional integer value.
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

/// Convert a string to snake_case.
fn to_snake_case(s: &str) -> String {
    let mut result = String::new();
    for (i, c) in s.chars().enumerate() {
        if c.is_uppercase() {
            if i > 0 {
                result.push('_');
            }
            result.push(c.to_lowercase().next().unwrap());
        } else {
            result.push(c);
        }
    }
    result
}

/// Global schema registry instance.
/// Models register themselves here when their constructors are called.
#[allow(dead_code)]
static GLOBAL_REGISTRY: std::sync::LazyLock<SchemaRegistry> =
    std::sync::LazyLock::new(SchemaRegistry::new);

/// Get the global schema registry.
#[allow(dead_code)]
pub fn global_registry() -> &'static SchemaRegistry {
    &GLOBAL_REGISTRY
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::schema::field::{FieldAttribute, FieldDef};
    use crate::schema::model::TableDef;
    use crate::schema::types::RustType;

    #[test]
    fn test_registry_basic() {
        let registry = SchemaRegistry::new();

        let mut table = TableDef::new("users", "User");
        let mut id_field = FieldDef::new("id", RustType::Uuid);
        id_field.attributes.push(FieldAttribute::Id);
        table.fields.push(id_field);

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

        let ts = enum_def.to_typescript();
        assert!(ts.contains("export type ProjectStatus"));
        assert!(ts.contains("'draft'"));
    }
}

use serde::{Deserialize, Serialize};

use super::types::{RustType, SqlType};

/// Definition of a model field.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FieldDef {
    /// Field name in Rust (snake_case).
    pub name: String,

    /// Column name in SQL (may differ from field name).
    pub column_name: String,

    /// Rust type.
    pub rust_type: RustType,

    /// SQL type.
    pub sql_type: SqlType,

    /// Whether the field is nullable.
    pub nullable: bool,

    /// Field attributes.
    pub attributes: Vec<FieldAttribute>,

    /// Default value expression (SQL).
    pub default: Option<String>,

    /// Documentation comment.
    pub doc: Option<String>,
}

impl FieldDef {
    /// Create a new field definition.
    pub fn new(name: &str, rust_type: RustType) -> Self {
        let sql_type = rust_type.to_sql_type();
        let nullable = rust_type.is_nullable();
        let column_name = to_snake_case(name);

        Self {
            name: name.to_string(),
            column_name,
            rust_type,
            sql_type,
            nullable,
            attributes: Vec::new(),
            default: None,
            doc: None,
        }
    }

    /// Check if this field is a primary key.
    pub fn is_primary_key(&self) -> bool {
        self.attributes
            .iter()
            .any(|a| matches!(a, FieldAttribute::Id | FieldAttribute::IdAuto))
    }

    /// Check if this field is indexed.
    pub fn is_indexed(&self) -> bool {
        self.attributes
            .iter()
            .any(|a| matches!(a, FieldAttribute::Indexed))
    }

    /// Check if this field is unique.
    pub fn is_unique(&self) -> bool {
        self.attributes
            .iter()
            .any(|a| matches!(a, FieldAttribute::Unique))
    }

    /// Check if this field is encrypted.
    pub fn is_encrypted(&self) -> bool {
        self.attributes
            .iter()
            .any(|a| matches!(a, FieldAttribute::Encrypted))
    }

    /// Check if this field auto-updates on modification.
    pub fn is_updated_at(&self) -> bool {
        self.attributes
            .iter()
            .any(|a| matches!(a, FieldAttribute::UpdatedAt))
    }

    /// Generate SQL column definition.
    pub fn to_sql_column(&self) -> String {
        let mut parts = vec![self.column_name.clone(), self.sql_type.to_sql()];

        if self.is_primary_key() {
            parts.push("PRIMARY KEY".to_string());
        }

        if !self.nullable && !self.is_primary_key() {
            parts.push("NOT NULL".to_string());
        }

        if self.is_unique() && !self.is_primary_key() {
            parts.push("UNIQUE".to_string());
        }

        if let Some(ref default) = self.default {
            parts.push(format!("DEFAULT {}", default));
        } else if self.is_primary_key() && matches!(self.sql_type, SqlType::Uuid) {
            parts.push("DEFAULT gen_random_uuid()".to_string());
        }

        parts.join(" ")
    }

    /// Generate TypeScript field.
    pub fn to_typescript(&self) -> String {
        let ts_type = self.rust_type.to_typescript();
        let optional = if self.nullable { "?" } else { "" };
        format!("  {}{}: {};", to_camel_case(&self.name), optional, ts_type)
    }
}

/// Field type representation for simpler cases.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldType {
    Scalar,
    Relation,
    Computed,
}

/// Field attributes applied via proc macros.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum FieldAttribute {
    /// Primary key (UUID).
    Id,
    /// Auto-incrementing primary key.
    IdAuto,
    /// Create an index on this field.
    Indexed,
    /// Unique constraint.
    Unique,
    /// Encrypt at rest.
    Encrypted,
    /// Store as JSONB.
    Jsonb,
    /// Auto-update on modification.
    UpdatedAt,
    /// Maximum length for strings.
    MaxLength(u32),
    /// Foreign key relation.
    BelongsTo(String),
    /// One-to-many relation.
    HasMany(String),
    /// One-to-one relation.
    HasOne(String),
    /// Many-to-many relation with join table.
    ManyToMany { target: String, through: String },
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

/// Convert a string to camelCase.
fn to_camel_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = false;
    for c in s.chars() {
        if c == '_' {
            capitalize_next = true;
        } else if capitalize_next {
            result.push(c.to_uppercase().next().unwrap());
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_field_def_basic() {
        let field = FieldDef::new("email", RustType::String);
        assert_eq!(field.name, "email");
        assert_eq!(field.column_name, "email");
        assert!(!field.nullable);
    }

    #[test]
    fn test_field_def_nullable() {
        let field = FieldDef::new("avatar_url", RustType::Option(Box::new(RustType::String)));
        assert!(field.nullable);
    }

    #[test]
    fn test_field_to_sql_column() {
        let mut field = FieldDef::new("id", RustType::Uuid);
        field.attributes.push(FieldAttribute::Id);
        let sql = field.to_sql_column();
        assert!(sql.contains("PRIMARY KEY"));
        assert!(sql.contains("DEFAULT gen_random_uuid()"));
    }

    #[test]
    fn test_to_snake_case() {
        assert_eq!(to_snake_case("createdAt"), "created_at");
        assert_eq!(to_snake_case("userId"), "user_id");
        assert_eq!(to_snake_case("HTTPServer"), "h_t_t_p_server");
    }

    #[test]
    fn test_to_camel_case() {
        assert_eq!(to_camel_case("created_at"), "createdAt");
        assert_eq!(to_camel_case("user_id"), "userId");
    }
}

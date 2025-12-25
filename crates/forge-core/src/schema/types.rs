use serde::{Deserialize, Serialize};

/// PostgreSQL column types.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum SqlType {
    /// UUID type
    Uuid,
    /// Variable-length string with optional max length
    Varchar(Option<u32>),
    /// Unlimited text
    Text,
    /// 32-bit integer
    Integer,
    /// 64-bit integer
    BigInt,
    /// 32-bit floating point
    Real,
    /// 64-bit floating point
    DoublePrecision,
    /// Boolean
    Boolean,
    /// Timestamp with timezone
    Timestamptz,
    /// Date without time
    Date,
    /// Decimal with precision and scale
    Decimal(u8, u8),
    /// JSONB for structured data
    Jsonb,
    /// Byte array
    Bytea,
    /// Custom enum type
    Enum(String),
    /// Array of another type
    Array(Box<SqlType>),
}

impl SqlType {
    /// Generate the SQL type declaration.
    pub fn to_sql(&self) -> String {
        match self {
            SqlType::Uuid => "UUID".to_string(),
            SqlType::Varchar(None) => "VARCHAR(255)".to_string(),
            SqlType::Varchar(Some(len)) => format!("VARCHAR({})", len),
            SqlType::Text => "TEXT".to_string(),
            SqlType::Integer => "INTEGER".to_string(),
            SqlType::BigInt => "BIGINT".to_string(),
            SqlType::Real => "REAL".to_string(),
            SqlType::DoublePrecision => "DOUBLE PRECISION".to_string(),
            SqlType::Boolean => "BOOLEAN".to_string(),
            SqlType::Timestamptz => "TIMESTAMPTZ".to_string(),
            SqlType::Date => "DATE".to_string(),
            SqlType::Decimal(p, s) => format!("DECIMAL({}, {})", p, s),
            SqlType::Jsonb => "JSONB".to_string(),
            SqlType::Bytea => "BYTEA".to_string(),
            SqlType::Enum(name) => name.clone(),
            SqlType::Array(inner) => format!("{}[]", inner.to_sql()),
        }
    }
}

/// Rust type information for code generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum RustType {
    /// String type
    String,
    /// UUID from uuid crate
    Uuid,
    /// 32-bit integer
    I32,
    /// 64-bit integer
    I64,
    /// 32-bit float
    F32,
    /// 64-bit float
    F64,
    /// Boolean
    Bool,
    /// Chrono DateTime
    DateTime,
    /// Chrono NaiveDate
    Date,
    /// serde_json::Value
    Json,
    /// Vec<u8>
    Bytes,
    /// Option wrapper
    Option(Box<RustType>),
    /// Vec wrapper
    Vec(Box<RustType>),
    /// Custom type (enum or newtype)
    Custom(String),
}

impl RustType {
    /// Convert a Rust type string to RustType.
    pub fn from_type_string(type_str: &str) -> Self {
        match type_str.trim() {
            "String" => RustType::String,
            "Uuid" => RustType::Uuid,
            "i32" => RustType::I32,
            "i64" => RustType::I64,
            "f32" => RustType::F32,
            "f64" => RustType::F64,
            "bool" => RustType::Bool,
            "DateTime<Utc>" | "Timestamp" => RustType::DateTime,
            "NaiveDate" | "Date" => RustType::Date,
            "Value" | "Json" => RustType::Json,
            "Vec<u8>" => RustType::Bytes,
            s if s.starts_with("Option<") && s.ends_with('>') => {
                let inner = &s[7..s.len() - 1];
                RustType::Option(Box::new(RustType::from_type_string(inner)))
            }
            s if s.starts_with("Vec<") && s.ends_with('>') => {
                let inner = &s[4..s.len() - 1];
                RustType::Vec(Box::new(RustType::from_type_string(inner)))
            }
            s => RustType::Custom(s.to_string()),
        }
    }

    /// Map to corresponding SQL type.
    pub fn to_sql_type(&self) -> SqlType {
        match self {
            RustType::String => SqlType::Varchar(None),
            RustType::Uuid => SqlType::Uuid,
            RustType::I32 => SqlType::Integer,
            RustType::I64 => SqlType::BigInt,
            RustType::F32 => SqlType::Real,
            RustType::F64 => SqlType::DoublePrecision,
            RustType::Bool => SqlType::Boolean,
            RustType::DateTime => SqlType::Timestamptz,
            RustType::Date => SqlType::Date,
            RustType::Json => SqlType::Jsonb,
            RustType::Bytes => SqlType::Bytea,
            RustType::Option(inner) => inner.to_sql_type(),
            RustType::Vec(inner) => SqlType::Array(Box::new(inner.to_sql_type())),
            RustType::Custom(name) => SqlType::Enum(name.to_lowercase()),
        }
    }

    /// Check if this type is nullable.
    pub fn is_nullable(&self) -> bool {
        matches!(self, RustType::Option(_))
    }

    /// Generate TypeScript type.
    pub fn to_typescript(&self) -> String {
        match self {
            RustType::String => "string".to_string(),
            RustType::Uuid => "string".to_string(),
            RustType::I32 | RustType::I64 => "number".to_string(),
            RustType::F32 | RustType::F64 => "number".to_string(),
            RustType::Bool => "boolean".to_string(),
            RustType::DateTime | RustType::Date => "Date".to_string(),
            RustType::Json => "unknown".to_string(),
            RustType::Bytes => "Uint8Array".to_string(),
            RustType::Option(inner) => format!("{} | null", inner.to_typescript()),
            RustType::Vec(inner) => format!("{}[]", inner.to_typescript()),
            RustType::Custom(name) => name.clone(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sql_type_to_sql() {
        assert_eq!(SqlType::Uuid.to_sql(), "UUID");
        assert_eq!(SqlType::Varchar(Some(100)).to_sql(), "VARCHAR(100)");
        assert_eq!(SqlType::Decimal(10, 2).to_sql(), "DECIMAL(10, 2)");
    }

    #[test]
    fn test_rust_type_parsing() {
        assert_eq!(RustType::from_type_string("String"), RustType::String);
        assert_eq!(RustType::from_type_string("Uuid"), RustType::Uuid);
        assert_eq!(
            RustType::from_type_string("Option<String>"),
            RustType::Option(Box::new(RustType::String))
        );
        assert_eq!(
            RustType::from_type_string("Vec<i32>"),
            RustType::Vec(Box::new(RustType::I32))
        );
    }

    #[test]
    fn test_rust_type_to_sql() {
        assert_eq!(RustType::String.to_sql_type(), SqlType::Varchar(None));
        assert_eq!(RustType::Uuid.to_sql_type(), SqlType::Uuid);
        assert_eq!(RustType::I64.to_sql_type(), SqlType::BigInt);
    }

    #[test]
    fn test_rust_type_to_typescript() {
        assert_eq!(RustType::String.to_typescript(), "string");
        assert_eq!(RustType::I32.to_typescript(), "number");
        assert_eq!(
            RustType::Option(Box::new(RustType::String)).to_typescript(),
            "string | null"
        );
    }
}

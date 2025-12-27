//! Rust source code parser for extracting FORGE schema definitions.
//!
//! This module parses Rust source files to extract model and enum definitions
//! without requiring compilation.

use std::path::Path;

use forge_core::schema::{EnumDef, EnumVariant, FieldDef, RustType, SchemaRegistry, TableDef};
use syn::{Attribute, Expr, Fields, Lit, Meta};
use walkdir::WalkDir;

use crate::Error;

/// Parse all Rust source files in a directory and extract schema definitions.
pub fn parse_project(src_dir: &Path) -> Result<SchemaRegistry, Error> {
    let registry = SchemaRegistry::new();

    for entry in WalkDir::new(src_dir)
        .into_iter()
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().map(|ext| ext == "rs").unwrap_or(false))
    {
        let content = std::fs::read_to_string(entry.path())?;
        if let Err(e) = parse_file(&content, &registry) {
            tracing::debug!(file = ?entry.path(), error = %e, "Failed to parse file");
        }
    }

    Ok(registry)
}

/// Parse a single Rust source file and extract schema definitions.
fn parse_file(content: &str, registry: &SchemaRegistry) -> Result<(), Error> {
    let file = syn::parse_file(content).map_err(|e| Error::Template(e.to_string()))?;

    for item in file.items {
        match item {
            syn::Item::Struct(item_struct) => {
                if has_forge_model_attr(&item_struct.attrs) {
                    if let Some(table) = parse_model(&item_struct) {
                        registry.register_table(table);
                    }
                }
            }
            syn::Item::Enum(item_enum) => {
                if has_forge_enum_attr(&item_enum.attrs) {
                    if let Some(enum_def) = parse_enum(&item_enum) {
                        registry.register_enum(enum_def);
                    }
                }
            }
            _ => {}
        }
    }

    Ok(())
}

/// Check if attributes contain #[forge::model] or #[model].
fn has_forge_model_attr(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        let path = attr.path();
        path.is_ident("model")
            || path.segments.len() == 2
                && path.segments[0].ident == "forge"
                && path.segments[1].ident == "model"
    })
}

/// Check if attributes contain #[forge_enum] or #[forge::enum_type].
fn has_forge_enum_attr(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        let path = attr.path();
        path.is_ident("forge_enum")
            || path.is_ident("enum_type")
            || path.segments.len() == 2
                && path.segments[0].ident == "forge"
                && path.segments[1].ident == "enum_type"
    })
}

/// Parse a struct with #[model] attribute into a TableDef.
fn parse_model(item: &syn::ItemStruct) -> Option<TableDef> {
    let struct_name = item.ident.to_string();
    let table_name = get_table_name_from_attrs(&item.attrs).unwrap_or_else(|| {
        let snake = to_snake_case(&struct_name);
        pluralize(&snake)
    });

    let mut table = TableDef::new(&table_name, &struct_name);

    // Extract documentation
    table.doc = get_doc_comment(&item.attrs);

    // Extract fields
    if let Fields::Named(fields) = &item.fields {
        for field in &fields.named {
            if let Some(field_name) = &field.ident {
                let field_def = parse_field(field_name.to_string(), &field.ty, &field.attrs);
                table.fields.push(field_def);
            }
        }
    }

    Some(table)
}

/// Parse a field definition.
fn parse_field(name: String, ty: &syn::Type, attrs: &[Attribute]) -> FieldDef {
    let rust_type = type_to_rust_type(ty);
    let mut field = FieldDef::new(&name, rust_type);
    field.column_name = to_snake_case(&name);
    field.doc = get_doc_comment(attrs);

    // Parse field attributes
    for attr in attrs {
        let path = attr.path();
        if path.is_ident("id") {
            field
                .attributes
                .push(forge_core::schema::FieldAttribute::Id);
        } else if path.is_ident("indexed") {
            field
                .attributes
                .push(forge_core::schema::FieldAttribute::Indexed);
        } else if path.is_ident("unique") {
            field
                .attributes
                .push(forge_core::schema::FieldAttribute::Unique);
        } else if path.is_ident("encrypted") {
            field
                .attributes
                .push(forge_core::schema::FieldAttribute::Encrypted);
        } else if path.is_ident("updated_at") {
            field
                .attributes
                .push(forge_core::schema::FieldAttribute::UpdatedAt);
        } else if path.is_ident("default") {
            if let Some(value) = get_attribute_string_value(attr) {
                field.default = Some(value);
            }
        }
    }

    field
}

/// Parse an enum with #[forge_enum] attribute into an EnumDef.
fn parse_enum(item: &syn::ItemEnum) -> Option<EnumDef> {
    let enum_name = item.ident.to_string();
    let mut enum_def = EnumDef::new(&enum_name);
    enum_def.doc = get_doc_comment(&item.attrs);

    for variant in &item.variants {
        let variant_name = variant.ident.to_string();
        let mut enum_variant = EnumVariant::new(&variant_name);
        enum_variant.doc = get_doc_comment(&variant.attrs);

        // Check for explicit value
        if let Some((_, Expr::Lit(lit))) = &variant.discriminant {
            if let Lit::Int(int_lit) = &lit.lit {
                if let Ok(value) = int_lit.base10_parse::<i32>() {
                    enum_variant.int_value = Some(value);
                }
            }
        }

        enum_def.variants.push(enum_variant);
    }

    Some(enum_def)
}

/// Convert a syn::Type to RustType.
fn type_to_rust_type(ty: &syn::Type) -> RustType {
    let type_str = quote::quote!(#ty).to_string().replace(' ', "");

    // Handle common types
    match type_str.as_str() {
        "String" | "&str" => RustType::String,
        "i32" => RustType::I32,
        "i64" => RustType::I64,
        "f32" => RustType::F32,
        "f64" => RustType::F64,
        "bool" => RustType::Bool,
        "Uuid" | "uuid::Uuid" => RustType::Uuid,
        "DateTime<Utc>" | "chrono::DateTime<Utc>" | "chrono::DateTime<chrono::Utc>" => {
            RustType::DateTime
        }
        "NaiveDate" | "chrono::NaiveDate" => RustType::Date,
        "NaiveTime" | "chrono::NaiveTime" => RustType::Custom("NaiveTime".to_string()),
        "serde_json::Value" | "Value" => RustType::Json,
        "Vec<u8>" => RustType::Bytes,
        _ => {
            // Handle Option<T>
            if let Some(inner) = type_str
                .strip_prefix("Option<")
                .and_then(|s| s.strip_suffix('>'))
            {
                let inner_type = match inner {
                    "String" => RustType::String,
                    "i32" => RustType::I32,
                    "i64" => RustType::I64,
                    "f64" => RustType::F64,
                    "bool" => RustType::Bool,
                    "Uuid" => RustType::Uuid,
                    _ => RustType::String, // Default fallback
                };
                return RustType::Option(Box::new(inner_type));
            }

            // Handle Vec<T>
            if let Some(inner) = type_str
                .strip_prefix("Vec<")
                .and_then(|s| s.strip_suffix('>'))
            {
                let inner_type = match inner {
                    "String" => RustType::String,
                    "i32" => RustType::I32,
                    "u8" => return RustType::Bytes,
                    _ => RustType::String,
                };
                return RustType::Vec(Box::new(inner_type));
            }

            // Default to custom type
            RustType::Custom(type_str)
        }
    }
}

/// Get #[table(name = "...")] value from attributes.
fn get_table_name_from_attrs(attrs: &[Attribute]) -> Option<String> {
    for attr in attrs {
        if attr.path().is_ident("table") {
            if let Meta::List(list) = &attr.meta {
                let tokens = list.tokens.to_string();
                if let Some(value) = extract_name_value(&tokens) {
                    return Some(value);
                }
            }
        }
    }
    None
}

/// Get string value from attribute like #[attr = "value"].
fn get_attribute_string_value(attr: &Attribute) -> Option<String> {
    if let Meta::NameValue(nv) = &attr.meta {
        if let Expr::Lit(lit) = &nv.value {
            if let Lit::Str(s) = &lit.lit {
                return Some(s.value());
            }
        }
    }
    None
}

/// Get documentation comment from attributes.
fn get_doc_comment(attrs: &[Attribute]) -> Option<String> {
    let docs: Vec<String> = attrs
        .iter()
        .filter_map(|attr| {
            if attr.path().is_ident("doc") {
                get_attribute_string_value(attr)
            } else {
                None
            }
        })
        .collect();

    if docs.is_empty() {
        None
    } else {
        Some(
            docs.into_iter()
                .map(|s| s.trim().to_string())
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }
}

/// Extract name value from "name = \"value\"" format.
fn extract_name_value(s: &str) -> Option<String> {
    let parts: Vec<&str> = s.splitn(2, '=').collect();
    if parts.len() == 2 {
        let value = parts[1].trim();
        if let Some(stripped) = value.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            return Some(stripped.to_string());
        }
    }
    None
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

/// Simple English pluralization.
fn pluralize(s: &str) -> String {
    if s.ends_with('s')
        || s.ends_with("sh")
        || s.ends_with("ch")
        || s.ends_with('x')
        || s.ends_with('z')
    {
        format!("{}es", s)
    } else if let Some(stem) = s.strip_suffix('y') {
        if !s.ends_with("ay") && !s.ends_with("ey") && !s.ends_with("oy") && !s.ends_with("uy") {
            format!("{}ies", stem)
        } else {
            format!("{}s", s)
        }
    } else {
        format!("{}s", s)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_model_source() {
        let source = r#"
            #[model]
            struct User {
                #[id]
                id: Uuid,
                email: String,
                name: Option<String>,
                #[indexed]
                created_at: DateTime<Utc>,
            }
        "#;

        let registry = SchemaRegistry::new();
        parse_file(source, &registry).unwrap();

        let table = registry.get_table("users").unwrap();
        assert_eq!(table.struct_name, "User");
        assert_eq!(table.fields.len(), 4);
    }

    #[test]
    fn test_parse_enum_source() {
        let source = r#"
            #[forge_enum]
            enum ProjectStatus {
                Draft,
                Active,
                Completed,
            }
        "#;

        let registry = SchemaRegistry::new();
        parse_file(source, &registry).unwrap();

        let enum_def = registry.get_enum("ProjectStatus").unwrap();
        assert_eq!(enum_def.variants.len(), 3);
    }

    #[test]
    fn test_to_snake_case() {
        assert_eq!(to_snake_case("UserProfile"), "user_profile");
        assert_eq!(to_snake_case("ID"), "i_d");
        assert_eq!(to_snake_case("createdAt"), "created_at");
    }

    #[test]
    fn test_pluralize() {
        assert_eq!(pluralize("user"), "users");
        assert_eq!(pluralize("category"), "categories");
        assert_eq!(pluralize("box"), "boxes");
        assert_eq!(pluralize("address"), "addresses");
    }
}

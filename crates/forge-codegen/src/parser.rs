//! Rust source code parser for extracting FORGE schema definitions.
//!
//! Parses Rust source files using `syn` to extract model, enum, and function
//! definitions without requiring compilation.
//!
//! Key design decisions:
//! - Context arguments are matched against `KNOWN_CONTEXT_TYPES`, a fixed list
//!   of the 8 Forge context types. User-defined types like `AppContext` are not
//!   accidentally skipped.
//! - `Result<T, E>` extraction uses `syn::TypePath` recursion, handling
//!   arbitrarily nested generics.
//! - Unparseable inner types become `RustType::Custom(original_string)` instead
//!   of silently falling back to `String`.
//! - `NaiveTime` correctly maps to `RustType::LocalTime`.

use std::path::{Path, PathBuf};

use forge_core::schema::{
    EnumDef, EnumVariant, FieldDef, FunctionArg, FunctionDef, FunctionKind, RustType,
    SchemaRegistry, TableDef,
};
use forge_core::util::to_snake_case;
use std::collections::BTreeMap;

use quote::ToTokens;
use syn::{Attribute, Expr, Fields, FnArg, Lit, Meta, Pat, ReturnType};

use crate::Error;

fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().is_some_and(|ext| ext == "rs") {
            out.push(path);
        }
    }
}

/// Parse all Rust source files in a directory and extract schema definitions.
pub fn parse_project(src_dir: &Path) -> Result<SchemaRegistry, Error> {
    let registry = SchemaRegistry::new();

    let mut files = Vec::new();
    collect_rs_files(src_dir, &mut files);
    files.sort();

    for path in &files {
        let content = std::fs::read_to_string(path)?;
        if let Err(e) = parse_file(&content, &registry) {
            tracing::debug!(file = ?path, error = %e, "Failed to parse file");
        }
    }

    Ok(registry)
}

/// Validate every function in the registry uses types the binding emitters
/// support. Surfaces platform-dependent types like `usize`/`isize` and
/// unsupported integer widths up front instead of letting them fall through to
/// silently-lossy `number` mappings on the frontend.
pub fn validate_registry(registry: &SchemaRegistry) -> Result<(), Vec<String>> {
    let mut errors = Vec::new();
    for func in registry.all_functions() {
        for arg in &func.args {
            collect_unsupported(&arg.rust_type, &func.name, Some(&arg.name), &mut errors);
        }
        collect_unsupported(&func.return_type, &func.name, None, &mut errors);
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

fn collect_unsupported(
    ty: &RustType,
    func_name: &str,
    arg_name: Option<&str>,
    errors: &mut Vec<String>,
) {
    match ty {
        RustType::Option(inner) | RustType::Vec(inner) => {
            collect_unsupported(inner, func_name, arg_name, errors);
        }
        RustType::Custom(name) => {
            if let Some(reason) = unsupported_type_reason(name) {
                let location = match arg_name {
                    Some(arg) => format!("{}.{}", func_name, arg),
                    None => format!("{}() return type", func_name),
                };
                errors.push(format!("{}: {}", location, reason));
            }
        }
        _ => {}
    }
}

fn unsupported_type_reason(name: &str) -> Option<String> {
    match name {
        "usize" | "isize" => Some(format!(
            "`{}` is platform-dependent and not portable across the wire. \
             Use `i32`, `i64`, or `u32` instead.",
            name
        )),
        "u8" | "u16" | "u32" | "u64" | "u128" | "i8" | "i16" | "i128" => Some(format!(
            "`{}` is not supported in handler signatures. \
             Use `i32` or `i64` (signed integers) for portability.",
            name
        )),
        _ => None,
    }
}

/// Scan a source directory and return every `(kind, name)` pair that appears in more
/// than one file. The returned map is keyed by `"kind:name"` with a list of file paths
/// as the value.
pub fn find_duplicate_handlers(src_dir: &Path) -> Result<BTreeMap<String, Vec<PathBuf>>, Error> {
    let mut occurrences: BTreeMap<String, Vec<PathBuf>> = BTreeMap::new();

    let mut files = Vec::new();
    collect_rs_files(src_dir, &mut files);
    files.sort();

    for path in &files {
        let content = match std::fs::read_to_string(path) {
            Ok(c) => c,
            Err(_) => continue,
        };
        let file = match syn::parse_file(&content) {
            Ok(f) => f,
            Err(_) => continue,
        };
        for item in &file.items {
            if let syn::Item::Fn(item_fn) = item
                && let Some(func) = parse_function(item_fn)
            {
                let key = format!("{}:{}", func.kind.as_str(), func.name);
                occurrences.entry(key).or_default().push(path.clone());
            }
        }
    }

    Ok(occurrences
        .into_iter()
        .filter(|(_, paths)| paths.len() > 1)
        .collect())
}

/// Parse a single Rust source file and extract schema definitions.
fn parse_file(content: &str, registry: &SchemaRegistry) -> Result<(), Error> {
    let file = syn::parse_file(content).map_err(|e| Error::Parse {
        file: String::new(),
        message: e.to_string(),
    })?;

    for item in file.items {
        match item {
            syn::Item::Struct(item_struct) => {
                if has_forge_attr(&item_struct.attrs, "model") {
                    if let Some(table) = parse_model(&item_struct) {
                        registry.register_table(table);
                    }
                } else if has_serde_derive(&item_struct.attrs)
                    && let Some(table) = parse_dto_struct(&item_struct)
                {
                    registry.register_table(table);
                }
            }
            syn::Item::Enum(item_enum) => {
                if (has_forge_enum_attr(&item_enum.attrs) || has_serde_derive(&item_enum.attrs))
                    && let Some(enum_def) = parse_enum(&item_enum)
                {
                    registry.register_enum(enum_def);
                }
            }
            syn::Item::Fn(item_fn) => {
                if let Some(func) = parse_function(&item_fn) {
                    registry.register_function(func);
                }
            }
            _ => {}
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Attribute detection
// ---------------------------------------------------------------------------

/// Check if attributes contain `#[forge::name]` or `#[name]`.
fn has_forge_attr(attrs: &[Attribute], name: &str) -> bool {
    attrs.iter().any(|attr| {
        let path = attr.path();
        path.is_ident(name)
            || matches!(
                (path.segments.first(), path.segments.get(1), path.segments.get(2)),
                (Some(first), Some(second), None)
                    if first.ident == "forge" && second.ident == name
            )
    })
}

/// Check if attributes contain `#[forge_enum]`, `#[enum_type]`, or `#[forge::enum_type]`.
fn has_forge_enum_attr(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        let path = attr.path();
        path.is_ident("forge_enum")
            || path.is_ident("enum_type")
            || matches!(
                (path.segments.first(), path.segments.get(1), path.segments.get(2)),
                (Some(first), Some(second), None)
                    if first.ident == "forge"
                        && (second.ident == "enum_type" || second.ident == "forge_enum")
            )
    })
}

/// Check if attributes contain `#[derive(...Serialize...)]` or `#[derive(...Deserialize...)]`.
fn has_serde_derive(attrs: &[Attribute]) -> bool {
    attrs.iter().any(|attr| {
        if !attr.path().is_ident("derive") {
            return false;
        }
        let tokens = attr.meta.to_token_stream().to_string();
        tokens.contains("Serialize") || tokens.contains("Deserialize")
    })
}

// ---------------------------------------------------------------------------
// Struct/model parsing
// ---------------------------------------------------------------------------

/// Parse a DTO struct (with Serialize/Deserialize) into a TableDef.
fn parse_dto_struct(item: &syn::ItemStruct) -> Option<TableDef> {
    let struct_name = item.ident.to_string();
    let mut table = TableDef::new(&struct_name, &struct_name);
    table.is_dto = true;
    table.doc = get_doc_comment(&item.attrs);

    if let Fields::Named(fields) = &item.fields {
        for field in &fields.named {
            if let Some(field_name) = &field.ident {
                table
                    .fields
                    .push(parse_field(field_name.to_string(), &field.ty, &field.attrs));
            }
        }
    }

    Some(table)
}

/// Parse a struct with `#[model]` attribute into a TableDef.
fn parse_model(item: &syn::ItemStruct) -> Option<TableDef> {
    let struct_name = item.ident.to_string();
    let table_name = get_table_name_from_attrs(&item.attrs).unwrap_or_else(|| {
        let snake = to_snake_case(&struct_name);
        pluralize(&snake)
    });

    let mut table = TableDef::new(&table_name, &struct_name);
    table.doc = get_doc_comment(&item.attrs);

    if let Fields::Named(fields) = &item.fields {
        for field in &fields.named {
            if let Some(field_name) = &field.ident {
                table
                    .fields
                    .push(parse_field(field_name.to_string(), &field.ty, &field.attrs));
            }
        }
    }

    Some(table)
}

fn parse_field(name: String, ty: &syn::Type, attrs: &[Attribute]) -> FieldDef {
    let rust_type = type_to_rust_type(ty);
    let mut field = FieldDef::new(&name, rust_type);
    field.column_name = to_snake_case(&name);
    field.doc = get_doc_comment(attrs);
    field
}

// ---------------------------------------------------------------------------
// Enum parsing
// ---------------------------------------------------------------------------

fn parse_enum(item: &syn::ItemEnum) -> Option<EnumDef> {
    let enum_name = item.ident.to_string();
    let mut enum_def = EnumDef::new(&enum_name);
    enum_def.doc = get_doc_comment(&item.attrs);

    for variant in &item.variants {
        let variant_name = variant.ident.to_string();
        let mut enum_variant = EnumVariant::new(&variant_name);
        enum_variant.doc = get_doc_comment(&variant.attrs);

        if let Some((_, Expr::Lit(lit))) = &variant.discriminant
            && let Lit::Int(int_lit) = &lit.lit
            && let Ok(value) = int_lit.base10_parse::<i32>()
        {
            enum_variant.int_value = Some(value);
        }

        enum_def.variants.push(enum_variant);
    }

    Some(enum_def)
}

// ---------------------------------------------------------------------------
// Function parsing
// ---------------------------------------------------------------------------

/// Parse a function with a forge decorator attribute.
fn parse_function(item: &syn::ItemFn) -> Option<FunctionDef> {
    let kind = get_function_kind(&item.attrs)?;
    let func_name = item.sig.ident.to_string();

    let return_type = match &item.sig.output {
        ReturnType::Default => RustType::Custom("()".to_string()),
        ReturnType::Type(_, ty) => extract_result_type(ty),
    };

    let mut func = FunctionDef::new(&func_name, kind, return_type);
    func.doc = get_doc_comment(&item.attrs);
    func.is_async = item.sig.asyncness.is_some();

    // Parse arguments, skipping the context parameter.
    let mut is_first = true;
    for arg in &item.sig.inputs {
        if let FnArg::Typed(pat_type) = arg {
            if is_first {
                is_first = false;
                if is_context_type(&pat_type.ty) {
                    continue;
                }
            }

            if let Pat::Ident(pat_ident) = &*pat_type.pat {
                let arg_name = pat_ident.ident.to_string();
                let arg_type = type_to_rust_type(&pat_type.ty);
                func.args.push(FunctionArg::new(arg_name, arg_type));
            }
        }
    }

    Some(func)
}

/// Known Forge context types. Only these are skipped as the first parameter.
/// User-defined types like `AppContext` or `MyContext` are not matched.
const KNOWN_CONTEXT_TYPES: &[&str] = &[
    "QueryContext",
    "MutationContext",
    "JobContext",
    "CronContext",
    "WorkflowContext",
    "DaemonContext",
    "WebhookContext",
    "McpToolContext",
];

/// Check if a type is a known Forge context type.
///
/// Walks through references (`&T`, `&mut T`) and uses the syn AST to read the
/// final path segment, instead of stringifying the whole token stream.
fn is_context_type(ty: &syn::Type) -> bool {
    let mut inner = ty;
    while let syn::Type::Reference(r) = inner {
        inner = &r.elem;
    }
    let syn::Type::Path(type_path) = inner else {
        return false;
    };
    let Some(last) = type_path.path.segments.last() else {
        return false;
    };
    KNOWN_CONTEXT_TYPES.contains(&last.ident.to_string().as_str())
}

fn get_function_kind(attrs: &[Attribute]) -> Option<FunctionKind> {
    for attr in attrs {
        let path = attr.path();
        let segments: Vec<_> = path.segments.iter().map(|s| s.ident.to_string()).collect();

        let kind_str = match segments.as_slice() {
            [forge, kind] if forge == "forge" => Some(kind.as_str()),
            [kind] => Some(kind.as_str()),
            _ => None,
        };

        if let Some(kind) = kind_str {
            match kind {
                "query" => return Some(FunctionKind::Query),
                "mutation" => return Some(FunctionKind::Mutation),
                "job" => return Some(FunctionKind::Job),
                "cron" => return Some(FunctionKind::Cron),
                "workflow" => return Some(FunctionKind::Workflow),
                _ => {}
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Type conversion
// ---------------------------------------------------------------------------

/// Extract the inner type from `Result<T, E>` using syn's AST.
fn extract_result_type(ty: &syn::Type) -> RustType {
    if let syn::Type::Path(type_path) = ty
        && let Some(seg) = type_path.path.segments.last()
        && seg.ident == "Result"
        && let syn::PathArguments::AngleBracketed(args) = &seg.arguments
        && let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first()
    {
        return type_to_rust_type(inner_ty);
    }

    type_to_rust_type(ty)
}

/// Convert a `syn::Type` to `RustType` via structural AST walk.
fn type_to_rust_type(ty: &syn::Type) -> RustType {
    match ty {
        syn::Type::Reference(r) => type_to_rust_type(&r.elem),
        syn::Type::Path(tp) => path_to_rust_type(tp),
        _ => RustType::Custom(quote::quote!(#ty).to_string()),
    }
}

/// Resolve a type path to `RustType` using the last path segment.
fn path_to_rust_type(tp: &syn::TypePath) -> RustType {
    let Some(last) = tp.path.segments.last() else {
        return RustType::Custom(quote::quote!(#tp).to_string());
    };
    let ident = last.ident.to_string();

    match ident.as_str() {
        "String" | "str" => RustType::String,
        "i32" => RustType::I32,
        "i64" => RustType::I64,
        "f32" => RustType::F32,
        "f64" => RustType::F64,
        "bool" => RustType::Bool,
        "Uuid" => RustType::Uuid,
        "DateTime" => RustType::Instant,
        "NaiveDate" => RustType::LocalDate,
        "NaiveTime" => RustType::LocalTime,
        "Value" => RustType::Json,
        "Option" => {
            let inner = first_generic_arg(last);
            RustType::Option(Box::new(inner))
        }
        "Vec" => {
            if is_vec_u8(last) {
                return RustType::Bytes;
            }
            let inner = first_generic_arg(last);
            RustType::Vec(Box::new(inner))
        }
        _ => RustType::Custom(ident),
    }
}

/// Check if a `Vec` segment has `u8` as its type argument.
fn is_vec_u8(seg: &syn::PathSegment) -> bool {
    if let syn::PathArguments::AngleBracketed(args) = &seg.arguments
        && let Some(syn::GenericArgument::Type(syn::Type::Path(tp))) = args.args.first()
        && let Some(s) = tp.path.segments.last()
    {
        return s.ident == "u8" && s.arguments.is_empty();
    }
    false
}

/// Extract the first generic type argument from a path segment.
fn first_generic_arg(seg: &syn::PathSegment) -> RustType {
    if let syn::PathArguments::AngleBracketed(args) = &seg.arguments
        && let Some(syn::GenericArgument::Type(inner_ty)) = args.args.first()
    {
        return type_to_rust_type(inner_ty);
    }
    RustType::Custom(seg.ident.to_string())
}

// ---------------------------------------------------------------------------
// Attribute value helpers
// ---------------------------------------------------------------------------

/// Get `#[table(name = "...")]` value from attributes.
fn get_table_name_from_attrs(attrs: &[Attribute]) -> Option<String> {
    for attr in attrs {
        if attr.path().is_ident("table")
            && let Meta::List(list) = &attr.meta
        {
            let tokens = list.tokens.to_string();
            if let Some(value) = extract_name_value(&tokens) {
                return Some(value);
            }
        }
    }
    None
}

/// Get string value from attribute like `#[attr = "value"]`.
fn get_attribute_string_value(attr: &Attribute) -> Option<String> {
    if let Meta::NameValue(nv) = &attr.meta
        && let Expr::Lit(lit) = &nv.value
        && let Lit::Str(s) = &lit.lit
    {
        return Some(s.value());
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

/// Extract name value from `name = "value"` format.
fn extract_name_value(s: &str) -> Option<String> {
    if let Some((_, value)) = s.split_once('=') {
        let value = value.trim();
        if let Some(stripped) = value.strip_prefix('"').and_then(|s| s.strip_suffix('"')) {
            return Some(stripped.to_string());
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Pluralization
// ---------------------------------------------------------------------------

fn pluralize(s: &str) -> String {
    forge_core::util::pluralize(s)
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::todo
)]
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
        parse_file(source, &registry).expect("model source should parse");

        let table = registry
            .get_table("users")
            .expect("users table should be registered");
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
        parse_file(source, &registry).expect("enum source should parse");

        let enum_def = registry
            .get_enum("ProjectStatus")
            .expect("ProjectStatus enum should be registered");
        assert_eq!(enum_def.variants.len(), 3);
    }

    #[test]
    fn test_to_snake_case() {
        assert_eq!(to_snake_case("UserProfile"), "user_profile");
        assert_eq!(to_snake_case("ID"), "id");
        assert_eq!(to_snake_case("createdAt"), "created_at");
    }

    #[test]
    fn test_pluralize() {
        assert_eq!(pluralize("user"), "users");
        assert_eq!(pluralize("category"), "categories");
        assert_eq!(pluralize("box"), "boxes");
        assert_eq!(pluralize("address"), "addresses");
    }

    #[test]
    fn test_parse_query_function() {
        let source = r#"
            #[query]
            async fn get_user(ctx: QueryContext, id: Uuid) -> Result<User> {
                todo!()
            }
        "#;

        let registry = SchemaRegistry::new();
        parse_file(source, &registry).expect("query function should parse");

        let func = registry
            .get_function("get_user")
            .expect("get_user function should be registered");
        assert_eq!(func.name, "get_user");
        assert_eq!(func.kind, FunctionKind::Query);
        assert!(func.is_async);
    }

    #[test]
    fn test_parse_mutation_function() {
        let source = r#"
            #[mutation]
            async fn create_user(ctx: MutationContext, name: String, email: String) -> Result<User> {
                todo!()
            }
        "#;

        let registry = SchemaRegistry::new();
        parse_file(source, &registry).expect("mutation function should parse");

        let func = registry
            .get_function("create_user")
            .expect("create_user function should be registered");
        assert_eq!(func.name, "create_user");
        assert_eq!(func.kind, FunctionKind::Mutation);
        assert_eq!(func.args.len(), 2);
        assert_eq!(
            func.args.first().expect("name arg should exist").name,
            "name"
        );
        assert_eq!(
            func.args.get(1).expect("email arg should exist").name,
            "email"
        );
    }

    #[test]
    fn test_context_detection_structural() {
        // A type ending with "Context" should be detected.
        let source = r#"
            #[query]
            async fn test(ctx: forge::QueryContext, id: Uuid) -> Result<User> {
                todo!()
            }
        "#;
        let registry = SchemaRegistry::new();
        parse_file(source, &registry).expect("context query should parse");
        let func = registry
            .get_function("test")
            .expect("test function should be registered");
        assert_eq!(func.args.len(), 1); // Only `id`, context was skipped.
        assert_eq!(func.args.first().expect("id arg should exist").name, "id");
    }

    #[test]
    fn test_context_detection_does_not_match_other_types() {
        // A type NOT ending with "Context" should not be skipped.
        let source = r#"
            #[query]
            async fn test(data: ContextManager, id: Uuid) -> Result<User> {
                todo!()
            }
        "#;
        let registry = SchemaRegistry::new();
        parse_file(source, &registry).expect("non-context query should parse");
        let func = registry
            .get_function("test")
            .expect("test function should be registered");
        // "ContextManager" ends with "Manager", not "Context", so both args kept.
        assert_eq!(func.args.len(), 2);
    }

    #[test]
    fn test_user_defined_context_not_skipped() {
        // User-defined "AppContext" is NOT a known Forge context type.
        let source = r#"
            #[query]
            async fn test(ctx: AppContext, id: Uuid) -> Result<User> {
                todo!()
            }
        "#;
        let registry = SchemaRegistry::new();
        parse_file(source, &registry).expect("user context should parse");
        let func = registry
            .get_function("test")
            .expect("test function should be registered");
        assert_eq!(func.args.len(), 2, "AppContext should not be skipped");
    }

    #[test]
    fn test_nested_result_type_extraction() {
        let source = r#"
            #[query]
            async fn nested(ctx: QueryContext) -> Result<Vec<Option<User>>> {
                todo!()
            }
        "#;
        let registry = SchemaRegistry::new();
        parse_file(source, &registry).expect("nested result should parse");
        let func = registry
            .get_function("nested")
            .expect("nested function should be registered");
        match &func.return_type {
            RustType::Vec(inner) => match inner.as_ref() {
                RustType::Option(inner2) => match inner2.as_ref() {
                    RustType::Custom(name) => assert_eq!(name, "User"),
                    other => panic!("Expected Custom(User), got: {other:?}"),
                },
                other => panic!("Expected Option, got: {other:?}"),
            },
            other => panic!("Expected Vec, got: {other:?}"),
        }
    }

    #[test]
    fn test_naive_time_maps_to_local_time() {
        let source = r#"
            #[derive(Serialize, Deserialize)]
            struct Schedule {
                start_time: NaiveTime,
            }
        "#;
        let registry = SchemaRegistry::new();
        parse_file(source, &registry).expect("schedule DTO should parse");
        let table = registry
            .get_table("Schedule")
            .expect("Schedule table should be registered");
        assert_eq!(
            table
                .fields
                .first()
                .expect("start_time field should exist")
                .rust_type,
            RustType::LocalTime
        );
    }

    // --- End-to-end pipeline tests ---

    /// Simulates a realistic Forge project with models, enums, queries, mutations,
    /// jobs, and workflows, then verifies the full parse pipeline produces
    /// consistent and complete schema.
    #[test]
    fn end_to_end_realistic_schema_pipeline() {
        let source = r#"
            use forge::prelude::*;

            #[model]
            struct User {
                id: Uuid,
                email: String,
                name: Option<String>,
                role: UserRole,
                created_at: DateTime<Utc>,
            }

            #[model]
            struct Post {
                id: Uuid,
                title: String,
                body: String,
                author_id: Uuid,
                published: bool,
                view_count: i64,
                created_at: DateTime<Utc>,
            }

            #[forge_enum]
            enum UserRole {
                Admin,
                Member,
                Guest,
            }

            #[derive(Serialize, Deserialize)]
            struct CreateUserArgs {
                email: String,
                name: Option<String>,
                role: UserRole,
            }

            #[query]
            async fn get_users(ctx: QueryContext) -> Result<Vec<User>> {
                todo!()
            }

            #[query]
            async fn get_user(ctx: QueryContext, id: Uuid) -> Result<User> {
                todo!()
            }

            #[mutation]
            async fn create_user(ctx: MutationContext, args: CreateUserArgs) -> Result<User> {
                todo!()
            }

            #[mutation]
            async fn delete_user(ctx: MutationContext, id: Uuid) -> Result<()> {
                todo!()
            }

            #[job]
            async fn send_welcome_email(ctx: JobContext, user_id: Uuid) -> Result<()> {
                todo!()
            }

            #[workflow]
            async fn onboarding(ctx: WorkflowContext, user_id: Uuid) -> Result<String> {
                todo!()
            }

            #[cron]
            async fn daily_cleanup(ctx: CronContext) -> Result<()> {
                todo!()
            }
        "#;

        let registry = SchemaRegistry::new();
        parse_file(source, &registry).expect("realistic project should parse");

        // Models
        let users = registry.get_table("users").expect("users table");
        assert_eq!(users.fields.len(), 5);
        let posts = registry.get_table("posts").expect("posts table");
        assert_eq!(posts.fields.len(), 7);

        // Enum
        let role_enum = registry.get_enum("UserRole").expect("UserRole enum");
        assert_eq!(role_enum.variants.len(), 3);

        // DTO
        let args = registry
            .get_table("CreateUserArgs")
            .expect("CreateUserArgs DTO");
        assert_eq!(args.fields.len(), 3);

        // Functions
        let all_fns = registry.all_functions();
        assert_eq!(all_fns.len(), 7);

        let queries: Vec<_> = all_fns
            .iter()
            .filter(|f| f.kind == FunctionKind::Query)
            .collect();
        assert_eq!(queries.len(), 2);

        let mutations: Vec<_> = all_fns
            .iter()
            .filter(|f| f.kind == FunctionKind::Mutation)
            .collect();
        assert_eq!(mutations.len(), 2);

        let jobs: Vec<_> = all_fns
            .iter()
            .filter(|f| f.kind == FunctionKind::Job)
            .collect();
        assert_eq!(jobs.len(), 1);

        let workflows: Vec<_> = all_fns
            .iter()
            .filter(|f| f.kind == FunctionKind::Workflow)
            .collect();
        assert_eq!(workflows.len(), 1);

        let crons: Vec<_> = all_fns
            .iter()
            .filter(|f| f.kind == FunctionKind::Cron)
            .collect();
        assert_eq!(crons.len(), 1);

        // Verify function details
        let get_users = registry.get_function("get_users").expect("get_users");
        assert!(
            get_users.args.is_empty(),
            "get_users has no user args (context stripped)"
        );

        let create_user = registry.get_function("create_user").expect("create_user");
        assert_eq!(create_user.args.len(), 1, "create_user has one user arg");
        assert_eq!(create_user.args.first().expect("arg").name, "args");

        let send_email = registry
            .get_function("send_welcome_email")
            .expect("send_welcome_email");
        assert_eq!(send_email.kind, FunctionKind::Job);
        assert_eq!(send_email.args.len(), 1);
    }

    /// Verify that BindingSet correctly groups and filters functions.
    #[test]
    fn binding_set_from_mixed_schema() {
        use crate::binding::BindingSet;

        let source = r#"
            #[query]
            async fn list_items(ctx: QueryContext) -> Result<Vec<Item>> { todo!() }

            #[mutation]
            async fn add_item(ctx: MutationContext, name: String) -> Result<Item> { todo!() }

            #[job]
            async fn process_item(ctx: JobContext, id: Uuid) -> Result<()> { todo!() }

            #[cron]
            async fn cleanup(ctx: CronContext) -> Result<()> { todo!() }

            #[workflow]
            async fn item_pipeline(ctx: WorkflowContext, id: Uuid) -> Result<String> { todo!() }
        "#;

        let registry = SchemaRegistry::new();
        parse_file(source, &registry).expect("parse");

        let bindings = BindingSet::from_registry(&registry);
        assert_eq!(bindings.queries.len(), 1);
        assert_eq!(bindings.mutations.len(), 1);
        assert_eq!(bindings.jobs.len(), 1);
        assert_eq!(bindings.workflows.len(), 1);
        // Crons must NOT appear in client bindings
    }

    #[test]
    fn parse_function_with_multiple_args() {
        let source = r#"
            #[mutation]
            async fn update_user(ctx: MutationContext, id: Uuid, name: String, email: Option<String>) -> Result<User> {
                todo!()
            }
        "#;

        let registry = SchemaRegistry::new();
        parse_file(source, &registry).expect("parse");

        let func = registry.get_function("update_user").expect("update_user");
        assert_eq!(func.args.len(), 3);
        assert_eq!(func.args.first().expect("id").name, "id");
        assert_eq!(func.args.get(1).expect("name").name, "name");
        assert_eq!(func.args.get(2).expect("email").name, "email");
    }

    #[test]
    fn parse_function_with_vec_return() {
        let source = r#"
            #[query]
            async fn list_posts(ctx: QueryContext) -> Result<Vec<Post>> {
                todo!()
            }
        "#;

        let registry = SchemaRegistry::new();
        parse_file(source, &registry).expect("parse");

        let func = registry.get_function("list_posts").expect("list_posts");
        match &func.return_type {
            RustType::Vec(inner) => match inner.as_ref() {
                RustType::Custom(name) => assert_eq!(name, "Post"),
                other => panic!("Expected Custom(Post), got: {other:?}"),
            },
            other => panic!("Expected Vec, got: {other:?}"),
        }
    }

    fn parse_type(s: &str) -> RustType {
        let ty: syn::Type = syn::parse_str(s).expect("valid type");
        type_to_rust_type(&ty)
    }

    #[test]
    fn type_to_rust_type_primitives() {
        assert_eq!(parse_type("String"), RustType::String);
        assert_eq!(parse_type("&str"), RustType::String);
        assert_eq!(parse_type("i32"), RustType::I32);
        assert_eq!(parse_type("i64"), RustType::I64);
        assert_eq!(parse_type("f32"), RustType::F32);
        assert_eq!(parse_type("f64"), RustType::F64);
        assert_eq!(parse_type("bool"), RustType::Bool);
    }

    #[test]
    fn type_to_rust_type_qualified_paths() {
        assert_eq!(parse_type("Uuid"), RustType::Uuid);
        assert_eq!(parse_type("uuid::Uuid"), RustType::Uuid);
        assert_eq!(parse_type("DateTime<Utc>"), RustType::Instant);
        assert_eq!(parse_type("chrono::DateTime<Utc>"), RustType::Instant);
        assert_eq!(
            parse_type("chrono::DateTime<chrono::Utc>"),
            RustType::Instant
        );
        assert_eq!(parse_type("NaiveDate"), RustType::LocalDate);
        assert_eq!(parse_type("chrono::NaiveDate"), RustType::LocalDate);
        assert_eq!(parse_type("NaiveTime"), RustType::LocalTime);
        assert_eq!(parse_type("chrono::NaiveTime"), RustType::LocalTime);
        assert_eq!(parse_type("serde_json::Value"), RustType::Json);
        assert_eq!(parse_type("Value"), RustType::Json);
    }

    #[test]
    fn type_to_rust_type_containers() {
        assert_eq!(parse_type("Vec<u8>"), RustType::Bytes);
        assert_eq!(
            parse_type("Vec<String>"),
            RustType::Vec(Box::new(RustType::String))
        );
        assert_eq!(
            parse_type("Option<i32>"),
            RustType::Option(Box::new(RustType::I32))
        );
        assert_eq!(
            parse_type("Option<Vec<String>>"),
            RustType::Option(Box::new(RustType::Vec(Box::new(RustType::String))))
        );
    }

    #[test]
    fn type_to_rust_type_std_qualified_vec() {
        assert_eq!(
            parse_type("std::vec::Vec<i32>"),
            RustType::Vec(Box::new(RustType::I32))
        );
        assert_eq!(
            parse_type("std::option::Option<String>"),
            RustType::Option(Box::new(RustType::String))
        );
    }

    #[test]
    fn type_to_rust_type_custom() {
        assert_eq!(
            parse_type("MyStruct"),
            RustType::Custom("MyStruct".to_string())
        );
    }
}

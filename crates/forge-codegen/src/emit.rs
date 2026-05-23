//! Single source of truth for type mapping and code generation utilities.
//!
//! Every type conversion — RustType to TypeScript, RustType to Dioxus Rust —
//! lives here. Generators call these functions instead of implementing their own.
//! This makes it impossible for type mappings to diverge between generators.

use forge_core::schema::RustType;

/// Position context for type mapping. Some types map differently by position.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Position {
    /// Function argument or struct field.
    Arg,
    /// Function return type.
    Return,
}

/// Convert a RustType to its TypeScript representation.
pub fn ts_type(rust_type: &RustType, pos: Position) -> String {
    match rust_type {
        RustType::String
        | RustType::Uuid
        | RustType::Instant
        | RustType::LocalDate
        | RustType::LocalTime => "string".into(),

        // NOTE: `i64` exceeds JavaScript's safe-integer range (2^53). Emit as
        // `number` to match the JSON wire format produced by serde, and warn
        // users to pick `String` (or apply `serde_with::DisplayFromStr`) for
        // values that genuinely need full 64-bit precision (large IDs, big
        // counters). Switching to `string` everywhere would silently corrupt
        // every existing handler signature, so we leave the choice with the
        // application author and document the trap in `frontend.md`.
        RustType::I32 | RustType::I64 | RustType::F32 | RustType::F64 => "number".into(),

        RustType::Bool => "boolean".into(),
        RustType::Json => "unknown".into(),
        RustType::Upload => "File | Blob".into(),

        RustType::Bytes => match pos {
            Position::Return => "Blob".into(),
            Position::Arg => "Uint8Array".into(),
        },

        RustType::Option(inner) => format!("{} | null", ts_type(inner, pos)),
        // `Option<T>` inside an array renders as `Array<T | null>` not `(T | null)[]`
        // to keep the union bound to the element type, not the whole array.
        RustType::Vec(inner) => match inner.as_ref() {
            RustType::Option(_) => format!("Array<{}>", ts_type(inner, pos)),
            _ => format!("{}[]", ts_type(inner, pos)),
        },

        RustType::Custom(name) => ts_custom(name, pos),
    }
}

fn ts_custom(name: &str, pos: Position) -> String {
    match name {
        "()" => "void".into(),
        "Upload" => "File | Blob".into(),
        "Bytes" => match pos {
            Position::Return => "Blob".into(),
            Position::Arg => "Uint8Array".into(),
        },

        "Uuid" | "uuid::Uuid" | "DateTime<Utc>" | "NaiveDate" | "NaiveDateTime" | "Instant"
        | "LocalDate" | "LocalTime" | "Timestamp" => "string".into(),

        "i32" | "i64" | "u32" | "u64" | "f32" | "f64" | "usize" | "isize" => "number".into(),

        "bool" => "boolean".into(),

        "Value" | "serde_json::Value" => "unknown".into(),

        _ if name.starts_with("Vec<") => {
            let inner = name
                .strip_prefix("Vec<")
                .and_then(|s| s.strip_suffix('>'))
                .unwrap_or("unknown");
            format!("{}[]", ts_type(&RustType::Custom(inner.to_string()), pos))
        }

        _ if name.starts_with("HashMap<") || name.starts_with("std::collections::HashMap<") => {
            ts_hashmap(name, pos)
        }

        "Cursor" => "string".into(),
        "PageInfo" => "PageInfo".into(),
        _ if name.starts_with("Page<") => {
            let inner = name
                .strip_prefix("Page<")
                .and_then(|s| s.strip_suffix('>'))
                .unwrap_or("unknown");
            format!(
                "Page<{}>",
                ts_type(&RustType::Custom(inner.to_string()), pos)
            )
        }

        _ => name.to_string(),
    }
}

fn ts_hashmap(name: &str, pos: Position) -> String {
    let inner = name
        .strip_prefix("HashMap<")
        .or_else(|| name.strip_prefix("std::collections::HashMap<"))
        .and_then(|s| s.strip_suffix('>'));

    let Some(inner) = inner else {
        return "Record<string, unknown>".into();
    };

    let Some((_key, value)) = split_top_level_comma(inner) else {
        return "Record<string, unknown>".into();
    };

    let value_type = ts_type(&RustType::Custom(value.to_string()), pos);
    format!("Record<string, {}>", value_type)
}

/// Split a generic parameter list at the first top-level comma,
/// respecting nested `<>`, `()`, and `[]` brackets.
fn split_top_level_comma(s: &str) -> Option<(&str, &str)> {
    let mut depth = 0usize;
    for (i, c) in s.char_indices() {
        match c {
            '<' | '(' | '[' => depth += 1,
            '>' | ')' | ']' => depth = depth.saturating_sub(1),
            ',' if depth == 0 => {
                return Some((s.get(..i)?.trim(), s.get(i + 1..)?.trim()));
            }
            _ => {}
        }
    }
    None
}

/// Convert a RustType to its Dioxus/Rust representation.
pub fn dioxus_type(rust_type: &RustType) -> String {
    match rust_type {
        RustType::String | RustType::Uuid => "String".into(),
        RustType::I32 => "i32".into(),
        RustType::I64 => "i64".into(),
        RustType::F32 => "f32".into(),
        RustType::F64 => "f64".into(),
        RustType::Bool => "bool".into(),
        RustType::Instant | RustType::LocalDate | RustType::LocalTime => "String".into(),
        RustType::Upload => "ForgeUpload".into(),
        RustType::Json => "JsonValue".into(),
        RustType::Bytes => "Vec<u8>".into(),
        RustType::Option(inner) => format!("Option<{}>", dioxus_type(inner)),
        RustType::Vec(inner) => format!("Vec<{}>", dioxus_type(inner)),
        RustType::Custom(name) => dioxus_custom(name),
    }
}

fn dioxus_custom(name: &str) -> String {
    match name {
        "Uuid" | "uuid::Uuid" => "String".into(),
        "DateTime<Utc>" | "NaiveDate" | "NaiveDateTime" | "Instant" | "LocalDate" | "LocalTime"
        | "Timestamp" => "String".into(),
        // Preserve narrow integer widths instead of silently widening to i64.
        // Mirrors what the handler actually returns on the wire.
        "i32" => "i32".into(),
        "u32" => "u32".into(),
        "usize" => "usize".into(),
        "isize" => "isize".into(),
        "i64" | "u64" => "i64".into(),
        "f32" => "f32".into(),
        "f64" => "f64".into(),
        "bool" => "bool".into(),
        "Value" | "serde_json::Value" => "JsonValue".into(),
        "Bytes" => "Vec<u8>".into(),
        "Upload" => "ForgeUpload".into(),
        "Cursor" => "String".into(),
        "PageInfo" => "forge_core::PageInfo".into(),

        _ if name.starts_with("Vec<") => {
            let inner = name
                .strip_prefix("Vec<")
                .and_then(|s| s.strip_suffix('>'))
                .unwrap_or("JsonValue");
            format!("Vec<{}>", dioxus_type(&RustType::Custom(inner.to_string())))
        }

        _ if name.starts_with("HashMap<") || name.starts_with("std::collections::HashMap<") => {
            dioxus_hashmap(name)
        }

        _ if name.starts_with("Page<") => {
            let inner = name
                .strip_prefix("Page<")
                .and_then(|s| s.strip_suffix('>'))
                .unwrap_or("JsonValue");
            format!(
                "forge_core::Page<{}>",
                dioxus_type(&RustType::Custom(inner.to_string()))
            )
        }
        other => other.to_string(),
    }
}

fn dioxus_hashmap(name: &str) -> String {
    let inner = name
        .strip_prefix("HashMap<")
        .or_else(|| name.strip_prefix("std::collections::HashMap<"))
        .and_then(|s| s.strip_suffix('>'));

    let Some(inner) = inner else {
        return "std::collections::HashMap<String, JsonValue>".into();
    };

    let Some((_key, value)) = split_top_level_comma(inner) else {
        return "std::collections::HashMap<String, JsonValue>".into();
    };

    let value_type = dioxus_type(&RustType::Custom(value.to_string()));
    format!("std::collections::HashMap<String, {}>", value_type)
}

fn walk_type(rust_type: &RustType, predicate: &dyn Fn(&RustType) -> bool) -> bool {
    if predicate(rust_type) {
        return true;
    }
    match rust_type {
        RustType::Option(inner) | RustType::Vec(inner) => walk_type(inner, predicate),
        _ => false,
    }
}

pub fn contains_upload(rust_type: &RustType) -> bool {
    walk_type(rust_type, &|t| {
        matches!(t, RustType::Upload) || matches!(t, RustType::Custom(n) if n == "Upload")
    })
}

pub fn contains_json(rust_type: &RustType) -> bool {
    walk_type(rust_type, &|t| {
        matches!(t, RustType::Json)
            || matches!(t, RustType::Custom(n) if n == "Value" || n == "serde_json::Value")
    })
}

/// Collect custom type names that need importing in TypeScript.
pub fn collect_type_imports(rust_type: &RustType, imports: &mut Vec<String>) {
    match rust_type {
        RustType::Custom(name) if is_importable_type(name) => {
            imports.push(name.clone());
        }
        RustType::Option(inner) | RustType::Vec(inner) => collect_type_imports(inner, imports),
        _ => {}
    }
}

fn is_importable_type(name: &str) -> bool {
    !matches!(
        name,
        "()" | "Upload" | "Bytes" | "Instant" | "LocalDate" | "LocalTime" | "Cursor" | "PageInfo"
    ) && !name.starts_with("Vec<")
        && !name.starts_with("HashMap<")
        && !name.starts_with("std::collections::")
        && !name.starts_with("Page<")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ts_primitives() {
        assert_eq!(ts_type(&RustType::String, Position::Arg), "string");
        assert_eq!(ts_type(&RustType::I32, Position::Arg), "number");
        assert_eq!(ts_type(&RustType::Bool, Position::Arg), "boolean");
        assert_eq!(ts_type(&RustType::Uuid, Position::Arg), "string");
        assert_eq!(ts_type(&RustType::Json, Position::Arg), "unknown");
    }

    #[test]
    fn ts_temporal_types() {
        for ty in [RustType::Instant, RustType::LocalDate, RustType::LocalTime] {
            assert_eq!(ts_type(&ty, Position::Arg), "string");
        }
    }

    #[test]
    fn ts_bytes_depends_on_position() {
        assert_eq!(ts_type(&RustType::Bytes, Position::Arg), "Uint8Array");
        assert_eq!(ts_type(&RustType::Bytes, Position::Return), "Blob");
    }

    #[test]
    fn ts_option_and_vec() {
        assert_eq!(
            ts_type(&RustType::Option(Box::new(RustType::String)), Position::Arg),
            "string | null"
        );
        assert_eq!(
            ts_type(&RustType::Vec(Box::new(RustType::I32)), Position::Arg),
            "number[]"
        );
    }

    #[test]
    fn ts_custom_types() {
        assert_eq!(
            ts_type(&RustType::Custom("()".into()), Position::Arg),
            "void"
        );
        assert_eq!(
            ts_type(&RustType::Custom("User".into()), Position::Arg),
            "User"
        );
        assert_eq!(
            ts_type(&RustType::Custom("Upload".into()), Position::Arg),
            "File | Blob"
        );
    }

    #[test]
    fn ts_hashmap() {
        assert_eq!(
            ts_type(
                &RustType::Custom("HashMap<String, i32>".into()),
                Position::Arg
            ),
            "Record<string, number>"
        );
        assert_eq!(
            ts_type(
                &RustType::Custom("HashMap<String, User>".into()),
                Position::Arg
            ),
            "Record<string, User>"
        );
    }

    #[test]
    fn ts_hashmap_nested_generics() {
        assert_eq!(
            ts_type(
                &RustType::Custom("HashMap<String, HashMap<String, i32>>".into()),
                Position::Arg
            ),
            "Record<string, Record<string, number>>"
        );
        assert_eq!(
            ts_type(
                &RustType::Custom("HashMap<(K1, K2), V>".into()),
                Position::Arg
            ),
            "Record<string, V>"
        );
    }

    #[test]
    fn dioxus_primitives() {
        assert_eq!(dioxus_type(&RustType::String), "String");
        assert_eq!(dioxus_type(&RustType::I32), "i32");
        assert_eq!(dioxus_type(&RustType::Bool), "bool");
        assert_eq!(dioxus_type(&RustType::Upload), "ForgeUpload");
        assert_eq!(dioxus_type(&RustType::Json), "JsonValue");
        assert_eq!(dioxus_type(&RustType::Bytes), "Vec<u8>");
    }

    #[test]
    fn dioxus_custom_known_aliases() {
        assert_eq!(dioxus_type(&RustType::Custom("Timestamp".into())), "String");
        assert_eq!(dioxus_type(&RustType::Custom("Value".into())), "JsonValue");
        assert_eq!(
            dioxus_type(&RustType::Custom("Upload".into())),
            "ForgeUpload"
        );
    }

    #[test]
    fn dioxus_hashmap() {
        // Narrow integers are preserved; the HashMap value follows the same path.
        assert_eq!(
            dioxus_type(&RustType::Custom("HashMap<String, i32>".into())),
            "std::collections::HashMap<String, i32>"
        );
        assert_eq!(
            dioxus_type(&RustType::Custom("HashMap<String, User>".into())),
            "std::collections::HashMap<String, User>"
        );
        assert_eq!(
            dioxus_type(&RustType::Custom(
                "std::collections::HashMap<String, bool>".into()
            )),
            "std::collections::HashMap<String, bool>"
        );
        // Value type is mapped recursively so wrapper aliases like Uuid map to String.
        assert_eq!(
            dioxus_type(&RustType::Custom("HashMap<String, Uuid>".into())),
            "std::collections::HashMap<String, String>"
        );
    }

    #[test]
    fn upload_detection() {
        assert!(contains_upload(&RustType::Upload));
        assert!(contains_upload(&RustType::Option(Box::new(
            RustType::Upload
        ))));
        assert!(contains_upload(&RustType::Vec(Box::new(RustType::Upload))));
        assert!(contains_upload(&RustType::Custom("Upload".into())));
        assert!(!contains_upload(&RustType::String));
        assert!(!contains_upload(&RustType::Custom("User".into())));
    }

    #[test]
    fn json_detection() {
        assert!(contains_json(&RustType::Json));
        assert!(contains_json(&RustType::Option(Box::new(RustType::Json))));
        assert!(contains_json(&RustType::Custom("Value".into())));
        assert!(!contains_json(&RustType::String));
    }

    #[test]
    fn type_imports() {
        let mut imports = Vec::new();
        collect_type_imports(&RustType::Custom("User".into()), &mut imports);
        collect_type_imports(&RustType::Custom("()".into()), &mut imports);
        collect_type_imports(&RustType::Custom("Upload".into()), &mut imports);
        collect_type_imports(
            &RustType::Vec(Box::new(RustType::Custom("Project".into()))),
            &mut imports,
        );
        assert_eq!(imports, vec!["User", "Project"]);
    }

    #[test]
    fn pagination_types_not_importable() {
        let mut imports = Vec::new();
        collect_type_imports(&RustType::Custom("Cursor".into()), &mut imports);
        collect_type_imports(&RustType::Custom("PageInfo".into()), &mut imports);
        collect_type_imports(&RustType::Custom("Page<User>".into()), &mut imports);
        assert!(imports.is_empty());
    }

    #[test]
    fn pagination_type_mapping() {
        assert_eq!(
            ts_type(&RustType::Custom("Cursor".into()), Position::Arg),
            "string"
        );
        assert_eq!(
            ts_type(&RustType::Custom("PageInfo".into()), Position::Arg),
            "PageInfo"
        );
        assert_eq!(
            ts_type(&RustType::Custom("Page<User>".into()), Position::Arg),
            "Page<User>"
        );
        assert_eq!(dioxus_type(&RustType::Custom("Cursor".into())), "String");
        assert_eq!(
            dioxus_type(&RustType::Custom("PageInfo".into())),
            "forge_core::PageInfo"
        );
        assert_eq!(
            dioxus_type(&RustType::Custom("Page<User>".into())),
            "forge_core::Page<User>"
        );
    }

    /// Adding a new RustType variant without updating both emitters fails this test.
    /// The exhaustive `match` in the emitters themselves also produces a compile error.
    #[test]
    fn all_types_map_to_nonempty_in_both_targets() {
        let mut types = RustType::leaf_variants();
        types.push(RustType::Option(Box::new(RustType::String)));
        types.push(RustType::Vec(Box::new(RustType::I32)));
        types.push(RustType::Custom("User".into()));
        types.push(RustType::Custom("()".into()));

        for ty in &types {
            let ts = ts_type(ty, Position::Arg);
            assert!(!ts.is_empty(), "ts_type returned empty for {ty:?}");

            let ts_ret = ts_type(ty, Position::Return);
            assert!(
                !ts_ret.is_empty(),
                "ts_type(Return) returned empty for {ty:?}"
            );

            let dx = dioxus_type(ty);
            assert!(!dx.is_empty(), "dioxus_type returned empty for {ty:?}");
        }

        assert!(
            RustType::leaf_variants().len() >= 13,
            "leaf_variants() must cover all non-recursive RustType variants"
        );
    }

    #[test]
    fn nested_option_vec_maps_correctly() {
        let ty = RustType::Option(Box::new(RustType::Vec(Box::new(RustType::String))));
        assert_eq!(ts_type(&ty, Position::Arg), "string[] | null");
        assert_eq!(dioxus_type(&ty), "Option<Vec<String>>");

        // Vec<Option<i32>> uses Array<...> to avoid the `T | (null[])` precedence trap.
        let ty2 = RustType::Vec(Box::new(RustType::Option(Box::new(RustType::I32))));
        assert_eq!(ts_type(&ty2, Position::Arg), "Array<number | null>");
        assert_eq!(dioxus_type(&ty2), "Vec<Option<i32>>");
    }

    #[test]
    fn position_sensitive_types_differ() {
        assert_ne!(
            ts_type(&RustType::Bytes, Position::Arg),
            ts_type(&RustType::Bytes, Position::Return)
        );
    }
}

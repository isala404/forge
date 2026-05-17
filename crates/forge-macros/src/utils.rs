//! Shared utility functions for forge macros.

use std::time::Duration;

use proc_macro2::TokenStream;
use quote::quote;

/// Convert a snake_case string to PascalCase.
pub fn to_pascal_case(s: &str) -> String {
    s.split('_')
        .map(|part| {
            let mut chars = part.chars();
            match chars.next() {
                None => String::new(),
                Some(first) => first.to_uppercase().chain(chars).collect(),
            }
        })
        .collect()
}

/// Parse a duration string (e.g., "30s", "5m", "1h") into a `Duration`.
/// Bare integers without a unit suffix are rejected.
fn parse_duration(s: &str) -> Option<Duration> {
    let s = s.trim();
    if let Some(num) = s.strip_suffix("ms") {
        num.parse::<u64>().ok().map(Duration::from_millis)
    } else if let Some(num) = s.strip_suffix('s') {
        num.parse::<u64>().ok().map(Duration::from_secs)
    } else if let Some(num) = s.strip_suffix('m') {
        num.parse::<u64>().ok().map(|m| Duration::from_secs(m * 60))
    } else if let Some(num) = s.strip_suffix('h') {
        num.parse::<u64>()
            .ok()
            .map(|h| Duration::from_secs(h * 3600))
    } else if let Some(num) = s.strip_suffix('d') {
        num.parse::<u64>()
            .ok()
            .map(|d| Duration::from_secs(d * 86400))
    } else {
        // Bare integers without a unit suffix are not accepted. Require explicit
        // suffixes (e.g. "30s") so intent is unambiguous at the macro callsite.
        None
    }
}

/// Parse a duration string into seconds.
/// Returns None if the string cannot be parsed or has no unit suffix.
pub fn parse_duration_secs(s: &str) -> Option<u64> {
    parse_duration(s).map(|d| d.as_secs())
}

/// Parse a duration string into a TokenStream representing std::time::Duration.
/// Emits a `compile_error!` if the string has no recognized unit suffix or if
/// the numeric portion can't be parsed (e.g. `30sec` is rejected, not silently
/// coerced to the default).
pub fn parse_duration_tokens(s: &str, default_secs: u64) -> TokenStream {
    let s = s.trim();
    let invalid = || {
        let msg = format!(
            "invalid duration \"{}\": use a suffix like \"30s\", \"5m\", or \"1h\"",
            s
        );
        quote! { compile_error!(#msg) }
    };

    if let Some(num) = s.strip_suffix("ms") {
        match num.parse::<u64>() {
            Ok(n) => quote! { std::time::Duration::from_millis(#n) },
            Err(_) => invalid(),
        }
    } else if let Some(num) = s.strip_suffix('s') {
        match num.parse::<u64>() {
            Ok(n) => quote! { std::time::Duration::from_secs(#n) },
            Err(_) => invalid(),
        }
    } else if let Some(num) = s.strip_suffix('m') {
        match num.parse::<u64>() {
            Ok(n) => {
                let secs = n * 60;
                quote! { std::time::Duration::from_secs(#secs) }
            }
            Err(_) => invalid(),
        }
    } else if let Some(num) = s.strip_suffix('h') {
        match num.parse::<u64>() {
            Ok(n) => {
                let secs = n * 3600;
                quote! { std::time::Duration::from_secs(#secs) }
            }
            Err(_) => invalid(),
        }
    } else if let Some(num) = s.strip_suffix('d') {
        match num.parse::<u64>() {
            Ok(n) => {
                let secs = n * 86400;
                quote! { std::time::Duration::from_secs(#secs) }
            }
            Err(_) => invalid(),
        }
    } else {
        let _ = default_secs;
        invalid()
    }
}

/// Parse a human-readable size string into bytes.
/// Returns None if the string cannot be parsed.
pub fn parse_size_bytes(s: &str) -> Option<usize> {
    let s = s.trim().to_lowercase();
    if let Some(num) = s.strip_suffix("gb") {
        num.trim()
            .parse::<usize>()
            .ok()
            .map(|n| n * 1024 * 1024 * 1024)
    } else if let Some(num) = s.strip_suffix("mb") {
        num.trim().parse::<usize>().ok().map(|n| n * 1024 * 1024)
    } else if let Some(num) = s.strip_suffix("kb") {
        num.trim().parse::<usize>().ok().map(|n| n * 1024)
    } else if let Some(num) = s.strip_suffix('b') {
        num.trim().parse::<usize>().ok()
    } else {
        s.parse::<usize>().ok()
    }
}

/// Returns true when the type's leaf path segment is a Rust primitive scalar,
/// `String`, `&str`, or a standard collection wrapper that should not be
/// passed through as a single args struct.
///
/// Used by query/mutation/mcp_tool macros to decide whether a single non-context
/// argument should be passed through directly (custom struct) or wrapped into a
/// generated args struct (primitive or collection of primitives).
pub fn is_primitive_arg_type(ty: &syn::Type) -> bool {
    use syn::Type;

    // References to primitives like &str count as primitive.
    if let Type::Reference(r) = ty {
        return is_primitive_arg_type(&r.elem);
    }

    let Type::Path(type_path) = ty else {
        return false;
    };

    let Some(segment) = type_path.path.segments.last() else {
        return false;
    };

    let name = segment.ident.to_string();
    matches!(
        name.as_str(),
        "i8" | "i16"
            | "i32"
            | "i64"
            | "i128"
            | "isize"
            | "u8"
            | "u16"
            | "u32"
            | "u64"
            | "u128"
            | "usize"
            | "f32"
            | "f64"
            | "bool"
            | "char"
            | "String"
            | "str"
            | "Vec"
            | "Option"
            | "HashMap"
            | "BTreeMap"
            | "HashSet"
            | "BTreeSet"
            | "Uuid"
    )
}

/// Convert a PascalCase identifier to snake_case.
pub(crate) fn to_snake_case(s: &str) -> String {
    let chars: Vec<char> = s.chars().collect();
    let mut result = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if c.is_uppercase() {
            let prev_lower = i > 0 && chars.get(i - 1).is_some_and(|p| p.is_lowercase());
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            if i > 0 && (prev_lower || next_lower) {
                result.push('_');
            }
            result.extend(c.to_lowercase());
        } else {
            result.push(c);
        }
    }
    result
}

/// Simple English pluralization.
pub(crate) fn pluralize(s: &str) -> String {
    if s.ends_with("ss")
        || s.ends_with("sh")
        || s.ends_with("ch")
        || s.ends_with('x')
        || s.ends_with("zz")
    {
        format!("{}es", s)
    } else if s.ends_with('z') {
        format!("{}zes", s)
    } else if s.ends_with('s') {
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
    fn test_to_pascal_case() {
        assert_eq!(to_pascal_case("get_user"), "GetUser");
        assert_eq!(to_pascal_case("list_all_projects"), "ListAllProjects");
        assert_eq!(to_pascal_case("simple"), "Simple");
        assert_eq!(to_pascal_case("send_welcome_email"), "SendWelcomeEmail");
    }

    #[test]
    fn test_parse_duration_secs() {
        assert_eq!(parse_duration_secs("30s"), Some(30));
        assert_eq!(parse_duration_secs("5m"), Some(300));
        assert_eq!(parse_duration_secs("1h"), Some(3600));
        assert_eq!(parse_duration_secs("2d"), Some(172800));
        // Bare integers are rejected — unit suffix required.
        assert_eq!(parse_duration_secs("60"), None);
        assert_eq!(parse_duration_secs("1000ms"), Some(1));
        assert_eq!(parse_duration_secs("invalid"), None);
    }

    #[test]
    fn test_parse_duration_tokens() {
        let ts = parse_duration_tokens("30s", 30);
        assert!(!ts.is_empty());

        let ts = parse_duration_tokens("5m", 300);
        assert!(!ts.is_empty());

        let ts = parse_duration_tokens("1h", 3600);
        assert!(!ts.is_empty());

        let ts = parse_duration_tokens("30", 30);
        let output = ts.to_string();
        assert!(
            output.contains("compile_error"),
            "bare integer should emit compile_error, got: {output}"
        );
    }

    #[test]
    fn test_parse_size_bytes() {
        assert_eq!(parse_size_bytes("100mb"), Some(100 * 1024 * 1024));
        assert_eq!(parse_size_bytes("1gb"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_size_bytes("512kb"), Some(512 * 1024));
        assert_eq!(parse_size_bytes("1024b"), Some(1024));
        assert_eq!(parse_size_bytes("200MB"), Some(200 * 1024 * 1024));
        assert_eq!(parse_size_bytes("1048576"), Some(1048576));
        assert_eq!(parse_size_bytes("invalid"), None);
    }

    #[test]
    fn pascal_case_handles_empty_and_edge_segments() {
        // Empty string -> empty Pascal-case output (no segments to capitalize).
        assert_eq!(to_pascal_case(""), "");
        // Single character is uppercased.
        assert_eq!(to_pascal_case("a"), "A");
        // Already-PascalCase identifier passes through unchanged (no underscores).
        assert_eq!(to_pascal_case("Already"), "Already");
        // Leading/trailing/consecutive underscores produce empty segments,
        // which map to empty strings — must not panic and must not insert
        // sentinel chars.
        assert_eq!(to_pascal_case("_foo"), "Foo");
        assert_eq!(to_pascal_case("foo_"), "Foo");
        assert_eq!(to_pascal_case("foo__bar"), "FooBar");
    }

    #[test]
    fn parse_duration_secs_accepts_ms_and_day_suffixes() {
        // Sub-second durations truncate to zero seconds (parse_duration_secs
        // discards the millisecond fraction). Callers that care about ms must
        // use parse_duration_tokens or parse_duration directly.
        assert_eq!(parse_duration_secs("500ms"), Some(0));
        assert_eq!(parse_duration_secs("1d"), Some(86400));
    }

    #[test]
    fn parse_duration_tokens_covers_all_unit_branches() {
        // Each unit branch must produce a non-empty TokenStream that does NOT
        // contain compile_error. The bare-integer case is already covered.
        for input in ["100ms", "30s", "5m", "1h", "1d"] {
            let ts = parse_duration_tokens(input, 0);
            let out = ts.to_string();
            assert!(!out.is_empty(), "empty token stream for {input}");
            assert!(
                !out.contains("compile_error"),
                "expected valid duration for {input}, got: {out}"
            );
        }
    }

    #[test]
    fn parse_duration_tokens_emits_compile_error_for_invalid_numeric() {
        // Numeric portion that doesn't parse as u64 should fall through to the
        // invalid() branch in EACH suffix arm — exercise them to make sure no
        // arm swallows the error silently.
        for input in ["xms", "abcs", "?m", " h", "  d"] {
            let out = parse_duration_tokens(input, 0).to_string();
            assert!(
                out.contains("compile_error"),
                "{input} should emit compile_error, got: {out}"
            );
        }
    }

    #[test]
    fn parse_size_bytes_is_case_insensitive_across_units() {
        // strip_suffix is matched against the lowercased input, so the unit
        // tag may be in any case — confirm the matrix.
        assert_eq!(parse_size_bytes("1KB"), Some(1024));
        assert_eq!(parse_size_bytes("1Kb"), Some(1024));
        assert_eq!(parse_size_bytes("1Gb"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_size_bytes("4B"), Some(4));
    }

    #[test]
    fn parse_size_bytes_tolerates_whitespace_and_zero() {
        // Inner whitespace between number and unit is allowed (num.trim() in
        // each arm). Outer whitespace is stripped by the initial s.trim().
        assert_eq!(parse_size_bytes("  16 kb  "), Some(16 * 1024));
        assert_eq!(parse_size_bytes("0gb"), Some(0));
        // Trailing garbage that isn't a recognized unit falls through to the
        // bare-integer branch and fails.
        assert_eq!(parse_size_bytes("10xy"), None);
    }

    fn parse_ty(src: &str) -> syn::Type {
        syn::parse_str(src).expect("type parses")
    }

    #[test]
    fn primitive_arg_type_recognizes_every_scalar() {
        // Walk the full matrix of names the matches!() arm in
        // is_primitive_arg_type accepts — keeps the list and the test in
        // lockstep if either drifts.
        let scalars = [
            "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128",
            "usize", "f32", "f64", "bool", "char", "String", "Uuid",
        ];
        for s in scalars {
            assert!(
                is_primitive_arg_type(&parse_ty(s)),
                "{s} should be treated as primitive"
            );
        }
    }

    #[test]
    fn primitive_arg_type_unwraps_references() {
        // &str -> Reference(Type::Path("str")) -> primitive. &String likewise.
        assert!(is_primitive_arg_type(&parse_ty("&str")));
        assert!(is_primitive_arg_type(&parse_ty("&String")));
        assert!(is_primitive_arg_type(&parse_ty("&i32")));
        // Nested reference still recurses through.
        assert!(is_primitive_arg_type(&parse_ty("&&u64")));
    }

    #[test]
    fn primitive_arg_type_matches_on_leaf_segment() {
        // Path qualification doesn't matter — only the last segment is
        // inspected — so fully-qualified scalars and collections still count.
        assert!(is_primitive_arg_type(&parse_ty("std::string::String")));
        assert!(is_primitive_arg_type(&parse_ty("std::vec::Vec<u8>")));
        assert!(is_primitive_arg_type(&parse_ty(
            "std::collections::HashMap<String, i32>"
        )));
        assert!(is_primitive_arg_type(&parse_ty("uuid::Uuid")));
    }

    #[test]
    fn primitive_arg_type_treats_collection_wrappers_as_primitive() {
        // Vec/Option/HashMap/etc. are not custom args structs — callers wrap
        // them in a generated args struct rather than passing through.
        assert!(is_primitive_arg_type(&parse_ty("Vec<u8>")));
        assert!(is_primitive_arg_type(&parse_ty("Option<String>")));
        assert!(is_primitive_arg_type(&parse_ty("HashMap<String, i32>")));
        assert!(is_primitive_arg_type(&parse_ty("BTreeMap<u64, bool>")));
        assert!(is_primitive_arg_type(&parse_ty("HashSet<u32>")));
        assert!(is_primitive_arg_type(&parse_ty("BTreeSet<i64>")));
    }

    #[test]
    fn primitive_arg_type_rejects_custom_structs_and_tuples() {
        // User-defined types must be passed through directly — the macros
        // detect that by getting `false` back from this helper.
        assert!(!is_primitive_arg_type(&parse_ty("MyArgs")));
        assert!(!is_primitive_arg_type(&parse_ty("crate::types::Input")));
        // Tuples are Type::Tuple, not Type::Path, so they fall through to
        // the early `return false` branch.
        assert!(!is_primitive_arg_type(&parse_ty("(u32, String)")));
        // Unit type is also Type::Tuple in syn.
        assert!(!is_primitive_arg_type(&parse_ty("()")));
        // Empty/non-path types (slice, array) are rejected too.
        assert!(!is_primitive_arg_type(&parse_ty("[u8; 4]")));
        assert!(!is_primitive_arg_type(&parse_ty("[u8]")));
    }

    #[test]
    fn snake_case_converts_pascal_case() {
        assert_eq!(to_snake_case("UserProfile"), "user_profile");
        assert_eq!(to_snake_case("HTTPRequest"), "http_request");
        assert_eq!(to_snake_case("simple"), "simple");
        assert_eq!(to_snake_case("A"), "a");
    }

    #[test]
    fn pluralize_handles_sibilants_and_z_doubling() {
        assert_eq!(pluralize("user"), "users");
        assert_eq!(pluralize("address"), "addresses");
        assert_eq!(pluralize("box"), "boxes");
        assert_eq!(pluralize("quiz"), "quizzes");
        assert_eq!(pluralize("buzz"), "buzzes");
        assert_eq!(pluralize("category"), "categories");
        assert_eq!(pluralize("key"), "keys");
    }
}

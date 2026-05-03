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
}

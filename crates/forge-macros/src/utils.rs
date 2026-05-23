//! Shared utility functions for forge macros.

use std::time::Duration;

use proc_macro_crate::{FoundCrate, crate_name};
use proc_macro2::TokenStream;
use quote::{format_ident, quote};

/// Resolve the path to the host `forge` crate at expansion time.
///
/// The crate is published as the `forgex` package but its library is named
/// `forge` (`[lib] name` in `crates/forge/Cargo.toml`) so users write
/// `use forge::...`. `proc-macro-crate` returns the *dependency key* from the
/// consumer's `Cargo.toml`, which doesn't always equal the extern crate name
/// rustc sees:
///
/// * `forge = { package = "forgex" }` (the scaffolded default) → key `forge`,
///   which is also the extern name. Emit `::forge`.
/// * a bare `forgex = "x"` dependency (what `cargo add forgex` produces, and
///   what `trybuild` generates) → key `forgex`, but rustc only knows the crate
///   by its lib name `forge`, so the key can't be used verbatim. Normalize the
///   package name back to the lib name.
/// * an explicit rename `myalias = { package = "forgex" }` → key `myalias`,
///   which *is* the extern name. Emit `::myalias`.
pub fn forge_path() -> TokenStream {
    match crate_name("forgex") {
        Ok(FoundCrate::Itself) => quote!(crate),
        Ok(FoundCrate::Name(name)) => {
            // proc-macro-crate hands back the dependency key; for a non-renamed
            // `forgex` dep that key is the package name, but the crate is only
            // reachable under its lib name `forge`.
            let extern_name = if name == "forgex" { "forge" } else { &name };
            let ident = format_ident!("{}", extern_name);
            quote!(::#ident)
        }
        // Not resolvable as a direct dependency (transitive use, or a context
        // proc-macro-crate can't read). The standard binding is `forge`.
        Err(_) => quote!(::forge),
    }
}

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
        num.parse::<u64>()
            .ok()
            .and_then(|m| m.checked_mul(60))
            .map(Duration::from_secs)
    } else if let Some(num) = s.strip_suffix('h') {
        num.parse::<u64>()
            .ok()
            .and_then(|h| h.checked_mul(3600))
            .map(Duration::from_secs)
    } else if let Some(num) = s.strip_suffix('d') {
        num.parse::<u64>()
            .ok()
            .and_then(|d| d.checked_mul(86400))
            .map(Duration::from_secs)
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
        match num.parse::<u64>().ok().and_then(|n| n.checked_mul(60)) {
            Some(secs) => quote! { std::time::Duration::from_secs(#secs) },
            None => invalid(),
        }
    } else if let Some(num) = s.strip_suffix('h') {
        match num.parse::<u64>().ok().and_then(|n| n.checked_mul(3600)) {
            Some(secs) => quote! { std::time::Duration::from_secs(#secs) },
            None => invalid(),
        }
    } else if let Some(num) = s.strip_suffix('d') {
        match num.parse::<u64>().ok().and_then(|n| n.checked_mul(86400)) {
            Some(secs) => quote! { std::time::Duration::from_secs(#secs) },
            None => invalid(),
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

/// Convert an `every = "..."` duration string to a cron expression.
///
/// Supported units: `m` (minutes), `h` (hours). Seconds are rejected because
/// the minimum cron granularity is one minute. Days and other units map to
/// the equivalent number of minutes/hours when they fit in a valid cron step.
///
/// Returns `Ok(cron_expression)` or `Err(human-readable error)`.
pub fn every_to_cron(s: &str) -> Result<String, String> {
    let s = s.trim();

    // Reject sub-minute units before numeric parsing. "ms" must be checked
    // first so that "500ms" doesn't also match the bare-'s' arm.
    if s.ends_with("ms") {
        return Err("cron minimum granularity is 1 minute; use \"1m\" or higher".to_string());
    }
    if let Some(body) = s.strip_suffix('s') {
        // Only treat it as a seconds suffix when the body ends with a digit
        // (e.g. "30s"). Things like "hours" ending in 's' are caught by the
        // fallthrough error at the end.
        if body.chars().last().is_some_and(|c| c.is_ascii_digit()) {
            return Err("cron minimum granularity is 1 minute; use \"1m\" or higher".to_string());
        }
    }

    if let Some(num_str) = s.strip_suffix('m') {
        let n: u64 = num_str.parse().map_err(|_| {
            format!("invalid duration \"{s}\": expected a positive integer before 'm'")
        })?;
        if n == 0 {
            return Err(format!("invalid duration \"{s}\": value must be >= 1"));
        }
        if n == 1 {
            return Ok("* * * * *".to_string());
        }
        if 60 % n == 0 {
            return Ok(format!("*/{n} * * * *"));
        }
        return Err(format!(
            "every = \"{s}\": {n} must evenly divide 60 for a valid cron step (use 1, 2, 3, 4, 5, 6, 10, 12, 15, 20, or 30)"
        ));
    }

    if let Some(num_str) = s.strip_suffix('h') {
        let n: u64 = num_str.parse().map_err(|_| {
            format!("invalid duration \"{s}\": expected a positive integer before 'h'")
        })?;
        if n == 0 {
            return Err(format!("invalid duration \"{s}\": value must be >= 1"));
        }
        if n == 1 {
            return Ok("0 * * * *".to_string());
        }
        if 24 % n == 0 {
            return Ok(format!("0 */{n} * * *"));
        }
        return Err(format!(
            "every = \"{s}\": {n} must evenly divide 24 for a valid cron step (use 1, 2, 3, 4, 6, 8, or 12)"
        ));
    }

    Err(format!(
        "invalid duration \"{s}\": use a suffix like \"5m\" or \"1h\""
    ))
}

/// Convert a `daily_at = "HH:MM"` string to a cron expression `"0 H * * *"`.
///
/// The timezone is handled separately at the runtime level; this function only
/// produces the schedule string. Returns `Ok(cron_expression)` or `Err(...)`.
pub fn daily_at_to_cron(s: &str) -> Result<String, String> {
    let s = s.trim();
    let (hour_str, minute_str) = s.split_once(':').ok_or_else(|| {
        format!("invalid daily_at \"{s}\": expected \"HH:MM\" format (e.g. \"03:00\")")
    })?;

    let hour: u32 = hour_str
        .parse()
        .map_err(|_| format!("invalid daily_at \"{s}\": hour must be an integer"))?;
    let minute: u32 = minute_str
        .parse()
        .map_err(|_| format!("invalid daily_at \"{s}\": minute must be an integer"))?;

    if hour > 23 {
        return Err(format!(
            "invalid daily_at \"{s}\": hour {hour} is out of range (0–23)"
        ));
    }
    if minute > 59 {
        return Err(format!(
            "invalid daily_at \"{s}\": minute {minute} is out of range (0–59)"
        ));
    }

    Ok(format!("{minute} {hour} * * *"))
}

/// Returns an error message if the type is not portable across the wire
/// (i.e. codegen cannot emit bindings for it). Must match
/// `forge-codegen/src/parser.rs:unsupported_type_reason`.
pub fn unsupported_wire_type(name: &str) -> Option<&'static str> {
    match name {
        "usize" | "isize" => Some(
            "platform-dependent size type is not portable across the wire; use i32 or i64 instead",
        ),
        "u8" | "u16" | "u32" | "u64" | "u128" | "i8" | "i16" | "i128" => Some(
            "unsigned and narrow integer types are not supported in handler signatures; use i32 or i64 for portability",
        ),
        _ => None,
    }
}

/// Check a handler argument type for wire-incompatible leaf types.
/// Returns `Some(error_message)` when the outermost non-wrapper type is
/// unsupported. Does not recurse into generic parameters — codegen's own
/// `validate_registry` handles nested types and special-cases like `Vec<u8>`.
pub fn check_arg_wire_type(ty: &syn::Type) -> Option<(String, proc_macro2::Span)> {
    let ident = leaf_type_ident(ty)?;
    let name = ident.to_string();
    unsupported_wire_type(&name).map(|reason| (reason.to_string(), ident.span()))
}

/// Walk through references and single-type-parameter wrappers (Option, Vec)
/// to find the innermost named type segment.
fn leaf_type_ident(ty: &syn::Type) -> Option<&syn::Ident> {
    match ty {
        syn::Type::Reference(r) => leaf_type_ident(&r.elem),
        syn::Type::Path(p) => {
            let seg = p.path.segments.last()?;
            let name = seg.ident.to_string();
            if matches!(name.as_str(), "Option" | "Vec")
                && let syn::PathArguments::AngleBracketed(args) = &seg.arguments
                && let Some(syn::GenericArgument::Type(inner)) = args.args.first()
            {
                return leaf_type_ident(inner);
            }
            Some(&seg.ident)
        }
        _ => None,
    }
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
            "i8", "i16", "i32", "i64", "i128", "isize", "u8", "u16", "u32", "u64", "u128", "usize",
            "f32", "f64", "bool", "char", "String", "Uuid",
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
    fn unsupported_wire_type_rejects_platform_dependent_and_unsigned() {
        assert!(unsupported_wire_type("usize").is_some());
        assert!(unsupported_wire_type("isize").is_some());
        assert!(unsupported_wire_type("u8").is_some());
        assert!(unsupported_wire_type("u16").is_some());
        assert!(unsupported_wire_type("u32").is_some());
        assert!(unsupported_wire_type("u64").is_some());
        assert!(unsupported_wire_type("u128").is_some());
        assert!(unsupported_wire_type("i8").is_some());
        assert!(unsupported_wire_type("i16").is_some());
        assert!(unsupported_wire_type("i128").is_some());
    }

    #[test]
    fn unsupported_wire_type_accepts_portable_types() {
        assert!(unsupported_wire_type("i32").is_none());
        assert!(unsupported_wire_type("i64").is_none());
        assert!(unsupported_wire_type("f32").is_none());
        assert!(unsupported_wire_type("f64").is_none());
        assert!(unsupported_wire_type("bool").is_none());
        assert!(unsupported_wire_type("String").is_none());
        assert!(unsupported_wire_type("Uuid").is_none());
        assert!(unsupported_wire_type("MyStruct").is_none());
    }

    #[test]
    fn check_arg_wire_type_rejects_bare_unsigned() {
        assert!(check_arg_wire_type(&parse_ty("u32")).is_some());
        assert!(check_arg_wire_type(&parse_ty("usize")).is_some());
        assert!(check_arg_wire_type(&parse_ty("i128")).is_some());
    }

    #[test]
    fn check_arg_wire_type_recurses_through_option_and_vec() {
        assert!(check_arg_wire_type(&parse_ty("Option<u32>")).is_some());
        assert!(check_arg_wire_type(&parse_ty("Vec<usize>")).is_some());
        assert!(check_arg_wire_type(&parse_ty("Option<Vec<i128>>")).is_some());
    }

    #[test]
    fn check_arg_wire_type_accepts_portable_types() {
        assert!(check_arg_wire_type(&parse_ty("i32")).is_none());
        assert!(check_arg_wire_type(&parse_ty("i64")).is_none());
        assert!(check_arg_wire_type(&parse_ty("f64")).is_none());
        assert!(check_arg_wire_type(&parse_ty("String")).is_none());
        assert!(check_arg_wire_type(&parse_ty("Option<i32>")).is_none());
        assert!(check_arg_wire_type(&parse_ty("Vec<String>")).is_none());
        assert!(check_arg_wire_type(&parse_ty("MyStruct")).is_none());
    }

    #[test]
    fn check_arg_wire_type_skips_non_wrapper_generics() {
        // HashMap<String, u32> — leaf is HashMap, not u32, so it passes.
        // Deep inner types are validated by codegen's validate_registry.
        assert!(check_arg_wire_type(&parse_ty("HashMap<String, u32>")).is_none());
    }

    #[test]
    fn every_to_cron_converts_minutes() {
        assert_eq!(every_to_cron("1m").unwrap(), "* * * * *");
        assert_eq!(every_to_cron("5m").unwrap(), "*/5 * * * *");
        assert_eq!(every_to_cron("15m").unwrap(), "*/15 * * * *");
        assert_eq!(every_to_cron("30m").unwrap(), "*/30 * * * *");
        assert_eq!(every_to_cron("60m").unwrap(), "*/60 * * * *");
    }

    #[test]
    fn every_to_cron_converts_hours() {
        assert_eq!(every_to_cron("1h").unwrap(), "0 * * * *");
        assert_eq!(every_to_cron("2h").unwrap(), "0 */2 * * *");
        assert_eq!(every_to_cron("6h").unwrap(), "0 */6 * * *");
        assert_eq!(every_to_cron("12h").unwrap(), "0 */12 * * *");
    }

    #[test]
    fn every_to_cron_rejects_sub_minute() {
        let err = every_to_cron("30s").unwrap_err();
        assert!(
            err.contains("minimum granularity"),
            "unexpected error: {err}"
        );
        let err = every_to_cron("500ms").unwrap_err();
        assert!(err.contains("minimum granularity"), "unexpected: {err}");
    }

    #[test]
    fn every_to_cron_rejects_non_divisors() {
        // 7m does not evenly divide 60 — must error.
        let err = every_to_cron("7m").unwrap_err();
        assert!(err.contains("evenly divide"), "unexpected: {err}");
        // 5h does not evenly divide 24 — must error.
        let err = every_to_cron("5h").unwrap_err();
        assert!(err.contains("evenly divide"), "unexpected: {err}");
    }

    #[test]
    fn every_to_cron_rejects_zero_and_invalid() {
        assert!(every_to_cron("0m").is_err());
        assert!(every_to_cron("0h").is_err());
        assert!(every_to_cron("xm").is_err());
        assert!(every_to_cron("abc").is_err());
        // No suffix at all.
        assert!(every_to_cron("5").is_err());
    }

    #[test]
    fn daily_at_to_cron_converts_time() {
        assert_eq!(daily_at_to_cron("03:00").unwrap(), "0 3 * * *");
        assert_eq!(daily_at_to_cron("00:00").unwrap(), "0 0 * * *");
        assert_eq!(daily_at_to_cron("23:59").unwrap(), "59 23 * * *");
        assert_eq!(daily_at_to_cron("12:30").unwrap(), "30 12 * * *");
    }

    #[test]
    fn daily_at_to_cron_rejects_bad_input() {
        // Missing colon.
        assert!(daily_at_to_cron("0300").is_err());
        // Hour out of range.
        let err = daily_at_to_cron("24:00").unwrap_err();
        assert!(err.contains("out of range"), "unexpected: {err}");
        // Minute out of range.
        let err = daily_at_to_cron("12:60").unwrap_err();
        assert!(err.contains("out of range"), "unexpected: {err}");
        // Non-numeric.
        assert!(daily_at_to_cron("ab:cd").is_err());
        // Leading whitespace is trimmed — valid.
        assert!(daily_at_to_cron("  09:00  ").is_ok());
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

//! Shared utility functions for the Forge framework.

use std::time::Duration;

/// Parse a duration string into `Duration`.
///
/// Supports the following suffixes:
/// - `ms` - milliseconds
/// - `s` - seconds
/// - `m` - minutes
/// - `h` - hours
/// - `d` - days
///
/// If no suffix is provided, the value is interpreted as seconds.
pub fn parse_duration(s: &str) -> Option<Duration> {
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
        s.parse::<u64>().ok().map(Duration::from_secs)
    }
}

/// Parse a human-readable size string into bytes.
///
/// Supports the following suffixes (case-insensitive):
/// - `kb` - kilobytes
/// - `mb` - megabytes
/// - `gb` - gigabytes
/// - `b` - bytes
///
/// If no suffix is provided, the value is interpreted as bytes.
pub fn parse_size(s: &str) -> Option<usize> {
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

/// Convert a PascalCase or camelCase string to snake_case.
pub fn to_snake_case(s: &str) -> String {
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

/// Simple English pluralization for table-name derivation.
pub fn pluralize(s: &str) -> String {
    if s.ends_with("ss")
        || s.ends_with("sh")
        || s.ends_with("ch")
        || s.ends_with('x')
        || s.ends_with("zz")
    {
        format!("{s}es")
    } else if s.ends_with('z') {
        format!("{s}zes")
    } else if s.ends_with('s') {
        format!("{s}es")
    } else if let Some(stem) = s.strip_suffix('y') {
        if !s.ends_with("ay") && !s.ends_with("ey") && !s.ends_with("oy") && !s.ends_with("uy") {
            format!("{stem}ies")
        } else {
            format!("{s}s")
        }
    } else {
        format!("{s}s")
    }
}

/// Convert a snake_case string to camelCase.
pub fn to_camel_case(s: &str) -> String {
    let mut result = String::new();
    let mut capitalize_next = false;
    for c in s.chars() {
        if c == '_' {
            capitalize_next = true;
        } else if capitalize_next {
            result.extend(c.to_uppercase());
            capitalize_next = false;
        } else {
            result.push(c);
        }
    }
    result
}

/// Normalize an args/input envelope before deserialization.
///
/// Job and workflow handlers accept either a bare value or a single-key
/// `{"args": …}` / `{"input": …}` wrapper depending on how the caller phrased
/// the dispatch. This helper unwraps the envelope so the handler's `Args` /
/// `Input` deserialize path doesn't have to special-case both shapes. `null`
/// is collapsed to an empty object so handlers with `()` args still match.
pub fn normalize_handler_args(args: serde_json::Value) -> serde_json::Value {
    let unwrapped = match &args {
        serde_json::Value::Object(map) if map.len() == 1 => {
            if map.contains_key("args") {
                map.get("args").cloned().unwrap_or(serde_json::Value::Null)
            } else if map.contains_key("input") {
                map.get("input").cloned().unwrap_or(serde_json::Value::Null)
            } else {
                args
            }
        }
        _ => args,
    };

    match &unwrapped {
        serde_json::Value::Null => serde_json::Value::Object(serde_json::Map::new()),
        _ => unwrapped,
    }
}

/// Extract the bare hostname from an authority component (`host[:port]`),
/// stripping an IPv6 bracket pair and any port. e.g. `[::1]:8080` -> `::1`,
/// `localhost:9081` -> `localhost`, `127.0.0.1` -> `127.0.0.1`.
pub fn hostname_from_authority(authority: &str) -> &str {
    match authority.strip_prefix('[') {
        // IPv6 literal: the hostname is everything up to the closing bracket.
        Some(rest) => rest.split(']').next().unwrap_or(rest),
        None => authority.split(':').next().unwrap_or(authority),
    }
}

/// True if `hostname` is a loopback address. Expects a bare hostname with no
/// port or brackets (see [`hostname_from_authority`]).
pub fn is_loopback_host(hostname: &str) -> bool {
    matches!(hostname, "localhost" | "127.0.0.1" | "::1")
}

/// Bare hostname of a plain-`http://` URL (port and IPv6 brackets stripped),
/// or `None` if `url` is not `http://`.
///
/// Used to decide whether a plain-HTTP endpoint is a safe loopback exception to
/// the HTTPS requirement. A naive `starts_with("http://localhost")` check would
/// wrongly accept `http://localhost.evil.com`, so callers parse the host first.
pub fn http_hostname(url: &str) -> Option<&str> {
    let rest = url.strip_prefix("http://")?;
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    Some(hostname_from_authority(authority))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_duration_milliseconds() {
        assert_eq!(parse_duration("100ms"), Some(Duration::from_millis(100)));
        assert_eq!(parse_duration("1000ms"), Some(Duration::from_millis(1000)));
    }

    #[test]
    fn test_parse_duration_seconds() {
        assert_eq!(parse_duration("30s"), Some(Duration::from_secs(30)));
        assert_eq!(parse_duration("60s"), Some(Duration::from_secs(60)));
    }

    #[test]
    fn test_parse_duration_minutes() {
        assert_eq!(parse_duration("5m"), Some(Duration::from_secs(300)));
        assert_eq!(parse_duration("10m"), Some(Duration::from_secs(600)));
    }

    #[test]
    fn test_parse_duration_hours() {
        assert_eq!(parse_duration("1h"), Some(Duration::from_secs(3600)));
        assert_eq!(parse_duration("24h"), Some(Duration::from_secs(86400)));
    }

    #[test]
    fn test_parse_duration_days() {
        assert_eq!(parse_duration("1d"), Some(Duration::from_secs(86400)));
        assert_eq!(parse_duration("7d"), Some(Duration::from_secs(604800)));
    }

    #[test]
    fn test_parse_duration_bare_number() {
        assert_eq!(parse_duration("60"), Some(Duration::from_secs(60)));
        assert_eq!(parse_duration("3600"), Some(Duration::from_secs(3600)));
    }

    #[test]
    fn test_parse_duration_whitespace() {
        assert_eq!(parse_duration("  30s  "), Some(Duration::from_secs(30)));
    }

    #[test]
    fn test_parse_duration_invalid() {
        assert_eq!(parse_duration("invalid"), None);
        assert_eq!(parse_duration("abc123"), None);
        assert_eq!(parse_duration(""), None);
    }

    #[test]
    fn test_parse_size_kilobytes() {
        assert_eq!(parse_size("100kb"), Some(100 * 1024));
        assert_eq!(parse_size("512KB"), Some(512 * 1024));
    }

    #[test]
    fn test_parse_size_megabytes() {
        assert_eq!(parse_size("20mb"), Some(20 * 1024 * 1024));
        assert_eq!(parse_size("100MB"), Some(100 * 1024 * 1024));
    }

    #[test]
    fn test_parse_size_gigabytes() {
        assert_eq!(parse_size("1gb"), Some(1024 * 1024 * 1024));
        assert_eq!(parse_size("2GB"), Some(2 * 1024 * 1024 * 1024));
    }

    #[test]
    fn test_parse_size_bytes() {
        assert_eq!(parse_size("1024b"), Some(1024));
        assert_eq!(parse_size("0b"), Some(0));
    }

    #[test]
    fn test_parse_size_bare_number() {
        assert_eq!(parse_size("1048576"), Some(1048576));
    }

    #[test]
    fn test_parse_size_whitespace() {
        assert_eq!(parse_size("  20mb  "), Some(20 * 1024 * 1024));
    }

    #[test]
    fn test_parse_size_invalid() {
        assert_eq!(parse_size("invalid"), None);
        assert_eq!(parse_size("abc123"), None);
        assert_eq!(parse_size(""), None);
    }

    #[test]
    fn test_to_snake_case() {
        assert_eq!(to_snake_case("GetUser"), "get_user");
        assert_eq!(to_snake_case("ListAllProjects"), "list_all_projects");
        assert_eq!(to_snake_case("Simple"), "simple");
        assert_eq!(to_snake_case("ProjectStatus"), "project_status");
        assert_eq!(to_snake_case("HTTPServer"), "http_server");
        assert_eq!(to_snake_case("XMLParser"), "xml_parser");
        assert_eq!(to_snake_case("listInvoices"), "list_invoices");
        assert_eq!(to_snake_case("foo2Bar"), "foo2_bar");
        assert_eq!(to_snake_case("already_snake"), "already_snake");
    }

    #[test]
    fn test_to_pascal_case() {
        assert_eq!(to_pascal_case("get_user"), "GetUser");
        assert_eq!(to_pascal_case("list_all_projects"), "ListAllProjects");
        assert_eq!(to_pascal_case("simple"), "Simple");
    }

    #[test]
    fn test_pluralize() {
        assert_eq!(pluralize("user"), "users");
        assert_eq!(pluralize("bus"), "buses");
        assert_eq!(pluralize("quiz"), "quizzes");
        assert_eq!(pluralize("index"), "indexes");
        assert_eq!(pluralize("match"), "matches");
        assert_eq!(pluralize("wish"), "wishes");
        assert_eq!(pluralize("box"), "boxes");
        assert_eq!(pluralize("class"), "classes");
        assert_eq!(pluralize("buzz"), "buzzes");
        assert_eq!(pluralize("policy"), "policies");
        assert_eq!(pluralize("key"), "keys");
        assert_eq!(pluralize("day"), "days");
    }

    #[test]
    fn test_to_camel_case() {
        assert_eq!(to_camel_case("get_user"), "getUser");
        assert_eq!(to_camel_case("list_all_projects"), "listAllProjects");
        assert_eq!(to_camel_case("simple"), "simple");
    }

    #[test]
    fn normalize_handler_args_converts_null_to_empty_object() {
        use serde_json::json;
        assert_eq!(normalize_handler_args(json!(null)), json!({}));
    }

    #[test]
    fn normalize_handler_args_unwraps_args_envelope() {
        use serde_json::json;
        assert_eq!(
            normalize_handler_args(json!({"args": {"x": 1}})),
            json!({"x": 1})
        );
        assert_eq!(normalize_handler_args(json!({"args": null})), json!({}));
    }

    #[test]
    fn normalize_handler_args_unwraps_input_envelope() {
        use serde_json::json;
        assert_eq!(
            normalize_handler_args(json!({"input": [1, 2]})),
            json!([1, 2])
        );
    }

    #[test]
    fn normalize_handler_args_preserves_other_shapes() {
        use serde_json::json;
        assert_eq!(normalize_handler_args(json!({"id": 7})), json!({"id": 7}));
        assert_eq!(normalize_handler_args(json!([1, 2])), json!([1, 2]));
        assert_eq!(normalize_handler_args(json!(42)), json!(42));
    }

    #[test]
    fn hostname_from_authority_strips_port_and_brackets() {
        assert_eq!(hostname_from_authority("localhost:9081"), "localhost");
        assert_eq!(hostname_from_authority("127.0.0.1"), "127.0.0.1");
        assert_eq!(hostname_from_authority("[::1]:8080"), "::1");
        assert_eq!(hostname_from_authority("[::1]"), "::1");
        assert_eq!(hostname_from_authority("example.com:443"), "example.com");
    }

    #[test]
    fn is_loopback_host_matches_only_loopback() {
        assert!(is_loopback_host("localhost"));
        assert!(is_loopback_host("127.0.0.1"));
        assert!(is_loopback_host("::1"));
        assert!(!is_loopback_host("localhost.evil.com"));
        assert!(!is_loopback_host("example.com"));
    }

    #[test]
    fn http_hostname_parses_host_and_rejects_spoofs() {
        assert_eq!(
            http_hostname("http://localhost:9081/jwks"),
            Some("localhost")
        );
        assert_eq!(http_hostname("http://[::1]:8080/jwks"), Some("::1"));
        assert_eq!(http_hostname("http://127.0.0.1/cb?x=1"), Some("127.0.0.1"));
        // The classic spoof: a subdomain of localhost is not loopback.
        assert_eq!(
            http_hostname("http://localhost.evil.com/cb"),
            Some("localhost.evil.com")
        );
        // Not plain HTTP.
        assert_eq!(http_hostname("https://localhost/jwks"), None);
    }
}

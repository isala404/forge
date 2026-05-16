//! Config file loading, TOML parsing, and environment variable substitution.

/// Substitute environment variables in the format `${VAR_NAME}`.
///
/// Supports default values with `${VAR-default}` or `${VAR:-default}`.
/// When the env var is unset, the default is used. Without a default,
/// the literal `${VAR}` is preserved (so TOML parsing can still fail
/// loudly if a required variable is missing).
#[allow(clippy::indexing_slicing)] // All indices from str::find(); guaranteed valid UTF-8 boundaries.
pub fn substitute_env_vars(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let mut remaining = content;

    while let Some(start) = remaining.find("${") {
        // Copy everything before the `${`
        result.push_str(&remaining[..start]);

        let after_open = &remaining[start + 2..];
        match after_open.find('}') {
            Some(end) => {
                let inner = &after_open[..end];
                let (var_name, default_value) = parse_var_with_default(inner);

                if is_valid_env_var_name(var_name) {
                    if let Ok(value) = std::env::var(var_name) {
                        result.push_str(&value);
                    } else if let Some(default) = default_value {
                        result.push_str(default);
                    } else {
                        // Preserve the literal so TOML parsing fails loudly
                        result.push_str(&remaining[start..start + 2 + end + 1]);
                    }
                } else {
                    // Not a valid env var name, preserve literal
                    result.push_str(&remaining[start..start + 2 + end + 1]);
                }
                remaining = &after_open[end + 1..];
            }
            None => {
                // No closing brace, preserve rest of string as-is
                result.push_str(&remaining[start..]);
                remaining = "";
            }
        }
    }

    // Append any trailing content after the last substitution
    result.push_str(remaining);
    result
}

/// Parse `VAR-default` or `VAR:-default` into (name, optional default).
/// Both forms behave identically (fallback when unset). `:-` is checked
/// first so its `-` doesn't get matched by the plain `-` branch.
#[allow(clippy::indexing_slicing)] // All indices from str::find(); guaranteed valid.
fn parse_var_with_default(inner: &str) -> (&str, Option<&str>) {
    if let Some(pos) = inner.find(":-") {
        return (&inner[..pos], Some(&inner[pos + 2..]));
    }
    if let Some(pos) = inner.find('-') {
        return (&inner[..pos], Some(&inner[pos + 1..]));
    }
    (inner, None)
}

fn is_valid_env_var_name(name: &str) -> bool {
    let first = match name.as_bytes().first() {
        Some(b) => b,
        None => return false,
    };
    (first.is_ascii_uppercase() || *first == b'_')
        && name
            .bytes()
            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, unsafe_code)]
mod tests {
    use super::*;

    #[test]
    fn default_used_when_unset() {
        unsafe { std::env::remove_var("TEST_FORGE_OTEL_UNSET") };

        let input = r#"enabled = ${TEST_FORGE_OTEL_UNSET-false}"#;
        let result = substitute_env_vars(input);
        assert_eq!(result, "enabled = false");
    }

    #[test]
    fn default_overridden_when_set() {
        unsafe { std::env::set_var("TEST_FORGE_OTEL_SET", "true") };

        let input = r#"enabled = ${TEST_FORGE_OTEL_SET-false}"#;
        let result = substitute_env_vars(input);
        assert_eq!(result, "enabled = true");

        unsafe { std::env::remove_var("TEST_FORGE_OTEL_SET") };
    }

    #[test]
    fn colon_dash_default() {
        unsafe { std::env::remove_var("TEST_FORGE_ENDPOINT_UNSET") };

        let input = r#"endpoint = "${TEST_FORGE_ENDPOINT_UNSET:-http://localhost:4318}""#;
        let result = substitute_env_vars(input);
        assert_eq!(result, r#"endpoint = "http://localhost:4318""#);
    }

    #[test]
    fn no_default_preserves_literal() {
        unsafe { std::env::remove_var("TEST_FORGE_MISSING") };

        let input = r#"url = "${TEST_FORGE_MISSING}""#;
        let result = substitute_env_vars(input);
        assert_eq!(result, r#"url = "${TEST_FORGE_MISSING}""#);
    }

    #[test]
    fn empty_default() {
        unsafe { std::env::remove_var("TEST_FORGE_EMPTY_DEFAULT") };

        let input = r#"val = "${TEST_FORGE_EMPTY_DEFAULT-}""#;
        let result = substitute_env_vars(input);
        assert_eq!(result, r#"val = """#);
    }
}

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
                    result.push_str(&remaining[start..start + 2 + end + 1]);
                }
                remaining = &after_open[end + 1..];
            }
            None => {
                result.push_str(&remaining[start..]);
                remaining = "";
            }
        }
    }

    result.push_str(remaining);
    result
}

/// Parse `VAR-default` or `VAR:-default` into (name, optional default).
/// Both forms behave identically (fallback when unset). `:-` is checked
/// first so its `-` doesn't get matched by the plain `-` branch.
///
/// For the bare `-` form, the split is taken at the LAST `-` so that
/// `${MY-NAMESPACE-VAR-fallback}` parses to name `MY-NAMESPACE-VAR`
/// (which then fails `is_valid_env_var_name` and the literal is
/// preserved) rather than silently substituting `$MY` with default
/// `NAMESPACE-VAR-fallback`.
#[allow(clippy::indexing_slicing)] // All indices from str::find(); guaranteed valid.
fn parse_var_with_default(inner: &str) -> (&str, Option<&str>) {
    if let Some(pos) = inner.find(":-") {
        return (&inner[..pos], Some(&inner[pos + 2..]));
    }
    if let Some((name, default)) = inner.rsplit_once('-') {
        return (name, Some(default));
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

    #[test]
    fn plain_braced_var_substituted_when_set() {
        // `${VAR}` with no default, variable present -> raw value.
        unsafe { std::env::set_var("TEST_FORGE_PLAIN_SET", "postgres://db") };

        let input = r#"url = "${TEST_FORGE_PLAIN_SET}""#;
        let result = substitute_env_vars(input);
        assert_eq!(result, r#"url = "postgres://db""#);

        unsafe { std::env::remove_var("TEST_FORGE_PLAIN_SET") };
    }

    #[test]
    fn set_var_wins_over_dash_default() {
        unsafe { std::env::set_var("TEST_FORGE_DASH_SET", "real") };

        let input = r#"x = "${TEST_FORGE_DASH_SET-fallback}""#;
        let result = substitute_env_vars(input);
        assert_eq!(result, r#"x = "real""#);

        unsafe { std::env::remove_var("TEST_FORGE_DASH_SET") };
    }

    #[test]
    fn dash_split_takes_last_dash() {
        // `parse_var_with_default` splits on the LAST `-`, so the name here is
        // "TEST_FORGE_NS_VAR" and the default is "tail". Name is valid and unset,
        // so the default wins.
        unsafe { std::env::remove_var("TEST_FORGE_NS_VAR") };

        let input = r#"v = "${TEST_FORGE_NS_VAR-tail}""#;
        let result = substitute_env_vars(input);
        assert_eq!(result, r#"v = "tail""#);
    }

    #[test]
    fn invalid_var_name_from_multi_dash_preserves_literal() {
        // Last-dash split yields name "MY-NAMESPACE-VAR", which fails
        // `is_valid_env_var_name` (contains '-'). The whole `${...}` is kept
        // verbatim rather than silently substituting a partial match.
        unsafe { std::env::remove_var("MY") };

        let input = r#"v = "${MY-NAMESPACE-VAR-fallback}""#;
        let result = substitute_env_vars(input);
        assert_eq!(result, r#"v = "${MY-NAMESPACE-VAR-fallback}""#);
    }

    #[test]
    fn lowercase_var_name_is_invalid_and_preserved() {
        // Env var names must be uppercase/underscore-led; a lowercase name is
        // not treated as a variable.
        let input = r#"v = "${lowercase}""#;
        let result = substitute_env_vars(input);
        assert_eq!(result, r#"v = "${lowercase}""#);
    }

    #[test]
    fn unterminated_brace_kept_verbatim() {
        // No closing `}` -> the remainder is emitted as-is, no panic.
        let input = r#"v = "${TEST_FORGE_UNTERMINATED"#;
        let result = substitute_env_vars(input);
        assert_eq!(result, r#"v = "${TEST_FORGE_UNTERMINATED"#);
    }

    #[test]
    fn colon_dash_split_is_preferred_over_plain_dash() {
        // `:-` is checked before plain `-`, so the name is the part before `:-`
        // and the `-` inside the default is left intact.
        unsafe { std::env::remove_var("TEST_FORGE_CDASH") };

        let input = r#"v = "${TEST_FORGE_CDASH:-a-b-c}""#;
        let result = substitute_env_vars(input);
        assert_eq!(result, r#"v = "a-b-c""#);
    }

    #[test]
    fn parse_var_with_default_forms() {
        assert_eq!(parse_var_with_default("VAR"), ("VAR", None));
        assert_eq!(
            parse_var_with_default("VAR-default"),
            ("VAR", Some("default"))
        );
        assert_eq!(
            parse_var_with_default("VAR:-default"),
            ("VAR", Some("default"))
        );
        // Last-dash split.
        assert_eq!(parse_var_with_default("A-B-C"), ("A-B", Some("C")));
        // Colon-dash beats plain dash and keeps trailing dashes in the default.
        assert_eq!(parse_var_with_default("V:-a-b"), ("V", Some("a-b")));
    }
}

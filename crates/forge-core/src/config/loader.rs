//! Config file loading, TOML parsing, and environment variable substitution.

use crate::error::{ForgeError, Result};

/// Reject config patterns where secret-like env vars have hardcoded defaults.
/// Catches `${JWT_SECRET-my-default}` before it silently becomes a production secret.
pub(crate) fn reject_secret_defaults(content: &str) -> Result<()> {
    const SECRET_KEYWORDS: &[&str] = &["secret", "password", "key"];

    let bytes = content.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if i + 1 < len
            && bytes.get(i) == Some(&b'$')
            && bytes.get(i + 1) == Some(&b'{')
            && let Some(end) = content.get(i + 2..).and_then(|s| s.find('}'))
        {
            let inner = &content[i + 2..i + 2 + end];
            let (var_name, default_value) = parse_var_with_default(inner);

            if default_value.is_some() {
                let var_lower = var_name.to_lowercase();
                for keyword in SECRET_KEYWORDS {
                    if var_lower.contains(keyword) {
                        return Err(ForgeError::Config(format!(
                            "${{{inner}}} uses a hardcoded default for a secret. \
                             Remove the default value and set {var_name} as an environment variable."
                        )));
                    }
                }
            }

            i += 2 + end + 1;
            continue;
        }
        i += 1;
    }

    Ok(())
}

/// Substitute environment variables in the format `${VAR_NAME}`.
///
/// Supports default values with `${VAR-default}` or `${VAR:-default}`.
/// When the env var is unset, the default is used. Without a default,
/// the literal `${VAR}` is preserved (so TOML parsing can still fail
/// loudly if a required variable is missing).
#[allow(clippy::indexing_slicing)]
pub fn substitute_env_vars(content: &str) -> String {
    let mut result = String::with_capacity(content.len());
    let bytes = content.as_bytes();
    let len = bytes.len();
    let mut i = 0;

    while i < len {
        if i + 1 < len
            && bytes[i] == b'$'
            && bytes[i + 1] == b'{'
            && let Some(end) = content[i + 2..].find('}')
        {
            let inner = &content[i + 2..i + 2 + end];

            let (var_name, default_value) = parse_var_with_default(inner);

            if is_valid_env_var_name(var_name) {
                if let Ok(value) = std::env::var(var_name) {
                    result.push_str(&value);
                } else if let Some(default) = default_value {
                    result.push_str(default);
                } else {
                    result.push_str(&content[i..i + 2 + end + 1]);
                }
                i += 2 + end + 1;
                continue;
            }
        }
        result.push(bytes[i] as char);
        i += 1;
    }

    result
}

/// Parse `VAR-default` or `VAR:-default` into (name, optional default).
/// Both forms behave identically (fallback when unset). `:-` is checked
/// first so its `-` doesn't get matched by the plain `-` branch.
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

    #[test]
    fn reject_secret_with_default() {
        let input = r#"secret = "${JWT_SECRET-my-default}""#;
        let result = reject_secret_defaults(input);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("hardcoded default"), "{err}");
    }

    #[test]
    fn allow_secret_without_default() {
        let input = r#"secret = "${JWT_SECRET}""#;
        assert!(reject_secret_defaults(input).is_ok());
    }

    #[test]
    fn allow_non_secret_with_default() {
        let input = r#"port = "${PORT-9081}""#;
        assert!(reject_secret_defaults(input).is_ok());
    }
}

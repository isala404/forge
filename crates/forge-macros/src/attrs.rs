//! Shared darling attribute structs for forge proc macros.
//!
//! These structs use `darling::FromMeta` to parse macro attributes declaratively
//! instead of manual string scanning. Each macro's `parse_*_attrs` function
//! converts from the darling struct to its internal representation.

use darling::FromMeta;

/// Rate limit configuration shared between query, mutation, and mcp_tool macros.
///
/// Parses `rate_limit(requests = 100, per = "1m", key = "user")`.
#[derive(Debug, Clone, Default, FromMeta)]
pub struct RateLimitMeta {
    pub requests: Option<u32>,
    pub per: Option<String>,
    pub key: Option<String>,
}

/// Retry configuration for job macros.
///
/// Parses `retry(max_attempts = 3, backoff = "exponential", max_backoff = "5m")`.
#[derive(Debug, Clone, Default, FromMeta)]
pub struct RetryMeta {
    pub max_attempts: Option<u32>,
    pub backoff: Option<String>,
    pub max_backoff: Option<String>,
}

/// Helper that accepts `require_role("admin")` or `require_role = "admin"`.
///
/// Darling's `FromMeta` for `String` handles `= "value"` natively.
/// For the parenthesized form `require_role("admin")`, we implement a custom
/// wrapper that handles both `Meta::NameValue` and `Meta::List`.
#[derive(Debug, Clone)]
pub struct RequireRole(pub String);

impl FromMeta for RequireRole {
    fn from_string(value: &str) -> darling::Result<Self> {
        Ok(RequireRole(value.to_string()))
    }

    fn from_list(items: &[darling::ast::NestedMeta]) -> darling::Result<Self> {
        if items.len() != 1 {
            return Err(darling::Error::custom(
                "require_role expects exactly one string argument, e.g. require_role(\"admin\")",
            ));
        }
        match &items[0] {
            darling::ast::NestedMeta::Lit(syn::Lit::Str(s)) => Ok(RequireRole(s.value())),
            _ => Err(darling::Error::custom(
                "require_role expects a string literal, e.g. require_role(\"admin\")",
            )),
        }
    }
}

/// Helper for `tables("users", "orders")` list-of-strings attribute.
#[derive(Debug, Clone)]
pub struct TablesList(pub Vec<String>);

impl FromMeta for TablesList {
    fn from_meta(item: &syn::Meta) -> darling::Result<Self> {
        // Detect the old `tables = [...]` array syntax and emit a migration hint.
        if let syn::Meta::NameValue(nv) = item
            && let syn::Expr::Array(_) = &nv.value
        {
            return Err(darling::Error::custom(
                "the `tables = [...]` syntax was removed; use `tables(\"foo\", \"bar\")` instead",
            ));
        }
        // Fall through to the standard list-form parser.
        match item {
            syn::Meta::List(_) => {
                let nested =
                    darling::ast::NestedMeta::parse_meta_list(item.require_list()?.tokens.clone())
                        .map_err(darling::Error::from)?;
                Self::from_list(&nested)
            }
            _ => Err(darling::Error::custom(
                "tables expects a parenthesized list of string literals, e.g. tables(\"users\", \"orders\")",
            )),
        }
    }

    fn from_list(items: &[darling::ast::NestedMeta]) -> darling::Result<Self> {
        let mut tables = Vec::new();
        for item in items {
            match item {
                darling::ast::NestedMeta::Lit(syn::Lit::Str(s)) => {
                    tables.push(s.value());
                }
                _ => {
                    return Err(darling::Error::custom(
                        "tables expects string literals, e.g. tables(\"users\", \"orders\")",
                    ));
                }
            }
        }
        if tables.is_empty() {
            return Err(darling::Error::custom("tables list must not be empty"));
        }
        Ok(TablesList(tables))
    }
}

/// Helper for idempotent flag that can be bare `idempotent` or `idempotent(key = "...")`.
#[derive(Debug, Clone)]
pub struct IdempotentMeta {
    pub enabled: bool,
    pub key: Option<String>,
}

impl FromMeta for IdempotentMeta {
    fn from_word() -> darling::Result<Self> {
        Ok(IdempotentMeta {
            enabled: true,
            key: None,
        })
    }

    fn from_list(items: &[darling::ast::NestedMeta]) -> darling::Result<Self> {
        #[derive(FromMeta)]
        struct Inner {
            key: Option<String>,
        }
        let inner = Inner::from_list(items)?;
        Ok(IdempotentMeta {
            enabled: true,
            key: inner.key,
        })
    }
}

/// Validate a rate limit key string.
pub fn validate_rate_limit_key(key: &str) -> Result<(), String> {
    if ["user", "ip", "tenant", "global"].contains(&key) || key.starts_with("custom(") {
        Ok(())
    } else {
        Err(format!(
            "invalid rate_limit key \"{key}\". Valid keys: \"user\", \"ip\", \"tenant\", \"global\", or \"custom(...)\""
        ))
    }
}

/// Validate that rate limit fields are complete when any are present.
pub fn validate_rate_limit(rl: &RateLimitMeta) -> syn::Result<()> {
    let has_any = rl.requests.is_some() || rl.per.is_some() || rl.key.is_some();
    if !has_any {
        return Ok(());
    }

    if rl.requests.is_none() {
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "rate_limit requires `requests` field (e.g. rate_limit(requests = 100, per = \"1m\", key = \"user\"))",
        ));
    }

    if let Some(0) = rl.requests {
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "rate_limit requests must be at least 1",
        ));
    }

    if rl.per.is_none() {
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "rate_limit requires `per` field (e.g. rate_limit(requests = 100, per = \"1m\", key = \"user\"))",
        ));
    }

    if let Some(ref key) = rl.key
        && let Err(msg) = validate_rate_limit_key(key)
    {
        return Err(syn::Error::new(proc_macro2::Span::call_site(), msg));
    }

    Ok(())
}

/// Parse the `rate_limit.per` duration and validate it.
pub fn parse_rate_limit_per(rl: &RateLimitMeta) -> syn::Result<Option<u64>> {
    match &rl.per {
        Some(per_str) => match crate::utils::parse_duration_secs(per_str) {
            Some(secs) => Ok(Some(secs)),
            None => Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                format!(
                    "invalid rate_limit per duration \"{per_str}\": use a duration like \"1m\", \"30s\", or \"1h\""
                ),
            )),
        },
        None => Ok(None),
    }
}

/// Reserved key names that aren't implemented yet. If darling sees these it will
/// parse them, but we reject post-parse.
pub fn reject_reserved(
    keys: &[&str],
    present: &[(&str, bool)],
    macro_name: &str,
) -> syn::Result<()> {
    for &(key, is_present) in present {
        if is_present && keys.contains(&key) {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                format!(
                    "Attribute `{key}` is reserved for a future Forge release and is not yet \
                     implemented. Remove it from #[{macro_name}] until support lands."
                ),
            ));
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use darling::FromMeta;

    fn parse_meta(src: &str) -> syn::Meta {
        syn::parse_str(src).expect("meta parses")
    }

    #[test]
    fn rate_limit_key_validator_accepts_documented_keys() {
        for k in ["user", "ip", "tenant", "global"] {
            assert!(validate_rate_limit_key(k).is_ok(), "should accept {k}");
        }
        // custom(...) prefix is the documented escape hatch — the inner
        // contents aren't inspected here, so just ensure the prefix passes.
        assert!(validate_rate_limit_key("custom(user_plus_tenant)").is_ok());
    }

    #[test]
    fn rate_limit_key_validator_rejects_unknown_and_lists_alternatives() {
        let err = validate_rate_limit_key("region").expect_err("unknown key rejected");
        // Error must surface the offending key AND list the valid options so
        // the user doesn't have to consult the docs to fix it.
        assert!(err.contains("region"), "missing offending key: {err}");
        for valid in ["user", "ip", "tenant", "global", "custom("] {
            assert!(
                err.contains(valid),
                "missing valid option {valid} in error: {err}"
            );
        }
    }

    #[test]
    fn rate_limit_validator_no_op_when_no_fields_present() {
        // Empty meta — nothing to validate, must not error or require fields.
        let rl = RateLimitMeta::default();
        assert!(validate_rate_limit(&rl).is_ok());
    }

    #[test]
    fn rate_limit_validator_requires_requests_when_any_field_present() {
        let rl = RateLimitMeta {
            requests: None,
            per: Some("1m".to_string()),
            key: None,
        };
        let err = validate_rate_limit(&rl).expect_err("missing requests rejected");
        assert!(err.to_string().contains("`requests`"));
    }

    #[test]
    fn rate_limit_validator_rejects_zero_requests() {
        let rl = RateLimitMeta {
            requests: Some(0),
            per: Some("1m".to_string()),
            key: None,
        };
        let err = validate_rate_limit(&rl).expect_err("zero requests rejected");
        assert!(err.to_string().contains("at least 1"));
    }

    #[test]
    fn rate_limit_validator_requires_per_when_requests_set() {
        let rl = RateLimitMeta {
            requests: Some(10),
            per: None,
            key: None,
        };
        let err = validate_rate_limit(&rl).expect_err("missing per rejected");
        assert!(err.to_string().contains("`per`"));
    }

    #[test]
    fn rate_limit_validator_rejects_invalid_key_when_present() {
        let rl = RateLimitMeta {
            requests: Some(10),
            per: Some("1m".to_string()),
            key: Some("nonsense".to_string()),
        };
        let err = validate_rate_limit(&rl).expect_err("invalid key rejected");
        assert!(err.to_string().contains("nonsense"));
    }

    #[test]
    fn rate_limit_validator_passes_complete_config() {
        let rl = RateLimitMeta {
            requests: Some(100),
            per: Some("1m".to_string()),
            key: Some("user".to_string()),
        };
        assert!(validate_rate_limit(&rl).is_ok());
    }

    #[test]
    fn parse_rate_limit_per_round_trips_known_units() {
        let rl = RateLimitMeta {
            requests: Some(1),
            per: Some("90s".to_string()),
            key: None,
        };
        assert_eq!(parse_rate_limit_per(&rl).unwrap(), Some(90));
    }

    #[test]
    fn parse_rate_limit_per_returns_none_when_unset() {
        let rl = RateLimitMeta::default();
        assert_eq!(parse_rate_limit_per(&rl).unwrap(), None);
    }

    #[test]
    fn parse_rate_limit_per_errors_on_bare_integer() {
        // Bare integers are rejected at the utils layer; the error message
        // must point the user toward a suffixed example.
        let rl = RateLimitMeta {
            requests: Some(1),
            per: Some("60".to_string()),
            key: None,
        };
        let err = parse_rate_limit_per(&rl).expect_err("bare integer rejected");
        let msg = err.to_string();
        assert!(msg.contains("invalid rate_limit per"), "got: {msg}");
        assert!(msg.contains("1m"), "should suggest suffixed form: {msg}");
    }

    #[test]
    fn require_role_parses_string_form() {
        // require_role = "admin" — handled by from_string.
        let role = RequireRole::from_string("admin").unwrap();
        assert_eq!(role.0, "admin");
    }

    #[test]
    fn require_role_rejects_non_string_literal_in_list() {
        // require_role(123) — not a string literal, must fail.
        let meta = parse_meta("require_role(123)");
        let err = RequireRole::from_meta(&meta).expect_err("integer literal rejected");
        assert!(err.to_string().contains("string literal"));
    }

    #[test]
    fn require_role_rejects_zero_or_multiple_args_in_list() {
        // Zero args: require_role() — caught by len() != 1.
        let meta = parse_meta("require_role()");
        let err = RequireRole::from_meta(&meta).expect_err("empty args rejected");
        assert!(err.to_string().contains("exactly one"));

        // Multiple args: require_role("a", "b") — same path.
        let meta = parse_meta("require_role(\"a\", \"b\")");
        let err = RequireRole::from_meta(&meta).expect_err("multiple args rejected");
        assert!(err.to_string().contains("exactly one"));
    }

    #[test]
    fn tables_list_rejects_legacy_array_syntax_with_migration_hint() {
        // The old `tables = ["a", "b"]` form must produce an actionable error
        // pointing at the new parenthesized form.
        let meta = parse_meta("tables = [\"users\", \"orders\"]");
        let err = TablesList::from_meta(&meta).expect_err("array syntax rejected");
        let msg = err.to_string();
        assert!(msg.contains("was removed"), "missing removal notice: {msg}");
        assert!(
            msg.contains("tables(\"foo\", \"bar\")"),
            "missing migration example: {msg}"
        );
    }

    #[test]
    fn tables_list_parses_parenthesized_form() {
        let meta = parse_meta("tables(\"users\", \"orders\")");
        let parsed = TablesList::from_meta(&meta).unwrap();
        assert_eq!(parsed.0, vec!["users", "orders"]);
    }

    #[test]
    fn tables_list_rejects_empty_parenthesized_form() {
        // tables() — would otherwise produce a vacuous list; reject so the
        // attribute is meaningful when present.
        let meta = parse_meta("tables()");
        let err = TablesList::from_meta(&meta).expect_err("empty list rejected");
        assert!(err.to_string().contains("must not be empty"));
    }

    #[test]
    fn tables_list_rejects_non_string_entries() {
        let meta = parse_meta("tables(123)");
        let err = TablesList::from_meta(&meta).expect_err("integer entry rejected");
        assert!(err.to_string().contains("string literals"));
    }

    #[test]
    fn tables_list_rejects_bare_path_attribute() {
        // Bare `tables` with no list form falls through to the catch-all.
        let meta = parse_meta("tables");
        let err = TablesList::from_meta(&meta).expect_err("bare path rejected");
        assert!(err.to_string().contains("parenthesized list"));
    }

    #[test]
    fn idempotent_meta_from_word_marks_enabled_without_key() {
        let i = IdempotentMeta::from_word().unwrap();
        assert!(i.enabled);
        assert!(i.key.is_none());
    }

    #[test]
    fn idempotent_meta_parses_key_in_list_form() {
        let meta = parse_meta("idempotent(key = \"order-id\")");
        let i = IdempotentMeta::from_meta(&meta).unwrap();
        assert!(i.enabled);
        assert_eq!(i.key.as_deref(), Some("order-id"));
    }

    #[test]
    fn reject_reserved_passes_when_no_reserved_keys_present() {
        // Caller asserts `cache` is not present — must succeed silently.
        let res = reject_reserved(&["cache"], &[("cache", false)], "query");
        assert!(res.is_ok());
    }

    #[test]
    fn reject_reserved_errors_with_macro_name_and_key_in_message() {
        // When a reserved key IS present, the error must name both the key
        // and the surrounding macro so the user knows where to look.
        let err = reject_reserved(&["webhook"], &[("webhook", true)], "mutation")
            .expect_err("reserved key rejected");
        let msg = err.to_string();
        assert!(msg.contains("webhook"), "missing key: {msg}");
        assert!(msg.contains("mutation"), "missing macro name: {msg}");
        assert!(msg.contains("reserved"), "missing reason: {msg}");
    }

    #[test]
    fn reject_reserved_ignores_non_reserved_keys_even_when_present() {
        // `other` is present but not in the reserved set — must pass.
        let res = reject_reserved(&["cache"], &[("other", true)], "query");
        assert!(res.is_ok());
    }
}

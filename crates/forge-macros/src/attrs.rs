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

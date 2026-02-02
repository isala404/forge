use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{ItemFn, parse_macro_input};

#[derive(Debug, Default)]
struct JobAttrs {
    name: Option<String>,
    timeout: Option<String>,
    priority: Option<String>,
    max_attempts: Option<u32>,
    backoff: Option<String>,
    max_backoff: Option<String>,
    worker_capability: Option<String>,
    idempotent: bool,
    idempotency_key: Option<String>,
    compensate: Option<String>,
    is_public: bool,
    required_role: Option<String>,
}

const VALID_PRIORITIES: &[&str] = &["background", "low", "normal", "high", "critical"];
const VALID_BACKOFFS: &[&str] = &["fixed", "linear", "exponential"];

fn parse_job_attrs(attr: TokenStream) -> syn::Result<JobAttrs> {
    let mut result = JobAttrs::default();
    let attr_str = attr.to_string();

    if attr_str.contains("public") {
        result.is_public = true;
    }

    // Parse idempotent (check for bare flag or with key)
    if let Some(idem_start) = attr_str.find("idempotent") {
        result.idempotent = true;
        // Check for idempotent(key = "...")
        if let Some(paren_start) = attr_str[idem_start..].find('(') {
            let remaining = &attr_str[idem_start + paren_start + 1..];
            if let Some(paren_end) = remaining.find(')') {
                let content = &remaining[..paren_end];
                if let Some(key_start) = content.find("key") {
                    if let Some(quote_start) = content[key_start..].find('"') {
                        let after_quote = &content[key_start + quote_start + 1..];
                        if let Some(quote_end) = after_quote.find('"') {
                            result.idempotency_key = Some(after_quote[..quote_end].to_string());
                        }
                    }
                }
            }
        }
    }

    // Parse require_role("admin")
    if let Some(role_start) = attr_str.find("require_role") {
        if let Some(paren_start) = attr_str[role_start..].find('(') {
            let remaining = &attr_str[role_start + paren_start + 1..];
            if let Some(paren_end) = remaining.find(')') {
                let role = remaining[..paren_end].trim().trim_matches('"');
                result.required_role = Some(role.to_string());
            }
        }
    }

    // Parse compensate = "handler_name"
    if let Some(comp_start) = attr_str.find("compensate") {
        if let Some(eq_pos) = attr_str[comp_start..].find('=') {
            let after_eq = &attr_str[comp_start + eq_pos + 1..];
            if let Some(quote_start) = after_eq.find('"') {
                let after_quote = &after_eq[quote_start + 1..];
                if let Some(quote_end) = after_quote.find('"') {
                    result.compensate = Some(after_quote[..quote_end].to_string());
                }
            }
        }
    }

    // Parse name = "custom_name"
    if let Some(name_start) = attr_str.find("name") {
        // Ensure it's not part of another word
        let before = if name_start > 0 {
            attr_str.chars().nth(name_start - 1)
        } else {
            None
        };
        if before.is_none() || !before.unwrap().is_alphanumeric() {
            if let Some(eq_pos) = attr_str[name_start..].find('=') {
                let after_eq = &attr_str[name_start + eq_pos + 1..];
                if let Some(quote_start) = after_eq.find('"') {
                    let after_quote = &after_eq[quote_start + 1..];
                    if let Some(quote_end) = after_quote.find('"') {
                        result.name = Some(after_quote[..quote_end].to_string());
                    }
                }
            }
        }
    }

    // Parse timeout = "30m"
    if let Some(timeout_start) = attr_str.find("timeout") {
        if let Some(eq_pos) = attr_str[timeout_start..].find('=') {
            let after_eq = &attr_str[timeout_start + eq_pos + 1..];
            if let Some(quote_start) = after_eq.find('"') {
                let after_quote = &after_eq[quote_start + 1..];
                if let Some(quote_end) = after_quote.find('"') {
                    result.timeout = Some(after_quote[..quote_end].to_string());
                }
            }
        }
    }

    // Parse priority = "high"
    if let Some(priority_start) = attr_str.find("priority") {
        if let Some(eq_pos) = attr_str[priority_start..].find('=') {
            let after_eq = &attr_str[priority_start + eq_pos + 1..];
            if let Some(quote_start) = after_eq.find('"') {
                let after_quote = &after_eq[quote_start + 1..];
                if let Some(quote_end) = after_quote.find('"') {
                    let priority = after_quote[..quote_end].to_string();
                    let priority_lower = priority.to_lowercase();
                    if !VALID_PRIORITIES.contains(&priority_lower.as_str()) {
                        return Err(syn::Error::new(
                            proc_macro2::Span::call_site(),
                            format!(
                                "Invalid job priority '{}'. Valid values: {}",
                                priority,
                                VALID_PRIORITIES.join(", ")
                            ),
                        ));
                    }
                    result.priority = Some(priority);
                }
            }
        }
    }

    // Parse worker_capability = "media"
    if let Some(cap_start) = attr_str.find("worker_capability") {
        if let Some(eq_pos) = attr_str[cap_start..].find('=') {
            let after_eq = &attr_str[cap_start + eq_pos + 1..];
            if let Some(quote_start) = after_eq.find('"') {
                let after_quote = &after_eq[quote_start + 1..];
                if let Some(quote_end) = after_quote.find('"') {
                    result.worker_capability = Some(after_quote[..quote_end].to_string());
                }
            }
        }
    }

    // Parse max_attempts = 3 (can be inside retry() or standalone)
    // First check for standalone max_attempts
    if let Some(ma_start) = attr_str.find("max_attempts") {
        if let Some(eq_pos) = attr_str[ma_start..].find('=') {
            let after_eq = &attr_str[ma_start + eq_pos + 1..];
            if let Ok(n) = after_eq
                .split(&[',', ')'])
                .next()
                .unwrap_or("")
                .trim()
                .parse::<u32>()
            {
                result.max_attempts = Some(n);
            }
        }
    }

    // Parse backoff = "exponential" (standalone)
    if let Some(backoff_start) = attr_str.find("backoff") {
        // Ensure it's not max_backoff
        let before = if backoff_start > 0 {
            attr_str.chars().nth(backoff_start - 1)
        } else {
            None
        };
        if before.is_none() || before.unwrap() != '_' {
            if let Some(eq_pos) = attr_str[backoff_start..].find('=') {
                let after_eq = &attr_str[backoff_start + eq_pos + 1..];
                if let Some(quote_start) = after_eq.find('"') {
                    let after_quote = &after_eq[quote_start + 1..];
                    if let Some(quote_end) = after_quote.find('"') {
                        let backoff = after_quote[..quote_end].to_string();
                        if !VALID_BACKOFFS.contains(&backoff.as_str()) {
                            return Err(syn::Error::new(
                                proc_macro2::Span::call_site(),
                                format!(
                                    "Invalid backoff strategy '{}'. Valid values: {}",
                                    backoff,
                                    VALID_BACKOFFS.join(", ")
                                ),
                            ));
                        }
                        result.backoff = Some(backoff);
                    }
                }
            }
        }
    }

    // Parse max_backoff = "5m" (standalone)
    if let Some(mb_start) = attr_str.find("max_backoff") {
        if let Some(eq_pos) = attr_str[mb_start..].find('=') {
            let after_eq = &attr_str[mb_start + eq_pos + 1..];
            if let Some(quote_start) = after_eq.find('"') {
                let after_quote = &after_eq[quote_start + 1..];
                if let Some(quote_end) = after_quote.find('"') {
                    result.max_backoff = Some(after_quote[..quote_end].to_string());
                }
            }
        }
    }

    // Parse retry(max_attempts = 3, backoff = "exponential", max_backoff = "5m")
    if let Some(retry_start) = attr_str.find("retry") {
        if let Some(paren_start) = attr_str[retry_start..].find('(') {
            let remaining = &attr_str[retry_start + paren_start + 1..];
            if let Some(paren_end) = remaining.find(')') {
                let content = &remaining[..paren_end];

                // Parse max_attempts inside retry()
                if let Some(ma_start) = content.find("max_attempts") {
                    if let Some(eq_pos) = content[ma_start..].find('=') {
                        let after_eq = &content[ma_start + eq_pos + 1..];
                        if let Ok(n) = after_eq
                            .split(',')
                            .next()
                            .unwrap_or("")
                            .trim()
                            .parse::<u32>()
                        {
                            result.max_attempts = Some(n);
                        }
                    }
                }

                // Parse backoff inside retry()
                if let Some(backoff_start) = content.find("backoff") {
                    let before = if backoff_start > 0 {
                        content.chars().nth(backoff_start - 1)
                    } else {
                        None
                    };
                    if before.is_none() || before.unwrap() != '_' {
                        if let Some(quote_start) = content[backoff_start..].find('"') {
                            let after_quote = &content[backoff_start + quote_start + 1..];
                            if let Some(quote_end) = after_quote.find('"') {
                                let backoff = after_quote[..quote_end].to_string();
                                if !VALID_BACKOFFS.contains(&backoff.as_str()) {
                                    return Err(syn::Error::new(
                                        proc_macro2::Span::call_site(),
                                        format!(
                                            "Invalid backoff strategy '{}'. Valid values: {}",
                                            backoff,
                                            VALID_BACKOFFS.join(", ")
                                        ),
                                    ));
                                }
                                result.backoff = Some(backoff);
                            }
                        }
                    }
                }

                // Parse max_backoff inside retry()
                if let Some(mb_start) = content.find("max_backoff") {
                    if let Some(quote_start) = content[mb_start..].find('"') {
                        let after_quote = &content[mb_start + quote_start + 1..];
                        if let Some(quote_end) = after_quote.find('"') {
                            result.max_backoff = Some(after_quote[..quote_end].to_string());
                        }
                    }
                }
            }
        }
    }

    Ok(result)
}

fn parse_duration(s: &str) -> proc_macro2::TokenStream {
    let s = s.trim();
    if s.ends_with("ms") {
        let n: u64 = s.trim_end_matches("ms").parse().unwrap_or(1000);
        quote! { std::time::Duration::from_millis(#n) }
    } else if s.ends_with('s') {
        let n: u64 = s.trim_end_matches('s').parse().unwrap_or(30);
        quote! { std::time::Duration::from_secs(#n) }
    } else if s.ends_with('m') {
        let n: u64 = s.trim_end_matches('m').parse().unwrap_or(5);
        let secs = n * 60;
        quote! { std::time::Duration::from_secs(#secs) }
    } else if s.ends_with('h') {
        let n: u64 = s.trim_end_matches('h').parse().unwrap_or(1);
        let secs = n * 3600;
        quote! { std::time::Duration::from_secs(#secs) }
    } else if s.ends_with('d') {
        let n: u64 = s.trim_end_matches('d').parse().unwrap_or(1);
        let secs = n * 86400;
        quote! { std::time::Duration::from_secs(#secs) }
    } else {
        let n: u64 = s.parse().unwrap_or(30);
        quote! { std::time::Duration::from_secs(#n) }
    }
}

pub fn job_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let attrs = match parse_job_attrs(attr) {
        Ok(attrs) => attrs,
        Err(e) => return e.to_compile_error().into(),
    };

    let fn_name = &input.sig.ident;
    let fn_name_str = attrs.name.unwrap_or_else(|| fn_name.to_string());
    let struct_name = format_ident!("{}Job", to_pascal_case(&fn_name.to_string()));

    let vis = &input.vis;
    let block = &input.block;

    // Parse context and args types from function signature
    let mut args_type = quote! { () };
    let mut args_ident = format_ident!("_args");

    for input_arg in input.sig.inputs.iter().skip(1) {
        if let syn::FnArg::Typed(pat_type) = input_arg {
            if let syn::Pat::Ident(ident) = pat_type.pat.as_ref() {
                args_ident = ident.ident.clone();
            }
            let ty = &pat_type.ty;
            args_type = quote! { #ty };
        }
    }

    // Parse return type
    let output_type = match &input.sig.output {
        syn::ReturnType::Default => quote! { () },
        syn::ReturnType::Type(_, ty) => {
            if let syn::Type::Path(path) = ty.as_ref() {
                if let Some(segment) = path.path.segments.last() {
                    if segment.ident == "Result" {
                        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                            if let Some(syn::GenericArgument::Type(inner)) = args.args.first() {
                                quote! { #inner }
                            } else {
                                quote! { () }
                            }
                        } else {
                            quote! { () }
                        }
                    } else {
                        quote! { #ty }
                    }
                } else {
                    quote! { #ty }
                }
            } else {
                quote! { #ty }
            }
        }
    };

    let timeout = if let Some(ref t) = attrs.timeout {
        parse_duration(t)
    } else {
        quote! { std::time::Duration::from_secs(3600) }
    };

    let priority = if let Some(ref p) = attrs.priority {
        let p_lower = p.to_lowercase();
        match p_lower.as_str() {
            "background" => quote! { forge::forge_core::job::JobPriority::Background },
            "low" => quote! { forge::forge_core::job::JobPriority::Low },
            "normal" => quote! { forge::forge_core::job::JobPriority::Normal },
            "high" => quote! { forge::forge_core::job::JobPriority::High },
            "critical" => quote! { forge::forge_core::job::JobPriority::Critical },
            _ => quote! { forge::forge_core::job::JobPriority::Normal },
        }
    } else {
        quote! { forge::forge_core::job::JobPriority::Normal }
    };

    let max_attempts = attrs.max_attempts.unwrap_or(3);
    let backoff = if let Some(ref b) = attrs.backoff {
        match b.as_str() {
            "fixed" => quote! { forge::forge_core::job::BackoffStrategy::Fixed },
            "linear" => quote! { forge::forge_core::job::BackoffStrategy::Linear },
            "exponential" => quote! { forge::forge_core::job::BackoffStrategy::Exponential },
            _ => quote! { forge::forge_core::job::BackoffStrategy::Exponential },
        }
    } else {
        quote! { forge::forge_core::job::BackoffStrategy::Exponential }
    };

    let max_backoff = if let Some(ref mb) = attrs.max_backoff {
        parse_duration(mb)
    } else {
        quote! { std::time::Duration::from_secs(300) }
    };

    let worker_capability = if let Some(ref cap) = attrs.worker_capability {
        quote! { Some(#cap) }
    } else {
        quote! { None }
    };

    let idempotent = attrs.idempotent;
    let idempotency_key = if let Some(ref key) = attrs.idempotency_key {
        quote! { Some(#key) }
    } else {
        quote! { None }
    };

    let is_public = attrs.is_public;
    let required_role = if let Some(ref role) = attrs.required_role {
        quote! { Some(#role) }
    } else {
        quote! { None }
    };

    let compensate = if let Some(ref handler) = attrs.compensate {
        let handler_ident = format_ident!("{}", handler);
        let compensation_args_ident = format_ident!("_comp_args");
        quote! {
            fn compensate(
                ctx: &forge::forge_core::job::JobContext,
                #compensation_args_ident: Self::Args,
                reason: &str,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = forge::forge_core::Result<()>> + Send + '_>> {
                Box::pin(async move { #handler_ident(ctx, #compensation_args_ident, reason).await })
            }
        }
    } else {
        quote! {}
    };

    let other_attrs = &input.attrs;

    let expanded = quote! {
        #(#other_attrs)*
        #vis struct #struct_name;

        impl forge::forge_core::job::ForgeJob for #struct_name {
            type Args = #args_type;
            type Output = #output_type;

            fn info() -> forge::forge_core::job::JobInfo {
                forge::forge_core::job::JobInfo {
                    name: #fn_name_str,
                    timeout: #timeout,
                    priority: #priority,
                    retry: forge::forge_core::job::RetryConfig {
                        max_attempts: #max_attempts,
                        backoff: #backoff,
                        max_backoff: #max_backoff,
                        retry_on: vec![],
                    },
                    worker_capability: #worker_capability,
                    idempotent: #idempotent,
                    idempotency_key: #idempotency_key,
                    is_public: #is_public,
                    required_role: #required_role,
                }
            }

            fn execute(
                ctx: &forge::forge_core::job::JobContext,
                #args_ident: Self::Args,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = forge::forge_core::Result<Self::Output>> + Send + '_>> {
                Box::pin(async move #block)
            }

            #compensate
        }
    };

    TokenStream::from(expanded)
}

fn to_pascal_case(s: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_pascal_case() {
        assert_eq!(to_pascal_case("send_welcome_email"), "SendWelcomeEmail");
        assert_eq!(to_pascal_case("process_video"), "ProcessVideo");
        assert_eq!(to_pascal_case("simple"), "Simple");
    }

    #[test]
    fn test_parse_duration_seconds() {
        let ts = parse_duration("30s");
        assert!(!ts.is_empty());
    }

    #[test]
    fn test_parse_duration_minutes() {
        let ts = parse_duration("5m");
        assert!(!ts.is_empty());
    }

    #[test]
    fn test_valid_priorities() {
        for p in VALID_PRIORITIES {
            assert!(VALID_PRIORITIES.contains(p), "{} should be valid", p);
        }
    }

    #[test]
    fn test_valid_backoffs() {
        for b in VALID_BACKOFFS {
            assert!(VALID_BACKOFFS.contains(b), "{} should be valid", b);
        }
    }

    #[test]
    fn test_invalid_priority_not_in_list() {
        assert!(!VALID_PRIORITIES.contains(&"invalid"));
        assert!(!VALID_PRIORITIES.contains(&"super"));
        assert!(!VALID_PRIORITIES.contains(&"urgent"));
    }

    #[test]
    fn test_invalid_backoff_not_in_list() {
        assert!(!VALID_BACKOFFS.contains(&"invalid"));
        assert!(!VALID_BACKOFFS.contains(&"random"));
        assert!(!VALID_BACKOFFS.contains(&"constant"));
    }
}

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse_macro_input, ItemFn, Meta};

/// Cron attributes.
#[derive(Debug, Default)]
struct CronAttrs {
    schedule: Option<String>,
    timezone: Option<String>,
    catch_up: bool,
    catch_up_limit: Option<u32>,
    timeout: Option<String>,
}

fn parse_cron_attrs(attrs: &[syn::Attribute], schedule_from_arg: Option<String>) -> CronAttrs {
    let mut result = CronAttrs {
        schedule: schedule_from_arg,
        ..Default::default()
    };

    for attr in attrs {
        if attr.path().is_ident("timezone") {
            if let Meta::NameValue(nv) = &attr.meta {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) = &nv.value
                {
                    result.timezone = Some(s.value());
                }
            }
        }
        if attr.path().is_ident("timeout") {
            if let Meta::NameValue(nv) = &attr.meta {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) = &nv.value
                {
                    result.timeout = Some(s.value());
                }
            }
        }
        if attr.path().is_ident("catch_up") {
            result.catch_up = true;
            if let Meta::NameValue(nv) = &attr.meta {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Int(i),
                    ..
                }) = &nv.value
                {
                    result.catch_up_limit = i.base10_parse().ok();
                }
            }
        }
        if attr.path().is_ident("catch_up_limit") {
            if let Meta::NameValue(nv) = &attr.meta {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Int(i),
                    ..
                }) = &nv.value
                {
                    result.catch_up_limit = i.base10_parse().ok();
                }
            }
        }
    }

    result
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
    } else {
        let n: u64 = s.parse().unwrap_or(3600);
        quote! { std::time::Duration::from_secs(#n) }
    }
}

pub fn cron_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);

    // Parse schedule from attribute argument
    let schedule_expr = if !attr.is_empty() {
        let attr_str = attr.to_string();
        // Remove quotes if present
        let cleaned = attr_str.trim().trim_matches('"');
        Some(cleaned.to_string())
    } else {
        None
    };

    let attrs = parse_cron_attrs(&input.attrs, schedule_expr);

    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let struct_name = format_ident!("{}Cron", to_pascal_case(&fn_name.to_string()));

    let vis = &input.vis;
    let block = &input.block;

    let schedule = attrs.schedule.unwrap_or_else(|| "* * * * *".to_string());
    let timezone = attrs.timezone.unwrap_or_else(|| "UTC".to_string());
    let catch_up = attrs.catch_up;
    let catch_up_limit = attrs.catch_up_limit.unwrap_or(10);

    let timeout = if let Some(ref t) = attrs.timeout {
        parse_duration(t)
    } else {
        quote! { std::time::Duration::from_secs(3600) }
    };

    // Filter out our custom attributes
    let other_attrs: Vec<_> = input
        .attrs
        .iter()
        .filter(|a| {
            !a.path().is_ident("timezone")
                && !a.path().is_ident("timeout")
                && !a.path().is_ident("catch_up")
                && !a.path().is_ident("catch_up_limit")
        })
        .collect();

    let expanded = quote! {
        #(#other_attrs)*
        #vis struct #struct_name;

        impl forge_core::cron::ForgeCron for #struct_name {
            fn info() -> forge_core::cron::CronInfo {
                forge_core::cron::CronInfo {
                    name: #fn_name_str,
                    schedule: forge_core::cron::CronSchedule::new(#schedule)
                        .expect("Invalid cron schedule"),
                    timezone: #timezone,
                    catch_up: #catch_up,
                    catch_up_limit: #catch_up_limit,
                    timeout: #timeout,
                }
            }

            fn execute(
                ctx: &forge_core::cron::CronContext,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = forge_core::Result<()>> + Send + '_>> {
                Box::pin(async move #block)
            }
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
        assert_eq!(to_pascal_case("daily_cleanup"), "DailyCleanup");
        assert_eq!(to_pascal_case("hourly_report"), "HourlyReport");
        assert_eq!(to_pascal_case("simple"), "Simple");
    }

    #[test]
    fn test_parse_duration_hours() {
        let ts = parse_duration("2h");
        assert!(!ts.is_empty());
    }

    #[test]
    fn test_parse_duration_minutes() {
        let ts = parse_duration("30m");
        assert!(!ts.is_empty());
    }
}

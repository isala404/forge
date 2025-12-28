use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse_macro_input, ItemFn, Meta};

/// Default attributes for job functions.
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
}

fn parse_job_attrs(attrs: &[syn::Attribute]) -> JobAttrs {
    let mut result = JobAttrs::default();

    for attr in attrs {
        if attr.path().is_ident("name") {
            if let Meta::NameValue(nv) = &attr.meta {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) = &nv.value
                {
                    result.name = Some(s.value());
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
        if attr.path().is_ident("priority") {
            if let Meta::NameValue(nv) = &attr.meta {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) = &nv.value
                {
                    result.priority = Some(s.value());
                }
            }
        }
        if attr.path().is_ident("max_attempts") {
            if let Meta::NameValue(nv) = &attr.meta {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Int(i),
                    ..
                }) = &nv.value
                {
                    result.max_attempts = i.base10_parse().ok();
                }
            }
        }
        if attr.path().is_ident("worker_capability") {
            if let Meta::NameValue(nv) = &attr.meta {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) = &nv.value
                {
                    result.worker_capability = Some(s.value());
                }
            }
        }
        if attr.path().is_ident("idempotent") {
            result.idempotent = true;
            if let Meta::List(list) = &attr.meta {
                let tokens = list.tokens.to_string();
                if tokens.starts_with("key") {
                    if let Some(key) = tokens.split('"').nth(1) {
                        result.idempotency_key = Some(key.to_string());
                    }
                }
            }
        }
        if attr.path().is_ident("retry") {
            if let Meta::List(list) = &attr.meta {
                let tokens = list.tokens.to_string();
                for part in tokens.split(',') {
                    let part = part.trim();
                    if part.starts_with("max_attempts") {
                        if let Some(val) = part.split('=').nth(1) {
                            result.max_attempts = val.trim().parse().ok();
                        }
                    }
                    if part.starts_with("backoff") {
                        if let Some(val) = part.split('"').nth(1) {
                            result.backoff = Some(val.to_string());
                        }
                    }
                    if part.starts_with("max_backoff") {
                        if let Some(val) = part.split('"').nth(1) {
                            result.max_backoff = Some(val.to_string());
                        }
                    }
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
        let n: u64 = s.parse().unwrap_or(30);
        quote! { std::time::Duration::from_secs(#n) }
    }
}

pub fn job_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    let _ = attr;
    let input = parse_macro_input!(item as ItemFn);
    let attrs = parse_job_attrs(&input.attrs);

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

    let other_attrs: Vec<_> = input
        .attrs
        .iter()
        .filter(|a| {
            !a.path().is_ident("name")
                && !a.path().is_ident("timeout")
                && !a.path().is_ident("priority")
                && !a.path().is_ident("max_attempts")
                && !a.path().is_ident("worker_capability")
                && !a.path().is_ident("idempotent")
                && !a.path().is_ident("retry")
        })
        .collect();

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
                }
            }

            fn execute(
                ctx: &forge::forge_core::job::JobContext,
                #args_ident: Self::Args,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = forge::forge_core::Result<Self::Output>> + Send + '_>> {
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
}

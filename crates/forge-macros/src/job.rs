use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{ItemFn, parse_macro_input};

use darling::FromMeta;
use darling::ast::NestedMeta;

use crate::attrs::{IdempotentMeta, RequireRole, RetryMeta, default_true, reject_reserved};
use crate::utils::{parse_duration_tokens, to_pascal_case};

/// Attribute keys whose names are reserved for upcoming job-deduplication and
/// per-key concurrency support. Using one today is a hard compile error so
/// apps don't think a feature works that doesn't.
const RESERVED_JOB_KEYS: &[&str] = &["unique_key", "concurrency_key", "concurrency_limit"];

const VALID_PRIORITIES: &[&str] = &["background", "low", "normal", "high", "critical"];
const VALID_BACKOFFS: &[&str] = &["fixed", "linear", "exponential"];

/// Darling-parsed job attributes.
#[derive(Debug, FromMeta)]
#[darling(and_then = DarlingJobAttrs::validate)]
struct DarlingJobAttrs {
    #[darling(default)]
    name: Option<String>,
    #[darling(default)]
    description: Option<String>,
    #[darling(default)]
    timeout: Option<String>,
    #[darling(default)]
    priority: Option<String>,
    #[darling(default)]
    max_attempts: Option<u32>,
    #[darling(default)]
    backoff: Option<String>,
    #[darling(default)]
    max_backoff: Option<String>,
    #[darling(default)]
    worker_capability: Option<String>,
    #[darling(default)]
    idempotent: Option<IdempotentMeta>,
    #[darling(default)]
    compensate: Option<String>,
    #[darling(default)]
    public: bool,
    #[darling(default)]
    require_role: Option<RequireRole>,
    #[darling(default)]
    ttl: Option<String>,
    #[darling(default)]
    retry: Option<RetryMeta>,
    /// Set `register = false` to skip `inventory::submit!` auto-registration.
    #[darling(default = "default_true")]
    register: bool,
    // Reserved keys
    #[darling(default)]
    unique_key: Option<String>,
    #[darling(default)]
    concurrency_key: Option<String>,
    #[darling(default)]
    concurrency_limit: Option<u32>,
}

impl DarlingJobAttrs {
    fn validate(self) -> darling::Result<Self> {
        reject_reserved(
            RESERVED_JOB_KEYS,
            &[
                ("unique_key", self.unique_key.is_some()),
                ("concurrency_key", self.concurrency_key.is_some()),
                ("concurrency_limit", self.concurrency_limit.is_some()),
            ],
            "job",
        )
        .map_err(|e| darling::Error::custom(e.to_string()))?;

        if let Some(ref p) = self.priority {
            let p_lower = p.to_lowercase();
            if !VALID_PRIORITIES.contains(&p_lower.as_str()) {
                return Err(darling::Error::custom(format!(
                    "Invalid job priority '{}'. Valid values: {}",
                    p,
                    VALID_PRIORITIES.join(", ")
                )));
            }
        }

        if let Some(ref b) = self.backoff
            && !VALID_BACKOFFS.contains(&b.as_str())
        {
            return Err(darling::Error::custom(format!(
                "Invalid backoff strategy '{}'. Valid values: {}",
                b,
                VALID_BACKOFFS.join(", ")
            )));
        }

        Ok(self)
    }
}

#[derive(Debug, Default)]
struct JobAttrs {
    name: Option<String>,
    description: Option<String>,
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
    ttl: Option<String>,
    register: bool,
}

pub fn job_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);

    let attr_args = match NestedMeta::parse_meta_list(attr.into()) {
        Ok(v) => v,
        Err(e) => return TokenStream::from(e.into_compile_error()),
    };

    let darling_attrs = match DarlingJobAttrs::from_list(&attr_args) {
        Ok(v) => v,
        Err(e) => return TokenStream::from(e.write_errors()),
    };

    let attrs = convert_job_attrs(darling_attrs);

    let fn_name = &input.sig.ident;
    let fn_name_str = attrs.name.unwrap_or_else(|| fn_name.to_string());

    // Reject job names starting with '$' — these are reserved for system jobs
    // ($cron:*, $workflow_resume, etc.) registered via `register_system`.
    if fn_name_str.starts_with('$') {
        return TokenStream::from(
            syn::Error::new(
                fn_name.span(),
                "job names starting with '$' are reserved for system jobs (e.g. $cron:*, $workflow_resume)",
            )
            .into_compile_error(),
        );
    }

    let module_name = format_ident!("__forge_handler_{}", fn_name);
    let struct_name = format_ident!("{}Job", to_pascal_case(&fn_name.to_string()));

    let _vis = &input.vis;
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

    let description_tokens = match &attrs.description {
        Some(d) => quote! { Some(#d) },
        None => quote! { None },
    };

    let timeout = if let Some(ref t) = attrs.timeout {
        parse_duration_tokens(t, 3600)
    } else {
        quote! { std::time::Duration::from_secs(3600) }
    };
    let http_timeout = if let Some(ref t) = attrs.timeout {
        let timeout = parse_duration_tokens(t, 0);
        quote! { Some(#timeout) }
    } else {
        quote! { None }
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
        parse_duration_tokens(mb, 300)
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

    let ttl = if let Some(ref t) = attrs.ttl {
        let duration = parse_duration_tokens(t, 3600);
        quote! { Some(#duration) }
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

    let registration = if attrs.register {
        quote! {
            forge::inventory::submit!(forge::AutoHandler(|registries| {
                registries.jobs.register::<#struct_name>();
            }));
        }
    } else {
        quote! {}
    };

    let expanded = quote! {
        #[doc(hidden)]
        #[allow(non_snake_case)]
        mod #module_name {
            use super::*;

            #(#other_attrs)*
            pub struct #struct_name;

            impl forge::forge_core::__sealed::Sealed for #struct_name {}

            impl forge::forge_core::job::ForgeJob for #struct_name {
                type Args = #args_type;
                type Output = #output_type;

                fn info() -> forge::forge_core::job::JobInfo {
                    forge::forge_core::job::JobInfo {
                        name: #fn_name_str,
                        description: #description_tokens,
                        timeout: #timeout,
                        http_timeout: #http_timeout,
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
                        ttl: #ttl,
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

            #registration
        }
    };

    TokenStream::from(expanded)
}

fn convert_job_attrs(darling: DarlingJobAttrs) -> JobAttrs {
    let (idempotent, idempotency_key) = match darling.idempotent {
        Some(idem) => (idem.enabled, idem.key),
        None => (false, None),
    };

    let mut max_attempts = darling.max_attempts;
    let mut backoff = darling.backoff;
    let mut max_backoff = darling.max_backoff;

    if let Some(retry) = darling.retry {
        if let Some(ma) = retry.max_attempts {
            max_attempts = Some(ma);
        }
        if let Some(b) = retry.backoff
            && VALID_BACKOFFS.contains(&b.as_str())
        {
            backoff = Some(b);
        }
        if let Some(mb) = retry.max_backoff {
            max_backoff = Some(mb);
        }
    }

    JobAttrs {
        name: darling.name,
        description: darling.description,
        timeout: darling.timeout,
        priority: darling.priority,
        max_attempts,
        backoff,
        max_backoff,
        worker_capability: darling.worker_capability,
        idempotent,
        idempotency_key,
        compensate: darling.compensate,
        is_public: darling.public,
        required_role: darling.require_role.map(|r| r.0),
        ttl: darling.ttl,
        register: darling.register,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests for to_pascal_case and parse_duration are in utils.rs (single source of truth).

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

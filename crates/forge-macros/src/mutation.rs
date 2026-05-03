use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{FnArg, ItemFn, Pat, ReturnType, Type, parse_macro_input};

use darling::FromMeta;
use darling::ast::NestedMeta;

use crate::attrs::{
    RateLimitMeta, RequireRole, TablesList, parse_rate_limit_per, reject_reserved,
    validate_rate_limit,
};
use crate::utils::{parse_duration_secs, parse_size_bytes, to_pascal_case};

/// Attribute keys whose names are reserved for upcoming mutation-coalescing
/// support (cursor positions, autosave, etc). Using one today is a hard
/// compile error to surface that the feature isn't wired up yet.
const RESERVED_MUTATION_KEYS: &[&str] = &["coalesce_window", "coalesce_by"];

/// Darling-parsed mutation attributes.
#[derive(Debug, FromMeta)]
#[darling(and_then = DarlingMutationAttrs::validate)]
#[allow(dead_code)]
struct DarlingMutationAttrs {
    #[darling(default)]
    name: Option<String>,
    #[darling(default)]
    description: Option<String>,
    /// Defaults to `true`. `transactional = false` opts out.
    #[darling(default)]
    transactional: Option<bool>,
    #[darling(default)]
    public: bool,
    #[darling(default)]
    unscoped: bool,
    #[darling(default)]
    require_role: Option<RequireRole>,
    #[darling(default)]
    timeout: Option<String>,
    #[darling(default)]
    rate_limit: Option<RateLimitMeta>,
    #[darling(default)]
    log: Option<String>,
    #[darling(default)]
    max_size: Option<String>,
    #[darling(default)]
    tables: Option<TablesList>,
    // Reserved keys
    #[darling(default)]
    coalesce_window: Option<String>,
    #[darling(default)]
    coalesce_by: Option<String>,
}

impl DarlingMutationAttrs {
    fn validate(self) -> darling::Result<Self> {
        reject_reserved(
            RESERVED_MUTATION_KEYS,
            &[
                ("coalesce_window", self.coalesce_window.is_some()),
                ("coalesce_by", self.coalesce_by.is_some()),
            ],
            "mutation",
        )
        .map_err(|e| darling::Error::custom(e.to_string()))?;
        Ok(self)
    }
}

struct MutationAttrs {
    /// Override the wire name (default: function name).
    name: Option<String>,
    description: Option<String>,
    required_role: Option<String>,
    is_public: bool,
    timeout: Option<u64>,
    rate_limit_requests: Option<u32>,
    rate_limit_per_secs: Option<u64>,
    rate_limit_key: Option<String>,
    log_level: Option<String>,
    /// Defaults to `true`. Opt out with `transactional = false` for
    /// high-throughput mutations that don't need atomicity.
    transactional: bool,
    max_upload_size_bytes: Option<usize>,
    /// Override auto-detected table dependencies from SQL extraction.
    tables: Option<Vec<String>>,
}

impl Default for MutationAttrs {
    fn default() -> Self {
        Self {
            name: None,
            description: None,
            required_role: None,
            is_public: false,
            timeout: None,
            rate_limit_requests: None,
            rate_limit_per_secs: None,
            rate_limit_key: None,
            log_level: None,
            transactional: true,
            max_upload_size_bytes: None,
            tables: None,
        }
    }
}

/// Expand the #[forge::mutation] attribute.
///
/// This transforms an async function into a mutation handler that:
/// - Takes a MutationContext as the first parameter
/// - Returns a Result<T>
/// - Runs within a database transaction
/// - Generates a struct implementing ForgeMutation trait
pub fn expand_mutation(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);

    let attr_args = match NestedMeta::parse_meta_list(attr.into()) {
        Ok(v) => v,
        Err(e) => return TokenStream::from(e.into_compile_error()),
    };

    let darling_attrs = match DarlingMutationAttrs::from_list(&attr_args) {
        Ok(v) => v,
        Err(e) => return TokenStream::from(e.write_errors()),
    };

    let attrs = match convert_mutation_attrs(darling_attrs) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error().into(),
    };

    expand_mutation_impl(input, attrs)
        .unwrap_or_else(|e| e.to_compile_error())
        .into()
}

fn convert_mutation_attrs(darling: DarlingMutationAttrs) -> Result<MutationAttrs, syn::Error> {
    let timeout = match darling.timeout {
        Some(ref s) => Some(parse_duration_secs(s).ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                format!("invalid timeout \"{s}\": use a duration string like \"30s\", \"5m\", or \"1h\""),
            )
        })?),
        None => None,
    };

    let (rate_limit_requests, rate_limit_per_secs, rate_limit_key) =
        if let Some(ref rl) = darling.rate_limit {
            validate_rate_limit(rl)?;
            (rl.requests, parse_rate_limit_per(rl)?, rl.key.clone())
        } else {
            (None, None, None)
        };

    Ok(MutationAttrs {
        name: darling.name,
        description: darling.description,
        required_role: darling.require_role.map(|r| r.0),
        is_public: darling.public,
        timeout,
        rate_limit_requests,
        rate_limit_per_secs,
        rate_limit_key,
        log_level: darling.log,
        transactional: darling.transactional.unwrap_or(true),
        max_upload_size_bytes: darling.max_size.and_then(|s| parse_size_bytes(&s)),
        tables: darling.tables.map(|t| t.0),
    })
}

fn expand_mutation_impl(input: ItemFn, attrs: MutationAttrs) -> syn::Result<TokenStream2> {
    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let rpc_name = attrs.name.as_deref().unwrap_or(&fn_name_str).to_string();
    let description = match &attrs.description {
        Some(d) => quote! { Some(#d) },
        None => quote! { None },
    };
    let module_name = syn::Ident::new(&format!("__forge_handler_{}", fn_name_str), fn_name.span());
    let struct_name = syn::Ident::new(
        &format!("{}Mutation", to_pascal_case(&fn_name_str)),
        fn_name.span(),
    );

    let vis = &input.vis;
    let asyncness = &input.sig.asyncness;
    let fn_block = &input.block;
    let fn_attrs = &input.attrs;

    // dispatch_job / start_workflow require a transaction so the outbox flush is
    // atomic with the database write. Explicitly opting out of transactions with
    // `transactional = false` while calling these is always a bug.
    let has_dispatch = {
        struct DispatchCallVisitor {
            found: bool,
        }
        impl<'ast> syn::visit::Visit<'ast> for DispatchCallVisitor {
            fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
                let method = node.method.to_string();
                if method == "dispatch_job"
                    || method == "dispatch_job_with_context"
                    || method == "start_workflow"
                {
                    self.found = true;
                }
                syn::visit::visit_expr_method_call(self, node);
            }
        }
        let mut visitor = DispatchCallVisitor { found: false };
        syn::visit::visit_block(&mut visitor, fn_block);
        visitor.found
    };
    if has_dispatch && !attrs.transactional {
        return Err(syn::Error::new_spanned(
            &input.sig.ident,
            "Mutations that call `dispatch_job()` or `start_workflow()` cannot use \
             `transactional = false` — jobs dispatched outside a transaction may \
             execute even when the database write is rolled back on error.",
        ));
    }

    // Validate async
    if asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &input.sig,
            "Mutation functions must be async",
        ));
    }

    // Extract parameters (skip first which should be &MutationContext)
    let params: Vec<_> = input.sig.inputs.iter().collect();
    if params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.sig,
            "Mutation functions must have at least a MutationContext parameter",
        ));
    }

    // Get context param - extract name and ensure it uses reference
    let (ctx_name, ctx_type) = match &params[0] {
        FnArg::Typed(pat_type) => {
            let name = if let Pat::Ident(pat_ident) = &*pat_type.pat {
                pat_ident.ident.clone()
            } else {
                return Err(syn::Error::new_spanned(
                    pat_type,
                    "Expected context parameter to be an identifier",
                ));
            };
            (name, &*pat_type.ty)
        }
        _ => {
            return Err(syn::Error::new_spanned(
                params[0],
                "Expected typed context parameter",
            ));
        }
    };

    // Determine the context type string (e.g., MutationContext)
    let type_str = quote! { #ctx_type }.to_string();
    let is_ref = type_str.starts_with('&');

    // Get remaining params for args struct
    let arg_params: Vec<_> = params.iter().skip(1).cloned().collect();

    // Build args struct fields
    let args_fields: Vec<TokenStream2> = arg_params
        .iter()
        .filter_map(|p| {
            if let FnArg::Typed(pat_type) = p
                && let Pat::Ident(pat_ident) = &*pat_type.pat
            {
                let name = &pat_ident.ident;
                let ty = &pat_type.ty;
                return Some(quote! { pub #name: #ty });
            }
            None
        })
        .collect();

    // Build destructuring for function call
    let arg_names: Vec<TokenStream2> = arg_params
        .iter()
        .filter_map(|p| {
            if let FnArg::Typed(pat_type) = p
                && let Pat::Ident(pat_ident) = &*pat_type.pat
            {
                let name = &pat_ident.ident;
                return Some(quote! { #name });
            }
            None
        })
        .collect();

    // Get return type
    let output_type = match &input.sig.output {
        ReturnType::Default => quote! { () },
        ReturnType::Type(_, ty) => {
            if let Type::Path(type_path) = &**ty {
                if let Some(segment) = type_path.path.segments.last() {
                    if segment.ident == "Result" {
                        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                            if let Some(syn::GenericArgument::Type(t)) = args.args.first() {
                                quote! { #t }
                            } else {
                                quote! { #ty }
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
            } else {
                quote! { #ty }
            }
        }
    };

    // Generate timeout option as Duration so all handler Info structs agree.
    let timeout = match attrs.timeout {
        Some(t) => quote! { Some(::std::time::Duration::from_secs(#t)) },
        None => quote! { None },
    };
    let http_timeout = timeout.clone();

    let required_role = match &attrs.required_role {
        Some(role) => quote! { Some(#role) },
        None => quote! { None },
    };

    let rate_limit_requests = match attrs.rate_limit_requests {
        Some(n) => quote! { Some(#n) },
        None => quote! { None },
    };

    let rate_limit_per_secs = match attrs.rate_limit_per_secs {
        Some(n) => quote! { Some(#n) },
        None => quote! { None },
    };

    let rate_limit_key = match &attrs.rate_limit_key {
        Some(k) => {
            let key_tokens = match k.as_str() {
                "user" => quote! { forge::forge_core::rate_limit::RateLimitKey::User },
                "ip" => quote! { forge::forge_core::rate_limit::RateLimitKey::Ip },
                "tenant" => quote! { forge::forge_core::rate_limit::RateLimitKey::Tenant },
                "user_action" => quote! { forge::forge_core::rate_limit::RateLimitKey::UserAction },
                "global" => quote! { forge::forge_core::rate_limit::RateLimitKey::Global },
                _ if k.starts_with("custom:") => {
                    let claim = k.trim_start_matches("custom:");
                    quote! { forge::forge_core::rate_limit::RateLimitKey::Custom(#claim.to_string()) }
                }
                _ => quote! { forge::forge_core::rate_limit::RateLimitKey::User },
            };
            quote! { Some(#key_tokens) }
        }
        None => quote! { None },
    };

    let log_level = match &attrs.log_level {
        Some(l) => {
            let level_tokens = match l.as_str() {
                "trace" => quote! { forge::forge_core::LogLevel::Trace },
                "debug" => quote! { forge::forge_core::LogLevel::Debug },
                "info" => quote! { forge::forge_core::LogLevel::Info },
                "warn" => quote! { forge::forge_core::LogLevel::Warn },
                "error" => quote! { forge::forge_core::LogLevel::Error },
                "off" => quote! { forge::forge_core::LogLevel::Off },
                _ => quote! { forge::forge_core::LogLevel::Trace },
            };
            quote! { Some(#level_tokens) }
        }
        None => quote! { None },
    };

    let max_upload_size_bytes = match attrs.max_upload_size_bytes {
        Some(n) => quote! { Some(#n) },
        None => quote! { None },
    };

    let table_deps_tokens = match &attrs.tables {
        Some(tables) => quote! { &[#(#tables),*] },
        None => quote! { &[] },
    };

    let transactional = attrs.transactional;
    let is_public = attrs.is_public;

    // A single non-primitive struct argument is passed through to the handler
    // as the args type directly. Primitives and collections get wrapped in a
    // generated #StructNameArgs struct so RPC payloads stay JSON-named.
    let single_custom_args_type: Option<&Type> = if arg_params.len() == 1 {
        if let FnArg::Typed(pat_type) = &arg_params[0] {
            if crate::utils::is_primitive_arg_type(&pat_type.ty) {
                None
            } else {
                Some(&*pat_type.ty)
            }
        } else {
            None
        }
    } else {
        None
    };

    let (module_struct_defs, args_type, execute_call) = if args_fields.is_empty() {
        (
            quote! { pub struct #struct_name; },
            quote! { () },
            quote! { super::#fn_name(ctx).await },
        )
    } else if let Some(user_args_type) = single_custom_args_type {
        (
            quote! { pub struct #struct_name; },
            quote! { #user_args_type },
            quote! { super::#fn_name(ctx, args).await },
        )
    } else {
        let args_struct_name = syn::Ident::new(&format!("{}Args", struct_name), fn_name.span());
        (
            quote! {
                #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
                pub struct #args_struct_name {
                    #(#args_fields),*
                }
                pub struct #struct_name;
            },
            quote! { #args_struct_name },
            quote! { super::#fn_name(ctx, #(args.#arg_names),*).await },
        )
    };

    // Generate the inner function - always take context by reference
    let inner_fn = if is_ref {
        // User already uses reference, keep the type as-is
        if arg_names.is_empty() {
            quote! {
                #(#fn_attrs)*
                #vis async fn #fn_name(#ctx_name: #ctx_type) -> forge::forge_core::Result<#output_type> #fn_block
            }
        } else {
            quote! {
                #(#fn_attrs)*
                #vis async fn #fn_name(#ctx_name: #ctx_type, #(#arg_params),*) -> forge::forge_core::Result<#output_type> #fn_block
            }
        }
    } else {
        // User uses value, convert to reference in the generated function
        if arg_names.is_empty() {
            quote! {
                #(#fn_attrs)*
                #vis async fn #fn_name(#ctx_name: &#ctx_type) -> forge::forge_core::Result<#output_type> #fn_block
            }
        } else {
            quote! {
                #(#fn_attrs)*
                #vis async fn #fn_name(#ctx_name: &#ctx_type, #(#arg_params),*) -> forge::forge_core::Result<#output_type> #fn_block
            }
        }
    };

    Ok(quote! {
        #inner_fn

        #[doc(hidden)]
        #[allow(non_snake_case)]
        mod #module_name {
            use super::*;

            #module_struct_defs

            impl forge::forge_core::__sealed::Sealed for #struct_name {}

            impl forge::forge_core::ForgeMutation for #struct_name {
                type Args = #args_type;
                type Output = #output_type;

                fn info() -> forge::forge_core::FunctionInfo {
                    forge::forge_core::FunctionInfo {
                        name: #rpc_name,
                        description: #description,
                        kind: forge::forge_core::FunctionKind::Mutation,
                        required_role: #required_role,
                        is_public: #is_public,
                        cache_ttl: None,
                        timeout: #timeout,
                        http_timeout: #http_timeout,
                        rate_limit_requests: #rate_limit_requests,
                        rate_limit_per_secs: #rate_limit_per_secs,
                        rate_limit_key: #rate_limit_key,
                        log_level: #log_level,
                        table_dependencies: #table_deps_tokens,
                        selected_columns: &[],
                        transactional: #transactional,
                        consistent: false,
                        max_upload_size_bytes: #max_upload_size_bytes,
                    }
                }

                fn execute(
                    ctx: &forge::forge_core::MutationContext,
                    args: Self::Args,
                ) -> std::pin::Pin<Box<dyn std::future::Future<Output = forge::forge_core::Result<Self::Output>> + Send + '_>> {
                    Box::pin(async move {
                        #execute_call
                    })
                }
            }

            forge::inventory::submit!(forge::AutoHandler(|registries| {
                registries.functions.register_mutation::<#struct_name>();
            }));
        }
    })
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    // Note: proc_macro::TokenStream cannot be created outside the compiler bridge,
    // so we test parse_mutation_attrs indirectly and test expand_mutation_impl directly
    // with syn::ItemFn + MutationAttrs.

    // --- MutationAttrs default ---

    #[test]
    fn default_attrs_transactional_is_true() {
        let attrs = MutationAttrs::default();
        assert!(attrs.transactional, "transactional defaults to true");
        assert!(!attrs.is_public);
        assert!(attrs.required_role.is_none());
        assert!(attrs.timeout.is_none());
        assert!(attrs.rate_limit_requests.is_none());
        assert!(attrs.rate_limit_per_secs.is_none());
        assert!(attrs.rate_limit_key.is_none());
        assert!(attrs.log_level.is_none());
        assert!(attrs.max_upload_size_bytes.is_none());
        assert!(attrs.tables.is_none());
    }

    // --- Validation: transactional requirement ---

    #[test]
    fn rejects_dispatch_job_with_transactional_false() {
        let input: ItemFn = syn::parse_str(
            r#"
            pub async fn create_user(ctx: &MutationContext, name: String) -> Result<User> {
                ctx.dispatch_job("send_email", json!({})).await?;
                Ok(User { name })
            }
            "#,
        )
        .unwrap();

        let attrs = MutationAttrs {
            transactional: false,
            ..MutationAttrs::default()
        };
        let result = expand_mutation_impl(input, attrs);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("transactional"),
            "Error should mention transactional: {err_msg}"
        );
    }

    #[test]
    fn rejects_start_workflow_with_transactional_false() {
        let input: ItemFn = syn::parse_str(
            r#"
            pub async fn begin_onboarding(ctx: &MutationContext) -> Result<()> {
                ctx.start_workflow("onboarding", json!({})).await?;
                Ok(())
            }
            "#,
        )
        .unwrap();

        let attrs = MutationAttrs {
            transactional: false,
            ..MutationAttrs::default()
        };
        let result = expand_mutation_impl(input, attrs);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("transactional"));
    }

    #[test]
    fn accepts_dispatch_job_with_default_transactional() {
        let input: ItemFn = syn::parse_str(
            r#"
            pub async fn create_user(ctx: &MutationContext, name: String) -> Result<User> {
                ctx.dispatch_job("send_email", json!({})).await?;
                Ok(User { name })
            }
            "#,
        )
        .unwrap();

        // Default is transactional = true, so dispatch_job is fine.
        let attrs = MutationAttrs::default();
        let result = expand_mutation_impl(input, attrs);
        assert!(
            result.is_ok(),
            "Should accept dispatch_job with default transactional=true"
        );
    }

    #[test]
    fn accepts_dispatch_job_with_transactional() {
        let input: ItemFn = syn::parse_str(
            r#"
            pub async fn create_user(ctx: &MutationContext, name: String) -> Result<User> {
                ctx.dispatch_job("send_email", json!({})).await?;
                Ok(User { name })
            }
            "#,
        )
        .unwrap();

        let attrs = MutationAttrs {
            transactional: true,
            ..Default::default()
        };
        let result = expand_mutation_impl(input, attrs);
        assert!(
            result.is_ok(),
            "Should accept dispatch_job with transactional"
        );
    }

    #[test]
    fn accepts_mutation_without_dispatch() {
        let input: ItemFn = syn::parse_str(
            r#"
            pub async fn update_name(ctx: &MutationContext, name: String) -> Result<()> {
                Ok(())
            }
            "#,
        )
        .unwrap();

        let attrs = MutationAttrs::default();
        let result = expand_mutation_impl(input, attrs);
        assert!(
            result.is_ok(),
            "Simple mutation without dispatch should work"
        );
    }

    // --- Validation: async requirement ---

    #[test]
    fn rejects_non_async_mutation() {
        let input: ItemFn = syn::parse_str(
            r#"
            pub fn create_user(ctx: &MutationContext) -> Result<()> {
                Ok(())
            }
            "#,
        )
        .unwrap();

        let attrs = MutationAttrs::default();
        let result = expand_mutation_impl(input, attrs);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("async"),
            "Error should mention async: {err_msg}"
        );
    }

    // --- Validation: context parameter ---

    #[test]
    fn rejects_mutation_without_parameters() {
        let input: ItemFn = syn::parse_str(
            r#"
            pub async fn create_user() -> Result<()> {
                Ok(())
            }
            "#,
        )
        .unwrap();

        let attrs = MutationAttrs::default();
        let result = expand_mutation_impl(input, attrs);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(
            err_msg.contains("MutationContext"),
            "Error should mention context param: {err_msg}"
        );
    }

    // --- Generated output structure ---

    #[test]
    fn generates_struct_for_no_arg_mutation() {
        let input: ItemFn = syn::parse_str(
            r#"
            pub async fn reset_all(ctx: &MutationContext) -> Result<()> {
                Ok(())
            }
            "#,
        )
        .unwrap();

        let attrs = MutationAttrs::default();
        let output = expand_mutation_impl(input, attrs).expect("should expand");
        let output_str = output.to_string();
        assert!(
            output_str.contains("ResetAllMutation"),
            "Should generate PascalCase struct name"
        );
        assert!(
            output_str.contains("ForgeMutation"),
            "Should implement ForgeMutation trait"
        );
        assert!(
            output_str.contains("inventory"),
            "Should register via inventory"
        );
    }

    #[test]
    fn generates_info_with_attributes() {
        let input: ItemFn = syn::parse_str(
            r#"
            pub async fn create_item(ctx: &MutationContext) -> Result<()> {
                Ok(())
            }
            "#,
        )
        .unwrap();

        let attrs = MutationAttrs {
            is_public: true,
            transactional: true,
            required_role: Some("admin".into()),
            ..Default::default()
        };
        let output = expand_mutation_impl(input, attrs).expect("should expand");
        let output_str = output.to_string();
        assert!(output_str.contains("is_public : true"));
        assert!(output_str.contains("transactional : true"));
        assert!(
            output_str.contains(r#"Some ("admin")"#) || output_str.contains(r#"Some("admin")"#)
        );
    }

    #[test]
    fn generates_explicit_table_dependencies() {
        let input: ItemFn = syn::parse_str(
            r#"
            pub async fn create_order(ctx: &MutationContext) -> Result<()> {
                Ok(())
            }
            "#,
        )
        .unwrap();

        let attrs = MutationAttrs {
            tables: Some(vec!["users".into(), "orders".into()]),
            ..Default::default()
        };
        let output = expand_mutation_impl(input, attrs).expect("should expand");
        let output_str = output.to_string();
        assert!(
            output_str.contains("users") && output_str.contains("orders"),
            "Should include explicit table dependencies in output: {output_str}"
        );
    }

    #[test]
    fn generates_empty_table_dependencies_by_default() {
        let input: ItemFn = syn::parse_str(
            r#"
            pub async fn update_user(ctx: &MutationContext) -> Result<()> {
                Ok(())
            }
            "#,
        )
        .unwrap();

        let attrs = MutationAttrs::default();
        let output = expand_mutation_impl(input, attrs).expect("should expand");
        let output_str = output.to_string();
        assert!(
            output_str.contains("table_dependencies : & []"),
            "Should have empty table_dependencies by default: {output_str}"
        );
    }
}

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::visit::Visit;
use syn::{FnArg, ItemFn, Pat, ReturnType, Type, parse_macro_input};

use darling::FromMeta;
use darling::ast::NestedMeta;

use crate::attrs::{
    RateLimitMeta, RequireRole, TablesList, default_true, parse_rate_limit_per, reject_reserved,
    validate_rate_limit,
};
use crate::sql_extractor::{
    DbDelegationDetector, ScopeCheckResult, SqlStringExtractor, TableExtractionResult,
    extract_columns_from_sql, extract_tables_from_sql, sql_references_identity_scope,
    sql_scope_requires_tenant,
};
use crate::utils::{parse_duration_secs, to_pascal_case};

/// Attribute keys whose names are reserved for upcoming reactor and
/// result-guardrail features. Using one today is a hard compile error
/// to surface that the feature isn't actually wired up yet.
const RESERVED_QUERY_KEYS: &[&str] = &[
    "debounce_ms",
    "max_debounce_ms",
    "reexecute_timeout",
    "max_rows",
    "max_bytes",
];

/// Darling-parsed query attributes.
#[derive(Debug, FromMeta)]
#[darling(and_then = DarlingQueryAttrs::validate)]
struct DarlingQueryAttrs {
    /// Override the wire name (default: function name).
    #[darling(default)]
    name: Option<String>,
    /// Human-readable description surfaced in metadata and docs.
    #[darling(default)]
    description: Option<String>,
    #[darling(default)]
    cache: Option<String>,
    #[darling(default)]
    require_role: Option<RequireRole>,
    #[darling(default)]
    public: bool,
    #[darling(default)]
    unscoped: bool,
    /// New-style alias for `public`. Accepted values: "none", "required".
    #[darling(default)]
    auth: Option<String>,
    /// New-style alias for `unscoped`. Accepted values: "global", "user".
    #[darling(default)]
    scope: Option<String>,
    #[darling(default)]
    consistent: bool,
    /// Set `register = false` to skip `inventory::submit!` auto-registration.
    #[darling(default = "default_true")]
    register: bool,
    #[darling(default)]
    timeout: Option<String>,
    #[darling(default)]
    rate_limit: Option<RateLimitMeta>,
    #[darling(default)]
    log: Option<String>,
    /// Explicitly specified table dependencies (override for dynamic SQL).
    #[darling(default)]
    tables: Option<TablesList>,
    // Reserved keys - parsed but rejected at validation time.
    #[darling(default)]
    debounce_ms: Option<u32>,
    #[darling(default)]
    max_debounce_ms: Option<u32>,
    #[darling(default)]
    reexecute_timeout: Option<String>,
    #[darling(default)]
    max_rows: Option<u32>,
    #[darling(default)]
    max_bytes: Option<String>,
}

impl DarlingQueryAttrs {
    fn validate(self) -> darling::Result<Self> {
        reject_reserved(
            RESERVED_QUERY_KEYS,
            &[
                ("debounce_ms", self.debounce_ms.is_some()),
                ("max_debounce_ms", self.max_debounce_ms.is_some()),
                ("reexecute_timeout", self.reexecute_timeout.is_some()),
                ("max_rows", self.max_rows.is_some()),
                ("max_bytes", self.max_bytes.is_some()),
            ],
            "query",
        )
        .map_err(|e| darling::Error::custom(e.to_string()))?;

        if let Some(ref a) = self.auth
            && !["none", "required"].contains(&a.as_str())
        {
            return Err(darling::Error::custom(format!(
                "invalid auth value \"{a}\": expected \"none\" or \"required\""
            )));
        }

        if let Some(ref s) = self.scope
            && !["global", "user"].contains(&s.as_str())
        {
            return Err(darling::Error::custom(format!(
                "invalid scope value \"{s}\": expected \"global\" or \"user\""
            )));
        }

        Ok(self)
    }
}

#[derive(Default)]
struct QueryAttrs {
    /// Override the wire name (default: function name).
    name: Option<String>,
    /// Human-readable description surfaced in metadata and docs.
    description: Option<String>,
    cache_ttl: Option<u64>,
    required_role: Option<String>,
    is_public: bool,
    is_unscoped: bool,
    consistent: bool,
    timeout: Option<u64>,
    rate_limit_requests: Option<u32>,
    rate_limit_per_secs: Option<u64>,
    rate_limit_key: Option<String>,
    log_level: Option<String>,
    /// Explicitly specified table dependencies (override for dynamic SQL).
    tables: Option<Vec<String>>,
    register: bool,
}

/// Expand the #[forge::query] attribute.
///
/// This transforms an async function into a query handler that:
/// - Takes a QueryContext as the first parameter
/// - Returns a Result<T>
/// - Generates a struct implementing ForgeQuery trait
pub fn expand_query(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);

    let attr_args = match NestedMeta::parse_meta_list(attr.into()) {
        Ok(v) => v,
        Err(e) => return TokenStream::from(e.into_compile_error()),
    };

    let darling_attrs = match DarlingQueryAttrs::from_list(&attr_args) {
        Ok(v) => v,
        Err(e) => return TokenStream::from(e.write_errors()),
    };

    let attrs = match convert_query_attrs(darling_attrs) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error().into(),
    };

    expand_query_impl(input, attrs)
        .unwrap_or_else(|e| e.to_compile_error())
        .into()
}

fn convert_query_attrs(darling: DarlingQueryAttrs) -> Result<QueryAttrs, syn::Error> {
    let cache_ttl = match darling.cache {
        Some(ref s) => Some(parse_duration_secs(s).ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                format!("invalid cache duration \"{s}\": use a duration string like \"30s\", \"5m\", or \"1h\""),
            )
        })?),
        None => None,
    };

    let timeout = match darling.timeout {
        Some(ref s) => Some(parse_duration_secs(s).ok_or_else(|| {
            syn::Error::new(
                proc_macro2::Span::call_site(),
                format!(
                    "invalid timeout \"{s}\": use a duration string like \"30s\", \"5m\", or \"1h\""
                ),
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

    Ok(QueryAttrs {
        name: darling.name,
        description: darling.description,
        cache_ttl,
        required_role: darling.require_role.map(|r| r.0),
        is_public: darling.public || darling.auth.as_deref() == Some("none"),
        is_unscoped: darling.unscoped || darling.scope.as_deref() == Some("global"),
        consistent: darling.consistent,
        timeout,
        rate_limit_requests,
        rate_limit_per_secs,
        rate_limit_key,
        log_level: darling.log,
        tables: darling.tables.map(|t| t.0),
        register: darling.register,
    })
}

fn expand_query_impl(input: ItemFn, attrs: QueryAttrs) -> syn::Result<TokenStream2> {
    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let rpc_name = attrs.name.as_deref().unwrap_or(&fn_name_str).to_string();
    let module_name = syn::Ident::new(&format!("__forge_handler_{}", fn_name_str), fn_name.span());
    let struct_name = syn::Ident::new(
        &format!("{}Query", to_pascal_case(&fn_name_str)),
        fn_name.span(),
    );

    let vis = &input.vis;
    let asyncness = &input.sig.asyncness;
    let fn_block = &input.block;
    let fn_attrs = &input.attrs;

    // Validate async
    if asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &input.sig,
            "Query functions must be async",
        ));
    }

    // Extract parameters (skip first which should be &QueryContext)
    let params: Vec<_> = input.sig.inputs.iter().collect();
    if params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.sig,
            "Query functions must have at least a QueryContext parameter",
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

    // Determine the context type string
    let type_str = quote! { #ctx_type }.to_string();
    let is_ref = type_str.starts_with('&');

    // Extract table dependencies from function body or use explicit override
    let has_explicit_tables = attrs.tables.is_some();
    let table_dependencies: Vec<String> = if let Some(explicit_tables) = attrs.tables {
        explicit_tables
    } else {
        let mut extractor = SqlStringExtractor::new();
        extractor.visit_block(fn_block);

        match extract_tables_from_sql(&extractor.sql_strings) {
            TableExtractionResult::Ok(tables) => {
                let mut sorted: Vec<String> = tables.into_iter().collect();
                sorted.sort();
                sorted
            }
            TableExtractionResult::ParseFailed(sql) => {
                let preview: String = sql.chars().take(80).collect();
                return Err(syn::Error::new_spanned(
                    &input.sig.ident,
                    format!(
                        "SQL in `{fn_name_str}` could not be parsed: \"{preview}...\"\n\
                         Add #[query(tables(\"your_table\"))] to specify table dependencies explicitly."
                    ),
                ));
            }
        }
    };

    // Extract selected columns from SQL
    let selected_columns: Vec<String> = {
        let mut extractor = SqlStringExtractor::new();
        extractor.visit_block(fn_block);
        let cols = extract_columns_from_sql(&extractor.sql_strings);
        let mut sorted: Vec<String> = cols.into_iter().collect();
        sorted.sort();
        sorted
    };

    // Detect DB delegation: if no SQL was found but the body calls .pool(),
    // a helper function is doing the DB work and bypassing scope extraction.
    if !attrs.is_public
        && !attrs.is_unscoped
        && table_dependencies.is_empty()
        && !has_explicit_tables
    {
        let mut delegation = DbDelegationDetector::new();
        delegation.visit_block(fn_block);
        if delegation.found {
            return Err(syn::Error::new_spanned(
                &input.sig.ident,
                format!(
                    "Private query `{fn_name_str}` calls .pool() but contains no inline SQL, so \
                     table dependencies and scope cannot be verified. Inline the SQL in the handler \
                     body, or add #[query(tables(\"...\"))] to declare dependencies explicitly."
                ),
            ));
        }
    }

    // Compile-time scope check: private queries that touch tables must filter by user identity.
    if !attrs.is_public
        && !attrs.is_unscoped
        && !table_dependencies.is_empty()
    {
        let mut scope_extractor = SqlStringExtractor::new();
        scope_extractor.visit_block(fn_block);
        match sql_references_identity_scope(&scope_extractor.sql_strings) {
            ScopeCheckResult::Scoped => {}
            ScopeCheckResult::Unscoped => {
                let tables_str = table_dependencies.join(", ");
                return Err(syn::Error::new_spanned(
                    &input.sig.ident,
                    format!(
                        "Private query `{fn_name_str}` references table(s) [{tables_str}] but SQL \
                         does not filter by user_id or owner_id. Add a WHERE clause scoped to the \
                         authenticated user, or use #[query(scope = \"global\")] if this is intentional."
                    ),
                ));
            }
            ScopeCheckResult::ParseFailed => {
                let tables_str = table_dependencies.join(", ");
                return Err(syn::Error::new_spanned(
                    &input.sig.ident,
                    format!(
                        "Private query `{fn_name_str}` references table(s) [{tables_str}] but SQL \
                         could not be parsed to verify scope. Add #[query(scope = \"global\")] to opt out of \
                         scope checking, or add #[query(tables(\"...\"))] to skip automatic extraction."
                    ),
                ));
            }
        }
    }

    // Detect tenant_id scope for runtime enforcement.
    let requires_tenant_scope = if !attrs.is_public && !attrs.is_unscoped {
        let mut tenant_extractor = SqlStringExtractor::new();
        tenant_extractor.visit_block(fn_block);
        sql_scope_requires_tenant(&tenant_extractor.sql_strings)
    } else {
        false
    };

    // Get remaining params for args struct
    let arg_params: Vec<_> = params.iter().skip(1).cloned().collect();

    // Reject argument types that codegen cannot emit bindings for.
    for p in &arg_params {
        if let FnArg::Typed(pat_type) = p
            && let Some((reason, span)) = crate::utils::check_arg_wire_type(&pat_type.ty)
        {
            return Err(syn::Error::new(span, reason));
        }
    }

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
            // Extract T from Result<T> or Result<T, E>
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

    // Generate cache_ttl option
    let cache_ttl = match attrs.cache_ttl {
        Some(ttl) => quote! { Some(#ttl) },
        None => quote! { None },
    };

    // Generate timeout option as Duration so all handler Info structs agree.
    let timeout = match attrs.timeout {
        Some(t) => quote! { Some(::std::time::Duration::from_secs(#t)) },
        None => quote! { None },
    };
    let http_timeout = timeout.clone();

    let description = match &attrs.description {
        Some(d) => quote! { Some(#d) },
        None => quote! { None },
    };

    let is_public = attrs.is_public;
    let consistent = attrs.consistent;

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

    // Generate table_dependencies token
    let table_deps_tokens = if table_dependencies.is_empty() {
        quote! { &[] }
    } else {
        let table_strs: Vec<_> = table_dependencies.iter().map(|t| quote! { #t }).collect();
        quote! { &[#(#table_strs),*] }
    };

    // Generate selected_columns token
    let selected_cols_tokens = if selected_columns.is_empty() {
        quote! { &[] }
    } else {
        let col_strs: Vec<_> = selected_columns.iter().map(|c| quote! { #c }).collect();
        quote! { &[#(#col_strs),*] }
    };

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

    // Generate handler struct definitions and execute call for the hidden module.
    // The struct and its args live in a private per-handler module; the original function
    // stays at the parent level and is called via super::.
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

    let registration = if attrs.register {
        quote! {
            forge::inventory::submit!(forge::AutoHandler(|registries| {
                registries.functions.register_query::<#struct_name>();
            }));
        }
    } else {
        quote! {}
    };

    Ok(quote! {
        #inner_fn

        #[doc(hidden)]
        #[allow(non_snake_case)]
        mod #module_name {
            use super::*;

            #module_struct_defs

            impl forge::forge_core::__sealed::Sealed for #struct_name {}

            impl forge::forge_core::ForgeQuery for #struct_name {
                type Args = #args_type;
                type Output = #output_type;

                fn info() -> forge::forge_core::FunctionInfo {
                    forge::forge_core::FunctionInfo {
                        name: #rpc_name,
                        description: #description,
                        kind: forge::forge_core::FunctionKind::Query,
                        required_role: #required_role,
                        is_public: #is_public,
                        cache_ttl: #cache_ttl,
                        timeout: #timeout,
                        http_timeout: #http_timeout,
                        rate_limit_requests: #rate_limit_requests,
                        rate_limit_per_secs: #rate_limit_per_secs,
                        rate_limit_key: #rate_limit_key,
                        log_level: #log_level,
                        table_dependencies: #table_deps_tokens,
                        selected_columns: #selected_cols_tokens,
                        changed_columns: &[],
                        transactional: false,
                        consistent: #consistent,
                        max_upload_size_bytes: None,
                        requires_tenant_scope: #requires_tenant_scope,
                    }
                }

                fn execute(
                    ctx: &forge::forge_core::QueryContext,
                    args: Self::Args,
                ) -> std::pin::Pin<Box<dyn std::future::Future<Output = forge::forge_core::Result<Self::Output>> + Send + '_>> {
                    Box::pin(async move {
                        #execute_call
                    })
                }
            }

            #registration
        }
    })
}

// Tests for to_pascal_case and parse_duration are in utils.rs (single source of truth).

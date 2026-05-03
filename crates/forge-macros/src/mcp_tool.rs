use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{FnArg, ItemFn, Pat, ReturnType, Type, parse_macro_input};

use darling::FromMeta;
use darling::ast::NestedMeta;

use crate::attrs::{
    RateLimitMeta, RequireRole, parse_rate_limit_per, validate_rate_limit,
    validate_rate_limit_key,
};
use crate::utils::{parse_duration_secs, to_pascal_case};

/// Darling-parsed MCP tool attributes.
#[derive(Debug, FromMeta)]
struct DarlingMcpToolAttrs {
    #[darling(default)]
    public: bool,
    #[darling(default)]
    read_only: bool,
    #[darling(default)]
    destructive: bool,
    #[darling(default)]
    idempotent: bool,
    #[darling(default)]
    open_world: bool,
    #[darling(default)]
    require_role: Option<RequireRole>,
    #[darling(default)]
    name: Option<String>,
    #[darling(default)]
    title: Option<String>,
    #[darling(default)]
    description: Option<String>,
    #[darling(default)]
    timeout: Option<String>,
    #[darling(default)]
    rate_limit: Option<RateLimitMeta>,
}

/// Expand the #[forge::mcp_tool] attribute.
pub fn expand_mcp_tool(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);

    let attr_args = match NestedMeta::parse_meta_list(attr.into()) {
        Ok(v) => v,
        Err(e) => return TokenStream::from(e.into_compile_error()),
    };

    let darling_attrs = match DarlingMcpToolAttrs::from_list(&attr_args) {
        Ok(v) => v,
        Err(e) => return TokenStream::from(e.write_errors()),
    };

    let attrs = match convert_mcp_tool_attrs(darling_attrs) {
        Ok(a) => a,
        Err(e) => return e.to_compile_error().into(),
    };

    expand_mcp_tool_impl(input, attrs)
        .unwrap_or_else(|e| e.to_compile_error())
        .into()
}

#[derive(Default)]
struct McpToolAttrs {
    name: Option<String>,
    title: Option<String>,
    description: Option<String>,
    required_role: Option<String>,
    is_public: bool,
    timeout: Option<u64>,
    rate_limit_requests: Option<u32>,
    rate_limit_per_secs: Option<u64>,
    rate_limit_key: Option<String>,
    read_only_hint: Option<bool>,
    destructive_hint: Option<bool>,
    idempotent_hint: Option<bool>,
    open_world_hint: Option<bool>,
}

fn convert_mcp_tool_attrs(darling: DarlingMcpToolAttrs) -> Result<McpToolAttrs, syn::Error> {
    let timeout = darling.timeout.and_then(|s| {
        parse_duration_secs(&s).or_else(|| s.parse::<u64>().ok())
    });

    let (rate_limit_requests, rate_limit_per_secs, rate_limit_key) =
        if let Some(ref rl) = darling.rate_limit {
            validate_rate_limit(rl)?;
            let per = parse_rate_limit_per(rl)?;
            if let Some(ref key) = rl.key
                && let Err(msg) = validate_rate_limit_key(key)
            {
                return Err(syn::Error::new(proc_macro2::Span::call_site(), msg));
            }
            (rl.requests, per, rl.key.clone())
        } else {
            (None, None, None)
        };

    Ok(McpToolAttrs {
        name: darling.name,
        title: darling.title,
        description: darling.description,
        required_role: darling.require_role.map(|r| r.0),
        is_public: darling.public,
        timeout,
        rate_limit_requests,
        rate_limit_per_secs,
        rate_limit_key,
        read_only_hint: if darling.read_only { Some(true) } else { None },
        destructive_hint: if darling.destructive { Some(true) } else { None },
        idempotent_hint: if darling.idempotent { Some(true) } else { None },
        open_world_hint: if darling.open_world { Some(true) } else { None },
    })
}

fn validate_tool_name(name: &str) -> syn::Result<()> {
    if name.is_empty() || name.len() > 128 {
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "MCP tool names must be 1-128 characters long",
        ));
    }

    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.'))
    {
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "MCP tool names may only contain ASCII letters, numbers, '_', '-', and '.'",
        ));
    }

    Ok(())
}

fn is_schema_field_attr(attr: &syn::Attribute) -> bool {
    let path = attr.path();
    path.is_ident("schemars") || path.is_ident("serde") || path.is_ident("doc")
}

fn tool_type_stem(fn_name: &str) -> &str {
    fn_name
        .strip_suffix("_mcp_tool")
        .or_else(|| fn_name.strip_suffix("_tool"))
        .filter(|stem| !stem.is_empty())
        .unwrap_or(fn_name)
}

fn expand_mcp_tool_impl(input: ItemFn, attrs: McpToolAttrs) -> syn::Result<TokenStream2> {
    let fn_name = &input.sig.ident;
    let fn_name_str = attrs.name.unwrap_or_else(|| fn_name.to_string());
    validate_tool_name(&fn_name_str)?;

    let fn_name_value = fn_name.to_string();
    let module_name = syn::Ident::new(
        &format!("__forge_handler_{}", fn_name_value),
        fn_name.span(),
    );
    let struct_name = syn::Ident::new(
        &format!("{}McpTool", to_pascal_case(tool_type_stem(&fn_name_value))),
        fn_name.span(),
    );
    let vis = &input.vis;
    let asyncness = &input.sig.asyncness;
    let fn_block = &input.block;
    let fn_attrs = &input.attrs;

    if asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &input.sig,
            "MCP tool functions must be async",
        ));
    }

    let params: Vec<_> = input.sig.inputs.iter().collect();
    if params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.sig,
            "MCP tool functions must have at least a McpToolContext parameter",
        ));
    }

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

    let type_str = quote! { #ctx_type }.to_string();
    let is_ref = type_str.starts_with('&');

    let arg_params: Vec<_> = params.iter().skip(1).cloned().collect();

    let args_fields: Vec<TokenStream2> = arg_params
        .iter()
        .filter_map(|p| {
            if let FnArg::Typed(pat_type) = p
                && let Pat::Ident(pat_ident) = &*pat_type.pat
            {
                let name = &pat_ident.ident;
                let ty = &pat_type.ty;
                let field_attrs: Vec<_> = pat_type
                    .attrs
                    .iter()
                    .filter(|attr| is_schema_field_attr(attr))
                    .collect();
                return Some(quote! {
                    #(#field_attrs)*
                    pub #name: #ty
                });
            }
            None
        })
        .collect();

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

    let timeout = match attrs.timeout {
        Some(t) => quote! { Some(std::time::Duration::from_secs(#t)) },
        None => quote! { None },
    };

    let required_role = match &attrs.required_role {
        Some(role) => quote! { Some(#role) },
        None => quote! { None },
    };

    let title = match &attrs.title {
        Some(t) => quote! { Some(#t) },
        None => quote! { None },
    };

    let description = match &attrs.description {
        Some(d) => quote! { Some(#d) },
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
        Some(k) => quote! { Some(#k) },
        None => quote! { None },
    };

    let read_only_hint = match attrs.read_only_hint {
        Some(v) => quote! { Some(#v) },
        None => quote! { None },
    };
    let destructive_hint = match attrs.destructive_hint {
        Some(v) => quote! { Some(#v) },
        None => quote! { None },
    };
    let idempotent_hint = match attrs.idempotent_hint {
        Some(v) => quote! { Some(#v) },
        None => quote! { None },
    };
    let open_world_hint = match attrs.open_world_hint {
        Some(v) => quote! { Some(#v) },
        None => quote! { None },
    };

    let is_public = attrs.is_public;

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

    let (module_struct_defs, args_type, execute_call) = if arg_params.is_empty() {
        let args_struct_name = syn::Ident::new(&format!("{}Args", struct_name), fn_name.span());
        (
            quote! {
                #[derive(Debug, Clone, serde::Deserialize, forge::forge_core::schemars::JsonSchema)]
                #[schemars(crate = "forge::forge_core::schemars")]
                pub struct #args_struct_name {}
                pub struct #struct_name;
            },
            quote! { #args_struct_name },
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
                #[derive(Debug, Clone, serde::Deserialize, forge::forge_core::schemars::JsonSchema)]
                #[schemars(crate = "forge::forge_core::schemars")]
                pub struct #args_struct_name {
                    #(#args_fields),*
                }
                pub struct #struct_name;
            },
            quote! { #args_struct_name },
            quote! { super::#fn_name(ctx, #(args.#arg_names),*).await },
        )
    };

    let inner_fn = if is_ref {
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
    } else if arg_names.is_empty() {
        quote! {
            #(#fn_attrs)*
            #vis async fn #fn_name(#ctx_name: &#ctx_type) -> forge::forge_core::Result<#output_type> #fn_block
        }
    } else {
        quote! {
            #(#fn_attrs)*
            #vis async fn #fn_name(#ctx_name: &#ctx_type, #(#arg_params),*) -> forge::forge_core::Result<#output_type> #fn_block
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

            impl forge::forge_core::ForgeMcpTool for #struct_name {
                type Args = #args_type;
                type Output = #output_type;

                fn info() -> forge::forge_core::McpToolInfo {
                    forge::forge_core::McpToolInfo {
                        name: #fn_name_str,
                        title: #title,
                        description: #description,
                        required_role: #required_role,
                        is_public: #is_public,
                        timeout: #timeout,
                        rate_limit_requests: #rate_limit_requests,
                        rate_limit_per_secs: #rate_limit_per_secs,
                        rate_limit_key: #rate_limit_key,
                        annotations: forge::forge_core::McpToolAnnotations {
                            title: #title,
                            read_only_hint: #read_only_hint,
                            destructive_hint: #destructive_hint,
                            idempotent_hint: #idempotent_hint,
                            open_world_hint: #open_world_hint,
                        },
                        icons: &[],
                    }
                }

                fn execute(
                    ctx: &forge::forge_core::McpToolContext,
                    args: Self::Args,
                ) -> std::pin::Pin<Box<dyn std::future::Future<Output = forge::forge_core::Result<Self::Output>> + Send + '_>> {
                    Box::pin(async move {
                        #execute_call
                    })
                }
            }

            forge::inventory::submit!(forge::AutoHandler(|registries| {
                registries.mcp_tools.register::<#struct_name>();
            }));
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_validate_tool_name_accepts_valid_names() {
        assert!(validate_tool_name("get_user").is_ok());
        assert!(validate_tool_name("admin.tools.list").is_ok());
        assert!(validate_tool_name("DATA_EXPORT_v2").is_ok());
    }

    #[test]
    fn test_validate_tool_name_rejects_invalid_names() {
        assert!(validate_tool_name("").is_err());
        assert!(validate_tool_name("with space").is_err());
        assert!(validate_tool_name("weird,comma").is_err());
    }

    #[test]
    fn test_generated_args_preserve_schema_field_attributes() {
        let input: ItemFn = syn::parse_quote! {
            pub async fn describe_weather(
                ctx: &McpToolContext,
                #[schemars(description = "City name or zip code", length(min = 1))]
                location: String,
                #[serde(default)]
                unit: Option<String>,
            ) -> forge::forge_core::Result<String> {
                Ok(format!("{}:{:?}", location, unit))
            }
        };

        let expanded =
            expand_mcp_tool_impl(input, McpToolAttrs::default()).expect("macro expansion succeeds");
        let tokens = expanded.to_string();

        assert!(tokens.contains("schemars"));
        assert!(tokens.contains("City name or zip code"));
        assert!(tokens.contains("serde"));
        assert!(tokens.contains("pub location : String"));
        assert!(tokens.contains("pub unit : Option < String >"));
    }

    #[test]
    fn test_tool_struct_name_strips_redundant_tool_suffix() {
        assert_eq!(tool_type_stem("export_project_tool"), "export_project");
        assert_eq!(tool_type_stem("sync_users_mcp_tool"), "sync_users");
        assert_eq!(tool_type_stem("lookup"), "lookup");
    }
}

use proc_macro::TokenStream;
use proc_macro2::TokenStream as TokenStream2;
use quote::quote;
use syn::{parse_macro_input, FnArg, ItemFn, Pat, ReturnType, Type};

/// Expand the #[forge::action] attribute.
///
/// This transforms an async function into an action handler that:
/// - Takes an ActionContext as the first parameter
/// - Returns a Result<T>
/// - Can call external APIs
/// - Generates a struct implementing ForgeAction trait
pub fn expand_action(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let attrs = parse_action_attrs(attr);

    expand_action_impl(input, attrs)
        .unwrap_or_else(|e| e.to_compile_error())
        .into()
}

#[derive(Default)]
struct ActionAttrs {
    requires_auth: bool,
    required_role: Option<String>,
    timeout: Option<u64>,
    rate_limit_requests: Option<u32>,
    rate_limit_per: Option<String>,
    rate_limit_key: Option<String>,
    retry_max_attempts: Option<u32>,
    retry_backoff: Option<String>,
}

fn parse_action_attrs(attr: TokenStream) -> ActionAttrs {
    let mut attrs = ActionAttrs::default();

    let attr_str = attr.to_string();

    if attr_str.contains("require_auth") {
        attrs.requires_auth = true;
    }

    // Parse role requirement
    if let Some(role_start) = attr_str.find("require_role") {
        if let Some(paren_start) = attr_str[role_start..].find('(') {
            let remaining = &attr_str[role_start + paren_start + 1..];
            if let Some(paren_end) = remaining.find(')') {
                let role = remaining[..paren_end].trim().trim_matches('"');
                attrs.required_role = Some(role.to_string());
                attrs.requires_auth = true;
            }
        }
    }

    // Parse timeout
    if let Some(timeout_start) = attr_str.find("timeout") {
        if let Some(eq_pos) = attr_str[timeout_start..].find('=') {
            let remaining = &attr_str[timeout_start + eq_pos + 1..];
            let trimmed = remaining.trim();
            if let Ok(secs) = trimmed
                .split(&[',', ')'])
                .next()
                .unwrap_or("")
                .trim()
                .parse::<u64>()
            {
                attrs.timeout = Some(secs);
            }
        }
    }

    attrs
}

fn expand_action_impl(input: ItemFn, attrs: ActionAttrs) -> syn::Result<TokenStream2> {
    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let struct_name = syn::Ident::new(
        &format!("{}Action", to_pascal_case(&fn_name_str)),
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
            "Action functions must be async",
        ));
    }

    // Extract parameters (skip first which should be &ActionContext)
    let params: Vec<_> = input.sig.inputs.iter().collect();
    if params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.sig,
            "Action functions must have at least an ActionContext parameter",
        ));
    }

    // Get context param
    let ctx_param = &params[0];

    // Get remaining params for args struct
    let arg_params: Vec<_> = params.iter().skip(1).cloned().collect();

    // Build args struct fields
    let args_fields: Vec<TokenStream2> = arg_params
        .iter()
        .filter_map(|p| {
            if let FnArg::Typed(pat_type) = p {
                if let Pat::Ident(pat_ident) = &*pat_type.pat {
                    let name = &pat_ident.ident;
                    let ty = &pat_type.ty;
                    return Some(quote! { pub #name: #ty });
                }
            }
            None
        })
        .collect();

    // Build destructuring for function call
    let arg_names: Vec<TokenStream2> = arg_params
        .iter()
        .filter_map(|p| {
            if let FnArg::Typed(pat_type) = p {
                if let Pat::Ident(pat_ident) = &*pat_type.pat {
                    let name = &pat_ident.ident;
                    return Some(quote! { #name });
                }
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

    // Generate timeout option
    let timeout = match attrs.timeout {
        Some(t) => quote! { Some(#t) },
        None => quote! { None },
    };

    let requires_auth = attrs.requires_auth;

    let required_role = match &attrs.required_role {
        Some(role) => quote! { Some(#role) },
        None => quote! { None },
    };

    // Generate the args struct (use unit type if no args)
    let args_struct = if args_fields.is_empty() {
        quote! {
            #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
            #vis struct #struct_name;

            impl #struct_name {
                type Args = ();
            }
        }
    } else {
        let args_struct_name = syn::Ident::new(&format!("{}Args", struct_name), fn_name.span());
        quote! {
            #[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
            #vis struct #args_struct_name {
                #(#args_fields),*
            }

            #vis struct #struct_name;
        }
    };

    // Generate the inner function
    let inner_fn = if arg_names.is_empty() {
        quote! {
            #(#fn_attrs)*
            #vis async fn #fn_name(#ctx_param) -> forge_core::Result<#output_type> #fn_block
        }
    } else {
        quote! {
            #(#fn_attrs)*
            #vis async fn #fn_name(#ctx_param, #(#arg_params),*) -> forge_core::Result<#output_type> #fn_block
        }
    };

    // Generate the ForgeAction implementation
    let args_type = if args_fields.is_empty() {
        quote! { () }
    } else {
        let args_struct_name = syn::Ident::new(&format!("{}Args", struct_name), fn_name.span());
        quote! { #args_struct_name }
    };

    let execute_call = if arg_names.is_empty() {
        quote! { #fn_name(ctx).await }
    } else {
        quote! { #fn_name(ctx, #(args.#arg_names),*).await }
    };

    Ok(quote! {
        #args_struct

        #inner_fn

        impl forge_core::ForgeAction for #struct_name {
            type Args = #args_type;
            type Output = #output_type;

            fn info() -> forge_core::FunctionInfo {
                forge_core::FunctionInfo {
                    name: #fn_name_str,
                    description: None,
                    kind: forge_core::FunctionKind::Action,
                    requires_auth: #requires_auth,
                    required_role: #required_role,
                    is_public: false,
                    cache_ttl: None,
                    timeout: #timeout,
                }
            }

            fn execute(
                ctx: &forge_core::ActionContext,
                args: Self::Args,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = forge_core::Result<Self::Output>> + Send + '_>> {
                Box::pin(async move {
                    #execute_call
                })
            }
        }
    })
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

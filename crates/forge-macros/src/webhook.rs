use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{ItemFn, parse_macro_input};

use darling::FromMeta;
use darling::ast::NestedMeta;

use crate::utils::{parse_duration_tokens, to_pascal_case};

/// Darling-parsed webhook attributes.
///
/// Note: `signature` is NOT parsed by darling because its value is a Rust
/// expression (`WebhookSignature::hmac_sha256("Header", "secret_env")`), not
/// a meta item. We extract it manually from the raw token stream.
#[derive(Debug, Default, FromMeta)]
struct DarlingWebhookAttrs {
    /// Override the registry name (default: function name).
    #[darling(default)]
    name: Option<String>,
    #[darling(default)]
    description: Option<String>,
    #[darling(default)]
    path: Option<String>,
    #[darling(default)]
    allow_unsigned: bool,
    #[darling(default)]
    idempotency: Option<String>,
    #[darling(default)]
    timeout: Option<String>,
    // `signature` is handled manually - darling will see it as unknown, so we
    // parse the raw token stream for it before handing to darling.
}

#[derive(Debug, Default)]
struct WebhookAttrs {
    /// Override the registry name (default: function name).
    name: Option<String>,
    description: Option<String>,
    path: Option<String>,
    signature_algorithm: Option<String>,
    signature_header: Option<String>,
    signature_secret_env: Option<String>,
    allow_unsigned: bool,
    idempotency: Option<String>,
    timeout: Option<String>,
}

/// Parse the `signature = WebhookSignature::...` attribute manually from the
/// raw attribute string. This is a Rust expression, not a meta item.
fn parse_signature_from_attr_str(attr_str: &str) -> WebhookSignatureInfo {
    let mut info = WebhookSignatureInfo::default();

    let Some(sig_start) = attr_str.find("signature") else {
        return info;
    };
    let remaining = &attr_str[sig_start..];

    if remaining.contains("hmac_sha256") {
        info.algorithm = Some("HmacSha256".to_string());
    } else if remaining.contains("stripe_webhooks") {
        info.algorithm = Some("StripeWebhooks".to_string());
    } else if remaining.contains("shopify_webhooks") {
        info.algorithm = Some("HmacSha256Base64".to_string());
    } else if remaining.contains("ed25519") {
        info.algorithm = Some("Ed25519".to_string());
    }

    if let Some(paren_start) = remaining.find('(') {
        let inside_parens = &remaining[paren_start + 1..];

        let mut depth = 1;
        let mut end_pos = 0;
        for (i, c) in inside_parens.char_indices() {
            match c {
                '(' => depth += 1,
                ')' => {
                    depth -= 1;
                    if depth == 0 {
                        end_pos = i;
                        break;
                    }
                }
                _ => {}
            }
        }

        let args_str = &inside_parens[..end_pos];

        let quotes: Vec<_> = args_str.match_indices('"').collect();
        // Single-arg variants: secret only, header is hardcoded per spec
        let single_arg_header = match info.algorithm.as_deref() {
            Some("StripeWebhooks") => Some("stripe-signature"),
            Some("HmacSha256Base64") => Some("x-shopify-hmac-sha256"),
            _ => None,
        };
        if let Some(fixed_header) = single_arg_header {
            if quotes.len() >= 2 {
                let secret_start = quotes[0].0 + 1;
                let secret_end = quotes[1].0;
                info.secret_env = Some(args_str[secret_start..secret_end].to_string());
                info.header = Some(fixed_header.to_string());
            }
        } else if quotes.len() >= 4 {
            // Two-arg variants: header name then secret/public-key env
            let header_start = quotes[0].0 + 1;
            let header_end = quotes[1].0;
            info.header = Some(args_str[header_start..header_end].to_string());

            let secret_start = quotes[2].0 + 1;
            let secret_end = quotes[3].0;
            info.secret_env = Some(args_str[secret_start..secret_end].to_string());
        }
    }

    info
}

#[derive(Debug, Default)]
struct WebhookSignatureInfo {
    algorithm: Option<String>,
    header: Option<String>,
    secret_env: Option<String>,
}

pub fn webhook_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);

    // Parse the raw attribute string for the signature expression before darling
    let attr_str = attr.to_string();
    let sig_info = parse_signature_from_attr_str(&attr_str);

    // Filter out the `signature = ...` meta item from the list since darling
    // can't parse Rust expressions. We need to handle it carefully: the
    // signature value contains `::` and `(...)` which makes it tricky.
    // Strategy: parse with NestedMeta, then filter out unknown items darling
    // would reject, and let the manual parser handle them.
    let attr_args = match NestedMeta::parse_meta_list(attr.into()) {
        Ok(v) => v,
        Err(e) => return TokenStream::from(e.into_compile_error()),
    };

    // Filter out `signature = <expr>` since it's a Rust expression darling can't handle
    let filtered_args: Vec<NestedMeta> = attr_args
        .into_iter()
        .filter(|meta| {
            if let NestedMeta::Meta(syn::Meta::NameValue(nv)) = meta {
                !nv.path.is_ident("signature")
            } else {
                true
            }
        })
        .collect();

    let darling_attrs = match DarlingWebhookAttrs::from_list(&filtered_args) {
        Ok(v) => v,
        Err(e) => return TokenStream::from(e.write_errors()),
    };

    let attrs = WebhookAttrs {
        name: darling_attrs.name,
        description: darling_attrs.description,
        path: darling_attrs.path,
        signature_algorithm: sig_info.algorithm,
        signature_header: sig_info.header,
        signature_secret_env: sig_info.secret_env,
        allow_unsigned: darling_attrs.allow_unsigned,
        idempotency: darling_attrs.idempotency,
        timeout: darling_attrs.timeout,
    };

    // Validate path
    match &attrs.path {
        None => {
            return syn::Error::new(
                proc_macro2::Span::call_site(),
                "webhook requires path attribute",
            )
            .to_compile_error()
            .into();
        }
        Some(p) if p.trim().is_empty() || !p.starts_with('/') => {
            return syn::Error::new(
                proc_macro2::Span::call_site(),
                "webhook path must start with '/' (example: path = \"/webhooks/stripe\")",
            )
            .to_compile_error()
            .into();
        }
        _ => {}
    }

    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let rpc_name = attrs.name.as_deref().unwrap_or(&fn_name_str).to_string();
    let module_name = format_ident!("__forge_handler_{}", fn_name);
    let struct_name = format_ident!("{}Webhook", to_pascal_case(&fn_name.to_string()));

    let _vis = &input.vis;
    let block = &input.block;

    let payload_type = input
        .sig
        .inputs
        .iter()
        .nth(1)
        .and_then(|arg| {
            if let syn::FnArg::Typed(pat_type) = arg {
                Some(pat_type.ty.clone())
            } else {
                None
            }
        })
        .unwrap_or_else(|| syn::parse_quote!(serde_json::Value));

    let path = attrs.path.unwrap_or_else(|| "/webhooks".to_string());
    let allow_unsigned = attrs.allow_unsigned;

    let description_tokens = match &attrs.description {
        Some(d) => quote! { Some(#d) },
        None => quote! { None },
    };

    let timeout = if let Some(ref t) = attrs.timeout {
        parse_duration_tokens(t, 30)
    } else {
        quote! { std::time::Duration::from_secs(30) }
    };
    let http_timeout = if let Some(ref t) = attrs.timeout {
        let timeout = parse_duration_tokens(t, 0);
        quote! { Some(#timeout) }
    } else {
        quote! { None }
    };

    let signature = if let (Some(alg), Some(header), Some(secret_env)) = (
        &attrs.signature_algorithm,
        &attrs.signature_header,
        &attrs.signature_secret_env,
    ) {
        let alg_token = match alg.as_str() {
            "HmacSha256" => quote! { forge::forge_core::webhook::SignatureAlgorithm::HmacSha256 },
            "StripeWebhooks" => {
                quote! { forge::forge_core::webhook::SignatureAlgorithm::StripeWebhooks }
            }
            "HmacSha256Base64" => {
                quote! { forge::forge_core::webhook::SignatureAlgorithm::HmacSha256Base64 }
            }
            "Ed25519" => quote! { forge::forge_core::webhook::SignatureAlgorithm::Ed25519 },
            _ => quote! { forge::forge_core::webhook::SignatureAlgorithm::HmacSha256 },
        };
        quote! {
            Some(forge::forge_core::webhook::SignatureConfig {
                algorithm: #alg_token,
                header_name: #header,
                secret_env: #secret_env,
            })
        }
    } else {
        quote! { None }
    };

    let idempotency = if let Some(ref idem) = attrs.idempotency {
        if let Some((prefix, value)) = idem.split_once(':') {
            match prefix {
                "header" => {
                    quote! {
                        Some(forge::forge_core::webhook::IdempotencyConfig::new(
                            forge::forge_core::webhook::IdempotencySource::Header(#value)
                        ))
                    }
                }
                "body" => {
                    quote! {
                        Some(forge::forge_core::webhook::IdempotencyConfig::new(
                            forge::forge_core::webhook::IdempotencySource::Body(#value)
                        ))
                    }
                }
                _ => quote! { None },
            }
        } else {
            quote! { None }
        }
    } else {
        quote! { None }
    };

    let other_attrs = &input.attrs;

    let expanded = quote! {
        #[doc(hidden)]
        #[allow(non_snake_case)]
        mod #module_name {
            use super::*;

            #(#other_attrs)*
            pub struct #struct_name;

            impl forge::forge_core::__sealed::Sealed for #struct_name {}

            impl forge::forge_core::webhook::ForgeWebhook for #struct_name {
                type Payload = #payload_type;

                fn info() -> forge::forge_core::webhook::WebhookInfo {
                    forge::forge_core::webhook::WebhookInfo {
                        name: #rpc_name,
                        description: #description_tokens,
                        path: #path,
                        signature: #signature,
                        allow_unsigned: #allow_unsigned,
                        idempotency: #idempotency,
                        timeout: #timeout,
                        http_timeout: #http_timeout,
                    }
                }

                fn execute(
                    ctx: &forge::forge_core::webhook::WebhookContext,
                    payload: #payload_type,
                ) -> std::pin::Pin<Box<dyn std::future::Future<Output = forge::forge_core::Result<forge::forge_core::webhook::WebhookResult>> + Send + '_>> {
                    Box::pin(async move #block)
                }
            }

            forge::inventory::submit!(forge::AutoHandler(|registries| {
                registries.webhooks.register::<#struct_name>();
                registries.functions.register_webhook_info(
                    forge::forge_core::FunctionInfo::from(&#struct_name::info())
                );
            }));
        }
    };

    TokenStream::from(expanded)
}

// Tests for to_pascal_case and parse_duration are in utils.rs (single source of truth).

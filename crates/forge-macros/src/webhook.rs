use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{ItemFn, parse_macro_input};

use crate::utils::{
    has_attr_flag, parse_attr_value, parse_duration_tokens, to_pascal_case, validate_attr_keys,
};

const ALLOWED_WEBHOOK_KEYS: &[&str] = &[
    "name",
    "description",
    "path",
    "signature",
    "allow_unsigned",
    "idempotency",
    "timeout",
];

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

fn parse_webhook_attrs(attr: TokenStream) -> syn::Result<WebhookAttrs> {
    let mut result = WebhookAttrs::default();
    let attr_str = attr.to_string();

    if let Some(name) = parse_attr_value(&attr_str, "name") {
        result.name = Some(name);
    }

    if let Some(description) = parse_attr_value(&attr_str, "description") {
        result.description = Some(description);
    }

    if has_attr_flag(&attr_str, "allow_unsigned") {
        result.allow_unsigned = true;
    }

    if let Some(path_start) = attr_str.find("path")
        && let Some(eq_pos) = attr_str[path_start..].find('=')
    {
        let after_eq = &attr_str[path_start + eq_pos + 1..];
        if let Some(quote_start) = after_eq.find('"') {
            let after_quote = &after_eq[quote_start + 1..];
            if let Some(quote_end) = after_quote.find('"') {
                result.path = Some(after_quote[..quote_end].to_string());
            }
        }
    }

    if let Some(sig_start) = attr_str.find("signature") {
        let remaining = &attr_str[sig_start..];

        if remaining.contains("hmac_sha256") {
            result.signature_algorithm = Some("HmacSha256".to_string());
        } else if remaining.contains("stripe_webhooks") {
            result.signature_algorithm = Some("StripeWebhooks".to_string());
        } else if remaining.contains("shopify_webhooks") {
            result.signature_algorithm = Some("HmacSha256Base64".to_string());
        } else if remaining.contains("ed25519") {
            result.signature_algorithm = Some("Ed25519".to_string());
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
            let single_arg_header = match result.signature_algorithm.as_deref() {
                Some("StripeWebhooks") => Some("stripe-signature"),
                Some("HmacSha256Base64") => Some("x-shopify-hmac-sha256"),
                _ => None,
            };
            if let Some(fixed_header) = single_arg_header {
                if quotes.len() >= 2 {
                    let secret_start = quotes[0].0 + 1;
                    let secret_end = quotes[1].0;
                    result.signature_secret_env =
                        Some(args_str[secret_start..secret_end].to_string());
                    result.signature_header = Some(fixed_header.to_string());
                }
            } else if quotes.len() >= 4 {
                // Two-arg variants: header name then secret/public-key env
                let header_start = quotes[0].0 + 1;
                let header_end = quotes[1].0;
                result.signature_header = Some(args_str[header_start..header_end].to_string());

                let secret_start = quotes[2].0 + 1;
                let secret_end = quotes[3].0;
                result.signature_secret_env = Some(args_str[secret_start..secret_end].to_string());
            }
        }
    }

    if let Some(idem_start) = attr_str.find("idempotency")
        && let Some(eq_pos) = attr_str[idem_start..].find('=')
    {
        let after_eq = &attr_str[idem_start + eq_pos + 1..];
        if let Some(quote_start) = after_eq.find('"') {
            let after_quote = &after_eq[quote_start + 1..];
            if let Some(quote_end) = after_quote.find('"') {
                result.idempotency = Some(after_quote[..quote_end].to_string());
            }
        }
    }

    if let Some(timeout) = parse_attr_value(&attr_str, "timeout") {
        result.timeout = Some(timeout);
    }

    match &result.path {
        None => {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                "webhook requires path attribute",
            ));
        }
        Some(p) if p.trim().is_empty() || !p.starts_with('/') => {
            return Err(syn::Error::new(
                proc_macro2::Span::call_site(),
                "webhook path must start with '/' (example: path = \"/webhooks/stripe\")",
            ));
        }
        _ => {}
    }

    Ok(result)
}

pub fn webhook_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);
    let attr_str = attr.to_string();

    if let Err(e) = validate_attr_keys(&attr_str, ALLOWED_WEBHOOK_KEYS, "webhook") {
        return e.to_compile_error().into();
    }

    let attrs = match parse_webhook_attrs(attr) {
        Ok(attrs) => attrs,
        Err(e) => return e.to_compile_error().into(),
    };

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

            forge::inventory::submit!(forge::AutoWebhook(|registry| {
                registry.register::<#struct_name>();
            }));
        }
    };

    TokenStream::from(expanded)
}

// Tests for to_pascal_case and parse_duration are in utils.rs (single source of truth).

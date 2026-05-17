use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{ItemFn, parse_macro_input};

use darling::FromMeta;
use darling::ast::NestedMeta;

use crate::attrs::default_true;
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
    /// Override the default replay window (300s) for non-Stripe schemes.
    /// Set to `0` to disable replay enforcement (not recommended).
    #[darling(default)]
    replay_window_secs: Option<u64>,
    /// Set `register = false` to skip `inventory::submit!` auto-registration.
    #[darling(default = "default_true")]
    register: bool,
    // `signature` is handled manually - darling will see it as unknown, so we
    // parse the raw token stream for it before handing to darling.
}

#[derive(Debug, Default)]
struct WebhookAttrs {
    /// Override the registry name (default: function name).
    name: Option<String>,
    description: Option<String>,
    path: Option<String>,
    signature_algorithm: Option<WebhookSignatureAlgorithm>,
    signature_header: Option<String>,
    signature_secret_env: Option<String>,
    allow_unsigned: bool,
    idempotency: Option<String>,
    timeout: Option<String>,
    replay_window_secs: Option<u64>,
    register: bool,
}

#[derive(Debug, Clone, Copy)]
enum WebhookSignatureAlgorithm {
    HmacSha256,
    StripeWebhooks,
    HmacSha256Base64,
    Ed25519,
}

#[derive(Debug, Default)]
struct WebhookSignatureInfo {
    algorithm: Option<WebhookSignatureAlgorithm>,
    header: Option<String>,
    secret_env: Option<String>,
}

/// Pull the `signature = WebhookSignature::<method>(...)` clause out of an
/// already-parsed attribute meta list. The value is a real Rust expression,
/// so we pattern-match the syn AST instead of substring-scanning the token
/// string (which previously misclassified env names like "MY_HMAC_SHA256_KEY"
/// because they happened to contain a method substring).
fn parse_signature_from_meta(attr_args: &[NestedMeta]) -> Result<WebhookSignatureInfo, syn::Error> {
    let mut info = WebhookSignatureInfo::default();

    let Some(value_expr) = attr_args.iter().find_map(|meta| {
        if let NestedMeta::Meta(syn::Meta::NameValue(nv)) = meta
            && nv.path.is_ident("signature")
        {
            return Some(&nv.value);
        }
        None
    }) else {
        return Ok(info);
    };

    let syn::Expr::Call(call) = value_expr else {
        return Err(syn::Error::new_spanned(
            value_expr,
            "expected `WebhookSignature::<method>(...)`",
        ));
    };

    let syn::Expr::Path(path_expr) = call.func.as_ref() else {
        return Err(syn::Error::new_spanned(
            call.func.as_ref(),
            "expected a `WebhookSignature::<method>` path",
        ));
    };
    let Some(method_seg) = path_expr.path.segments.last() else {
        return Err(syn::Error::new_spanned(
            &path_expr.path,
            "empty signature path",
        ));
    };
    let method_name = method_seg.ident.to_string();

    let (algorithm, single_arg_header) = match method_name.as_str() {
        "hmac_sha256" => (WebhookSignatureAlgorithm::HmacSha256, None),
        "stripe_webhooks" => (
            WebhookSignatureAlgorithm::StripeWebhooks,
            Some("stripe-signature"),
        ),
        "shopify_webhooks" => (
            WebhookSignatureAlgorithm::HmacSha256Base64,
            Some("x-shopify-hmac-sha256"),
        ),
        "ed25519" => (WebhookSignatureAlgorithm::Ed25519, None),
        other => {
            return Err(syn::Error::new_spanned(
                &method_seg.ident,
                format!(
                    "unknown signature method `{other}`; expected one of \
                     hmac_sha256, stripe_webhooks, shopify_webhooks, ed25519"
                ),
            ));
        }
    };
    info.algorithm = Some(algorithm);

    let extract_str = |arg: &syn::Expr| -> Result<String, syn::Error> {
        if let syn::Expr::Lit(lit) = arg
            && let syn::Lit::Str(s) = &lit.lit
        {
            return Ok(s.value());
        }
        Err(syn::Error::new_spanned(arg, "expected string literal"))
    };

    let args: Vec<&syn::Expr> = call.args.iter().collect();
    if let Some(fixed_header) = single_arg_header {
        if args.len() != 1 {
            return Err(syn::Error::new_spanned(
                &call.args,
                format!("`{method_name}` takes 1 argument (secret env name)"),
            ));
        }
        info.header = Some(fixed_header.to_string());
        info.secret_env = Some(extract_str(args[0])?);
    } else {
        if args.len() != 2 {
            return Err(syn::Error::new_spanned(
                &call.args,
                format!("`{method_name}` takes 2 arguments (header name, secret env name)"),
            ));
        }
        info.header = Some(extract_str(args[0])?);
        info.secret_env = Some(extract_str(args[1])?);
    }

    Ok(info)
}

pub fn webhook_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);

    // Parse the attribute meta list once. `signature = <Rust expr>` is not a
    // darling-friendly meta value, so we pull it out via syn before the
    // remaining items go to darling.
    let attr_args = match NestedMeta::parse_meta_list(attr.into()) {
        Ok(v) => v,
        Err(e) => return TokenStream::from(e.into_compile_error()),
    };

    let sig_info = match parse_signature_from_meta(&attr_args) {
        Ok(info) => info,
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
        replay_window_secs: darling_attrs.replay_window_secs,
        register: darling_attrs.register,
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
        attrs.signature_algorithm,
        &attrs.signature_header,
        &attrs.signature_secret_env,
    ) {
        let alg_token = match alg {
            WebhookSignatureAlgorithm::HmacSha256 => {
                quote! { forge::forge_core::webhook::SignatureAlgorithm::HmacSha256 }
            }
            WebhookSignatureAlgorithm::StripeWebhooks => {
                quote! { forge::forge_core::webhook::SignatureAlgorithm::StripeWebhooks }
            }
            WebhookSignatureAlgorithm::HmacSha256Base64 => {
                quote! { forge::forge_core::webhook::SignatureAlgorithm::HmacSha256Base64 }
            }
            WebhookSignatureAlgorithm::Ed25519 => {
                quote! { forge::forge_core::webhook::SignatureAlgorithm::Ed25519 }
            }
        };
        let replay_window_tokens = match attrs.replay_window_secs {
            Some(secs) => quote! { #secs },
            None => quote! { forge::forge_core::webhook::DEFAULT_REPLAY_WINDOW_SECS },
        };
        quote! {
            Some(forge::forge_core::webhook::SignatureConfig {
                algorithm: #alg_token,
                header_name: #header,
                secret_env: #secret_env,
                replay_window_secs: #replay_window_tokens,
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

    let registration = if attrs.register {
        quote! {
            forge::inventory::submit!(forge::AutoHandler(|registries| {
                registries.webhooks.register::<#struct_name>();
                registries.functions.register_webhook_info(
                    forge::forge_core::FunctionInfo::from(&#struct_name::info())
                );
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

            #registration
        }
    };

    TokenStream::from(expanded)
}

// Tests for to_pascal_case and parse_duration are in utils.rs (single source of truth).

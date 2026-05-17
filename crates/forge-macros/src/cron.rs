use std::str::FromStr;

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{ItemFn, parse_macro_input};

use darling::FromMeta;
use darling::ast::NestedMeta;

use crate::utils::{parse_duration_tokens, to_pascal_case};

/// Darling-parsed cron attributes (excludes the positional schedule string).
#[derive(Debug, Default, FromMeta)]
struct DarlingCronAttrs {
    /// Override the registry name (default: function name).
    #[darling(default)]
    name: Option<String>,
    #[darling(default)]
    timezone: Option<String>,
    #[darling(default)]
    group: Option<String>,
    #[darling(default)]
    catch_up: bool,
    #[darling(default)]
    catch_up_limit: Option<u32>,
    #[darling(default)]
    timeout: Option<String>,
}

#[derive(Debug, Default)]
struct CronAttrs {
    /// Override the registry name (default: function name).
    name: Option<String>,
    schedule: Option<String>,
    timezone: Option<String>,
    group: Option<String>,
    catch_up: bool,
    catch_up_limit: Option<u32>,
    timeout: Option<String>,
}

pub fn cron_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);

    let attr_args = match NestedMeta::parse_meta_list(attr.into()) {
        Ok(v) => v,
        Err(e) => return TokenStream::from(e.into_compile_error()),
    };

    // Extract the positional schedule string (first literal argument)
    let mut schedule: Option<String> = None;
    let mut remaining_args: Vec<NestedMeta> = Vec::new();

    for (i, arg) in attr_args.into_iter().enumerate() {
        if i == 0
            && let NestedMeta::Lit(syn::Lit::Str(s)) = &arg
        {
            schedule = Some(s.value());
            continue;
        }
        remaining_args.push(arg);
    }

    let darling_attrs = match DarlingCronAttrs::from_list(&remaining_args) {
        Ok(v) => v,
        Err(e) => return TokenStream::from(e.write_errors()),
    };

    let attrs = CronAttrs {
        name: darling_attrs.name,
        schedule,
        timezone: darling_attrs.timezone,
        group: darling_attrs.group,
        catch_up: darling_attrs.catch_up,
        catch_up_limit: darling_attrs.catch_up_limit,
        timeout: darling_attrs.timeout,
    };

    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let rpc_name = attrs.name.as_deref().unwrap_or(&fn_name_str).to_string();
    let module_name = format_ident!("__forge_handler_{}", fn_name);
    let struct_name = format_ident!("{}Cron", to_pascal_case(&fn_name.to_string()));

    let _vis = &input.vis;
    let block = &input.block;

    let schedule = attrs.schedule.unwrap_or_else(|| "* * * * *".to_string());

    // Validate cron expression at compile time to prevent runtime panics.
    // Normalize 5-part to 6-part (prepend seconds) to match what CronSchedule::new does.
    {
        let parts: Vec<&str> = schedule.split_whitespace().collect();
        let normalized = if parts.len() == 5 {
            format!("0 {schedule}")
        } else {
            schedule.clone()
        };
        if cron::Schedule::from_str(&normalized).is_err() {
            return syn::Error::new_spanned(
                &input.sig.ident,
                format!("Invalid cron schedule: \"{schedule}\""),
            )
            .to_compile_error()
            .into();
        }
    }
    let timezone = attrs.timezone.unwrap_or_else(|| "UTC".to_string());
    let group = attrs.group.unwrap_or_else(|| "default".to_string());
    let catch_up = attrs.catch_up;
    let catch_up_limit = attrs.catch_up_limit.unwrap_or(10);

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

    let other_attrs = &input.attrs;

    let expanded = quote! {
        #[doc(hidden)]
        #[allow(non_snake_case)]
        mod #module_name {
            use super::*;

            #(#other_attrs)*
            pub struct #struct_name;

            impl forge::forge_core::__sealed::Sealed for #struct_name {}

            impl forge::forge_core::cron::ForgeCron for #struct_name {
                type Args = ();

                fn info() -> forge::forge_core::cron::CronInfo {
                    forge::forge_core::cron::CronInfo {
                        name: #rpc_name,
                        schedule: forge::forge_core::cron::CronSchedule::new_validated(#schedule),
                        timezone: #timezone,
                        group: #group,
                        catch_up: #catch_up,
                        catch_up_limit: #catch_up_limit,
                        timeout: #timeout,
                        http_timeout: #http_timeout,
                    }
                }

                fn execute(
                    ctx: &forge::forge_core::cron::CronContext,
                ) -> std::pin::Pin<Box<dyn std::future::Future<Output = forge::forge_core::Result<()>> + Send + '_>> {
                    Box::pin(async move #block)
                }
            }

            forge::inventory::submit!(forge::AutoHandler(|registries| {
                registries.crons.register::<#struct_name>();
            }));
        }
    };

    TokenStream::from(expanded)
}

// Tests for to_pascal_case and parse_duration are in utils.rs (single source of truth).

use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{ItemFn, parse_macro_input};

use darling::FromMeta;
use darling::ast::NestedMeta;

use crate::attrs::default_true;
use crate::utils::{parse_duration_tokens, to_pascal_case};

/// Darling-parsed daemon attributes.
#[derive(Debug, Default, FromMeta)]
struct DarlingDaemonAttrs {
    /// Override the registry name (default: function name).
    #[darling(default)]
    name: Option<String>,
    #[darling(default)]
    leader_elected: Option<bool>,
    #[darling(default)]
    restart_on_panic: Option<bool>,
    #[darling(default)]
    timeout: Option<String>,
    #[darling(default)]
    restart_delay: Option<String>,
    #[darling(default)]
    startup_delay: Option<String>,
    #[darling(default)]
    max_restarts: Option<u32>,
    /// Set `register = false` to skip `inventory::submit!` auto-registration.
    #[darling(default = "default_true")]
    register: bool,
}

#[derive(Debug, Default)]
struct DaemonAttrs {
    /// Override the registry name (default: function name).
    name: Option<String>,
    leader_elected: Option<bool>,
    restart_on_panic: Option<bool>,
    timeout: Option<String>,
    restart_delay: Option<String>,
    startup_delay: Option<String>,
    max_restarts: Option<u32>,
    register: bool,
}

pub fn daemon_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    let forge = crate::utils::forge_path();
    let input = parse_macro_input!(item as ItemFn);

    let attr_args = match NestedMeta::parse_meta_list(attr.into()) {
        Ok(v) => v,
        Err(e) => return TokenStream::from(e.into_compile_error()),
    };

    let darling_attrs = match DarlingDaemonAttrs::from_list(&attr_args) {
        Ok(v) => v,
        Err(e) => return TokenStream::from(e.write_errors()),
    };

    let attrs = DaemonAttrs {
        name: darling_attrs.name,
        leader_elected: darling_attrs.leader_elected,
        restart_on_panic: darling_attrs.restart_on_panic,
        timeout: darling_attrs.timeout,
        restart_delay: darling_attrs.restart_delay,
        startup_delay: darling_attrs.startup_delay,
        max_restarts: darling_attrs.max_restarts,
        register: darling_attrs.register,
    };

    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let rpc_name = attrs.name.as_deref().unwrap_or(&fn_name_str).to_string();
    let module_name = format_ident!("__forge_handler_{}", fn_name);
    let struct_name = format_ident!("{}Daemon", to_pascal_case(&fn_name.to_string()));

    let _vis = &input.vis;
    let block = &input.block;

    let leader_elected = attrs.leader_elected.unwrap_or(true);
    let restart_on_panic = attrs.restart_on_panic.unwrap_or(true);

    let restart_delay = if let Some(ref d) = attrs.restart_delay {
        parse_duration_tokens(d, 5)
    } else {
        quote! { std::time::Duration::from_secs(5) }
    };

    let startup_delay = if let Some(ref d) = attrs.startup_delay {
        parse_duration_tokens(d, 0)
    } else {
        quote! { std::time::Duration::from_secs(0) }
    };
    let http_timeout = if let Some(ref t) = attrs.timeout {
        let timeout = parse_duration_tokens(t, 0);
        quote! { Some(#timeout) }
    } else {
        quote! { None }
    };

    let max_restarts = if let Some(n) = attrs.max_restarts {
        quote! { Some(#n) }
    } else {
        quote! { None }
    };

    let other_attrs = &input.attrs;

    let registration = if attrs.register {
        quote! {
            #forge::inventory::submit!(#forge::AutoHandler(|registries| {
                registries.daemons.register::<#struct_name>();
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

            impl #forge::forge_core::__sealed::Sealed for #struct_name {}

            impl #forge::forge_core::daemon::ForgeDaemon for #struct_name {
                fn info() -> #forge::forge_core::daemon::DaemonInfo {
                    #forge::forge_core::daemon::DaemonInfo {
                        name: #rpc_name,
                        leader_elected: #leader_elected,
                        restart_on_panic: #restart_on_panic,
                        restart_delay: #restart_delay,
                        startup_delay: #startup_delay,
                        http_timeout: #http_timeout,
                        max_restarts: #max_restarts,
                    }
                }

                fn execute(
                    ctx: &#forge::forge_core::daemon::DaemonContext,
                ) -> std::pin::Pin<Box<dyn std::future::Future<Output = #forge::forge_core::Result<()>> + Send + '_>> {
                    Box::pin(async move #block)
                }
            }

            #registration
        }
    };

    TokenStream::from(expanded)
}

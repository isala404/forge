use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::{parse_macro_input, ItemFn, Meta};

/// Workflow attributes.
#[derive(Debug, Default)]
struct WorkflowAttrs {
    version: Option<u32>,
    timeout: Option<String>,
    deprecated: bool,
}

fn parse_workflow_attrs(attrs: &[syn::Attribute]) -> WorkflowAttrs {
    let mut result = WorkflowAttrs::default();

    for attr in attrs {
        if attr.path().is_ident("version") {
            if let Meta::NameValue(nv) = &attr.meta {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Int(i),
                    ..
                }) = &nv.value
                {
                    result.version = i.base10_parse().ok();
                }
            }
        }
        if attr.path().is_ident("timeout") {
            if let Meta::NameValue(nv) = &attr.meta {
                if let syn::Expr::Lit(syn::ExprLit {
                    lit: syn::Lit::Str(s),
                    ..
                }) = &nv.value
                {
                    result.timeout = Some(s.value());
                }
            }
        }
        if attr.path().is_ident("deprecated") {
            result.deprecated = true;
        }
    }

    result
}

fn parse_duration(s: &str) -> proc_macro2::TokenStream {
    let s = s.trim();
    if s.ends_with("ms") {
        let n: u64 = s.trim_end_matches("ms").parse().unwrap_or(1000);
        quote! { std::time::Duration::from_millis(#n) }
    } else if s.ends_with('s') {
        let n: u64 = s.trim_end_matches('s').parse().unwrap_or(30);
        quote! { std::time::Duration::from_secs(#n) }
    } else if s.ends_with('m') {
        let n: u64 = s.trim_end_matches('m').parse().unwrap_or(5);
        let secs = n * 60;
        quote! { std::time::Duration::from_secs(#secs) }
    } else if s.ends_with('h') {
        let n: u64 = s.trim_end_matches('h').parse().unwrap_or(1);
        let secs = n * 3600;
        quote! { std::time::Duration::from_secs(#secs) }
    } else if s.ends_with('d') {
        let n: u64 = s.trim_end_matches('d').parse().unwrap_or(1);
        let secs = n * 86400;
        quote! { std::time::Duration::from_secs(#secs) }
    } else {
        let n: u64 = s.parse().unwrap_or(86400);
        quote! { std::time::Duration::from_secs(#n) }
    }
}

pub fn workflow_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    let _ = attr;
    let input = parse_macro_input!(item as ItemFn);
    let attrs = parse_workflow_attrs(&input.attrs);

    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let struct_name = format_ident!("{}Workflow", to_pascal_case(&fn_name.to_string()));

    let vis = &input.vis;
    let block = &input.block;

    // Parse input type from function signature
    let mut input_type = quote! { () };
    let mut input_ident = format_ident!("_input");

    for (i, input_arg) in input.sig.inputs.iter().enumerate() {
        if i == 0 {
            continue; // Skip context
        }
        if let syn::FnArg::Typed(pat_type) = input_arg {
            if let syn::Pat::Ident(ident) = pat_type.pat.as_ref() {
                input_ident = ident.ident.clone();
            }
            let ty = &pat_type.ty;
            input_type = quote! { #ty };
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

    let version = attrs.version.unwrap_or(1);
    let deprecated = attrs.deprecated;

    let timeout = if let Some(ref t) = attrs.timeout {
        parse_duration(t)
    } else {
        quote! { std::time::Duration::from_secs(86400) } // 24 hours default
    };

    // Filter out our custom attributes
    let other_attrs: Vec<_> = input
        .attrs
        .iter()
        .filter(|a| {
            !a.path().is_ident("version")
                && !a.path().is_ident("timeout")
                && !a.path().is_ident("deprecated")
        })
        .collect();

    let expanded = quote! {
        #(#other_attrs)*
        #vis struct #struct_name;

        impl forge_core::workflow::ForgeWorkflow for #struct_name {
            type Input = #input_type;
            type Output = #output_type;

            fn info() -> forge_core::workflow::WorkflowInfo {
                forge_core::workflow::WorkflowInfo {
                    name: #fn_name_str,
                    version: #version,
                    timeout: #timeout,
                    deprecated: #deprecated,
                }
            }

            fn execute(
                ctx: &forge_core::workflow::WorkflowContext,
                #input_ident: Self::Input,
            ) -> std::pin::Pin<Box<dyn std::future::Future<Output = forge_core::Result<Self::Output>> + Send + '_>> {
                Box::pin(async move #block)
            }
        }
    };

    TokenStream::from(expanded)
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_to_pascal_case() {
        assert_eq!(to_pascal_case("user_onboarding"), "UserOnboarding");
        assert_eq!(to_pascal_case("order_processing"), "OrderProcessing");
        assert_eq!(to_pascal_case("simple"), "Simple");
    }

    #[test]
    fn test_parse_duration_days() {
        let ts = parse_duration("7d");
        assert!(!ts.is_empty());
    }

    #[test]
    fn test_parse_duration_hours() {
        let ts = parse_duration("24h");
        assert!(!ts.is_empty());
    }
}

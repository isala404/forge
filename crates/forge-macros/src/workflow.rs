use proc_macro::TokenStream;
use quote::{format_ident, quote};
use syn::visit::Visit;
use syn::{ExprAwait, ExprCall, ItemFn, Lit, parse_macro_input};

use std::collections::BTreeSet;

use darling::FromMeta;
use darling::ast::NestedMeta;

use crate::attrs::RequireRole;
use crate::utils::{parse_duration_tokens, to_pascal_case};

/// Minimum sleep duration (in seconds) that triggers the tokio::sleep warning.
/// Sleeps shorter than this are allowed since they're typically used for polling/retry loops.
const TOKIO_SLEEP_THRESHOLD_SECS: u64 = 100;

/// Detects tokio::sleep calls with durations exceeding the threshold.
/// Returns the span of the first violation found, if any.
struct TokioSleepDetector {
    violation_span: Option<proc_macro2::Span>,
}

impl TokioSleepDetector {
    fn new() -> Self {
        Self {
            violation_span: None,
        }
    }

    /// Try to extract a duration in seconds from common patterns.
    fn extract_duration_secs(
        args: &syn::punctuated::Punctuated<syn::Expr, syn::token::Comma>,
    ) -> Option<u64> {
        if args.len() != 1 {
            return None;
        }

        if let syn::Expr::Call(call) = &args[0]
            && let syn::Expr::Path(path) = &*call.func
        {
            let path_str: String = path
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect::<Vec<_>>()
                .join("::");

            if path_str.ends_with("from_secs") {
                if let Some(syn::Expr::Lit(lit)) = call.args.first()
                    && let Lit::Int(int_lit) = &lit.lit
                {
                    return int_lit.base10_parse::<u64>().ok();
                }
            } else if path_str.ends_with("from_millis") {
                if let Some(syn::Expr::Lit(lit)) = call.args.first()
                    && let Lit::Int(int_lit) = &lit.lit
                {
                    return int_lit.base10_parse::<u64>().ok().map(|ms| ms / 1000);
                }
            } else if path_str.ends_with("from_days")
                && let Some(syn::Expr::Lit(lit)) = call.args.first()
                && let Lit::Int(int_lit) = &lit.lit
            {
                return int_lit.base10_parse::<u64>().ok().map(|d| d * 86400);
            }
        }
        None
    }

    fn check_sleep_call(
        &mut self,
        path_str: &str,
        args: &syn::punctuated::Punctuated<syn::Expr, syn::token::Comma>,
        span: proc_macro2::Span,
    ) {
        if self.violation_span.is_some() {
            return;
        }

        let is_tokio_sleep =
            (path_str.contains("tokio") && path_str.contains("sleep")) || path_str == "sleep";

        if !is_tokio_sleep {
            return;
        }

        match Self::extract_duration_secs(args) {
            Some(secs) if secs <= TOKIO_SLEEP_THRESHOLD_SECS => {}
            _ => self.violation_span = Some(span),
        }
    }
}

impl<'ast> Visit<'ast> for TokioSleepDetector {
    fn visit_expr_call(&mut self, node: &'ast ExprCall) {
        if let syn::Expr::Path(path) = &*node.func {
            let path_str: String = path
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect::<Vec<_>>()
                .join("::");

            let span = path
                .path
                .segments
                .last()
                .map(|s| s.ident.span())
                .unwrap_or_else(proc_macro2::Span::call_site);

            self.check_sleep_call(&path_str, &node.args, span);
        }
        syn::visit::visit_expr_call(self, node);
    }

    fn visit_expr_await(&mut self, node: &'ast ExprAwait) {
        // Check for tokio::time::sleep(...).await pattern
        if let syn::Expr::Call(call) = &*node.base
            && let syn::Expr::Path(path) = &*call.func
        {
            let path_str: String = path
                .path
                .segments
                .iter()
                .map(|s| s.ident.to_string())
                .collect::<Vec<_>>()
                .join("::");

            let span = path
                .path
                .segments
                .last()
                .map(|s| s.ident.span())
                .unwrap_or_else(proc_macro2::Span::call_site);

            self.check_sleep_call(&path_str, &call.args, span);
        }
        syn::visit::visit_expr_await(self, node);
    }
}

/// Darling-parsed workflow attributes.
#[derive(Debug, FromMeta)]
#[darling(and_then = DarlingWorkflowAttrs::validate)]
struct DarlingWorkflowAttrs {
    #[darling(default)]
    name: Option<String>,
    #[darling(default)]
    version: Option<String>,
    #[darling(default)]
    timeout: Option<String>,
    #[darling(default)]
    public: bool,
    #[darling(default)]
    active: bool,
    #[darling(default)]
    deprecated: bool,
    #[darling(default)]
    status: Option<String>,
    #[darling(default)]
    require_role: Option<RequireRole>,
}

impl DarlingWorkflowAttrs {
    fn validate(self) -> darling::Result<Self> {
        // Validate status value if provided
        if let Some(ref s) = self.status
            && !["active", "deprecated", "staging"].contains(&s.as_str())
        {
            return Err(darling::Error::custom(format!(
                "invalid workflow status \"{s}\": expected one of \"active\", \"deprecated\", \"staging\""
            )));
        }

        // Can't use both status= and legacy flags
        if self.status.is_some() && (self.active || self.deprecated) {
            return Err(darling::Error::custom(
                "use either `status = \"...\"` or the legacy `active`/`deprecated` flag, not both",
            ));
        }

        // Can't be both active and deprecated
        if self.active && self.deprecated {
            return Err(darling::Error::custom(
                "workflow cannot be both `active` and `deprecated`",
            ));
        }

        Ok(self)
    }
}

/// Workflow attributes.
#[derive(Debug)]
struct WorkflowAttrs {
    name: Option<String>,
    version: Option<String>,
    timeout: Option<String>,
    is_public: bool,
    status: WorkflowStatus,
    required_role: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WorkflowStatus {
    Active,
    Deprecated,
    Staging,
}

impl Default for WorkflowAttrs {
    fn default() -> Self {
        Self {
            name: None,
            version: None,
            timeout: None,
            is_public: false,
            status: WorkflowStatus::Active,
            required_role: None,
        }
    }
}

fn convert_workflow_attrs(darling: DarlingWorkflowAttrs) -> WorkflowAttrs {
    let status = if let Some(ref s) = darling.status {
        match s.as_str() {
            "deprecated" => WorkflowStatus::Deprecated,
            "staging" => WorkflowStatus::Staging,
            _ => WorkflowStatus::Active,
        }
    } else if darling.deprecated {
        WorkflowStatus::Deprecated
    } else {
        WorkflowStatus::Active
    };

    WorkflowAttrs {
        name: darling.name,
        version: darling.version,
        timeout: darling.timeout,
        is_public: darling.public,
        status,
        required_role: darling.require_role.map(|r| r.0),
    }
}

/// Extract step and wait keys from the workflow function body for signature derivation.
/// Looks for patterns like `ctx.step("key")`, `ctx.wait_for_event::<T>("event", ...)`,
/// `ctx.sleep(...)`, and `ctx.parallel()...step("key")`.
struct ContractExtractor {
    step_keys: BTreeSet<String>,
    wait_keys: BTreeSet<String>,
}

impl ContractExtractor {
    fn new() -> Self {
        Self {
            step_keys: BTreeSet::new(),
            wait_keys: BTreeSet::new(),
        }
    }

    fn extract_string_lit(expr: &syn::Expr) -> Option<String> {
        if let syn::Expr::Lit(lit) = expr
            && let Lit::Str(s) = &lit.lit
        {
            return Some(s.value());
        }
        None
    }
}

impl<'ast> Visit<'ast> for ContractExtractor {
    fn visit_expr_method_call(&mut self, node: &'ast syn::ExprMethodCall) {
        let method_name = node.method.to_string();

        match method_name.as_str() {
            // ctx.step("key", ...) or builder.step("key", ...)
            "step" => {
                if let Some(first_arg) = node.args.first()
                    && let Some(key) = Self::extract_string_lit(first_arg)
                {
                    self.step_keys.insert(key);
                }
            }
            // ctx.wait_for_event::<T>("event_name", ...)
            "wait_for_event" => {
                if let Some(first_arg) = node.args.first()
                    && let Some(key) = Self::extract_string_lit(first_arg)
                {
                    self.wait_keys.insert(key);
                }
            }
            _ => {}
        }

        // Continue visiting child nodes
        syn::visit::visit_expr_method_call(self, node);
    }
}

/// Derive a workflow signature from its persisted contract.
/// The signature is a hex-encoded hash of: name, version, step keys, wait keys,
/// timeout, and input/output type shapes.
fn derive_signature(
    name: &str,
    version: &str,
    step_keys: &BTreeSet<String>,
    wait_keys: &BTreeSet<String>,
    timeout_secs: u64,
    input_type: &str,
    output_type: &str,
) -> String {
    // Simple FNV-1a 64-bit hash (no external crate needed in proc macros)
    let mut hash: u64 = 0xcbf29ce484222325;
    let fnv_prime: u64 = 0x100000001b3;

    let mut feed = |bytes: &[u8]| {
        for &b in bytes {
            hash ^= u64::from(b);
            hash = hash.wrapping_mul(fnv_prime);
        }
        // separator
        hash ^= 0xff;
        hash = hash.wrapping_mul(fnv_prime);
    };

    feed(name.as_bytes());
    feed(version.as_bytes());
    for key in step_keys {
        feed(b"step:");
        feed(key.as_bytes());
    }
    for key in wait_keys {
        feed(b"wait:");
        feed(key.as_bytes());
    }
    feed(timeout_secs.to_le_bytes().as_slice());
    feed(input_type.as_bytes());
    feed(output_type.as_bytes());

    format!("{hash:016x}")
}

pub fn workflow_impl(attr: TokenStream, item: TokenStream) -> TokenStream {
    let input = parse_macro_input!(item as ItemFn);

    let attr_args = match NestedMeta::parse_meta_list(attr.into()) {
        Ok(v) => v,
        Err(e) => return TokenStream::from(e.into_compile_error()),
    };

    let darling_attrs = match DarlingWorkflowAttrs::from_list(&attr_args) {
        Ok(v) => v,
        Err(e) => return TokenStream::from(e.write_errors()),
    };

    let attrs = convert_workflow_attrs(darling_attrs);

    let fn_name = &input.sig.ident;
    let fn_name_str = fn_name.to_string();
    let module_name = format_ident!("__forge_handler_{}", fn_name);
    let workflow_name = attrs.name.as_deref().unwrap_or(&fn_name_str);
    let struct_name = format_ident!("{}Workflow", to_pascal_case(&fn_name.to_string()));

    let _vis = &input.vis;
    let block = &input.block;

    // Detect tokio::sleep usage (only for long sleeps > 100s)
    let mut sleep_detector = TokioSleepDetector::new();
    sleep_detector.visit_block(block);
    if let Some(span) = sleep_detector.violation_span {
        return syn::Error::new(
            span,
            "Use `ctx.sleep()` instead of `tokio::sleep()` for long sleeps in workflows. \
             Workflows require durable sleep that survives process restarts. \
             Short sleeps (<100s) for polling are allowed with tokio::sleep.",
        )
        .to_compile_error()
        .into();
    }

    // Extract step/wait keys from function body for signature derivation
    let mut contract_extractor = ContractExtractor::new();
    contract_extractor.visit_block(block);

    // Parse input type from function signature
    let mut input_type = quote! { () };
    let mut input_ident = format_ident!("_input");
    let mut input_type_str = String::from("()");

    for (i, input_arg) in input.sig.inputs.iter().enumerate() {
        if i == 0 {
            continue; // Skip context
        }
        if let syn::FnArg::Typed(pat_type) = input_arg {
            if let syn::Pat::Ident(ident) = pat_type.pat.as_ref() {
                input_ident = ident.ident.clone();
            }
            let ty = &pat_type.ty;
            input_type_str = quote!(#ty).to_string();
            input_type = quote! { #ty };
        }
    }

    // Parse return type
    let mut output_type_str = String::from("()");
    let output_type = match &input.sig.output {
        syn::ReturnType::Default => quote! { () },
        syn::ReturnType::Type(_, ty) => {
            if let syn::Type::Path(path) = ty.as_ref() {
                if let Some(segment) = path.path.segments.last() {
                    if segment.ident == "Result" {
                        if let syn::PathArguments::AngleBracketed(args) = &segment.arguments {
                            if let Some(syn::GenericArgument::Type(inner)) = args.args.first() {
                                output_type_str = quote!(#inner).to_string();
                                quote! { #inner }
                            } else {
                                quote! { () }
                            }
                        } else {
                            quote! { () }
                        }
                    } else {
                        output_type_str = quote!(#ty).to_string();
                        quote! { #ty }
                    }
                } else {
                    output_type_str = quote!(#ty).to_string();
                    quote! { #ty }
                }
            } else {
                output_type_str = quote!(#ty).to_string();
                quote! { #ty }
            }
        }
    };

    let version_str = attrs.version.as_deref().unwrap_or("v1");
    let is_public = attrs.is_public;
    let workflow_status = match attrs.status {
        WorkflowStatus::Active => {
            quote! { forge::forge_core::workflow::WorkflowDefStatus::Active }
        }
        WorkflowStatus::Deprecated => {
            quote! { forge::forge_core::workflow::WorkflowDefStatus::Deprecated }
        }
        WorkflowStatus::Staging => {
            quote! { forge::forge_core::workflow::WorkflowDefStatus::Staging }
        }
    };

    let required_role = if let Some(ref role) = attrs.required_role {
        quote! { Some(#role) }
    } else {
        quote! { None }
    };

    let timeout = if let Some(ref t) = attrs.timeout {
        parse_duration_tokens(t, 86400)
    } else {
        quote! { std::time::Duration::from_secs(86400) } // 24 hours default
    };

    // Compute timeout seconds for signature
    let timeout_secs: u64 = if let Some(ref t) = attrs.timeout {
        crate::utils::parse_duration_secs(t).unwrap_or(86400)
    } else {
        86400
    };

    let http_timeout = if let Some(ref t) = attrs.timeout {
        let timeout = parse_duration_tokens(t, 0);
        quote! { Some(#timeout) }
    } else {
        quote! { None }
    };

    // Derive the workflow signature from its persisted contract
    let signature = derive_signature(
        workflow_name,
        version_str,
        &contract_extractor.step_keys,
        &contract_extractor.wait_keys,
        timeout_secs,
        &input_type_str,
        &output_type_str,
    );

    let fn_attrs = &input.attrs;

    let expanded = quote! {
        #[doc(hidden)]
        #[allow(non_snake_case)]
        mod #module_name {
            use super::*;

            #(#fn_attrs)*
            pub struct #struct_name;

            impl forge::forge_core::__sealed::Sealed for #struct_name {}

            impl forge::forge_core::workflow::ForgeWorkflow for #struct_name {
                type Input = #input_type;
                type Output = #output_type;

                fn info() -> forge::forge_core::workflow::WorkflowInfo {
                    forge::forge_core::workflow::WorkflowInfo {
                        name: #workflow_name,
                        version: #version_str,
                        signature: #signature,
                        status: #workflow_status,
                        timeout: #timeout,
                        http_timeout: #http_timeout,
                        is_public: #is_public,
                        required_role: #required_role,
                    }
                }

                fn execute(
                    ctx: &forge::forge_core::workflow::WorkflowContext,
                    #input_ident: Self::Input,
                ) -> std::pin::Pin<Box<dyn std::future::Future<Output = forge::forge_core::Result<Self::Output>> + Send + '_>> {
                    Box::pin(async move #block)
                }
            }

            forge::inventory::submit!(forge::AutoHandler(|registries| {
                registries.workflows.register::<#struct_name>();
            }));
        }
    };

    TokenStream::from(expanded)
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests for to_pascal_case and parse_duration are in utils.rs (single source of truth).

    #[test]
    fn test_derive_signature_deterministic() {
        let mut steps = BTreeSet::new();
        steps.insert("create_user".to_string());
        steps.insert("send_email".to_string());
        let waits = BTreeSet::new();

        let sig1 = derive_signature("onboarding", "v1", &steps, &waits, 86400, "Input", "Output");
        let sig2 = derive_signature("onboarding", "v1", &steps, &waits, 86400, "Input", "Output");
        assert_eq!(sig1, sig2);
    }

    #[test]
    fn test_derive_signature_changes_with_steps() {
        let mut steps1 = BTreeSet::new();
        steps1.insert("create_user".to_string());
        let mut steps2 = BTreeSet::new();
        steps2.insert("create_user".to_string());
        steps2.insert("send_email".to_string());
        let waits = BTreeSet::new();

        let sig1 = derive_signature("wf", "v1", &steps1, &waits, 86400, "()", "()");
        let sig2 = derive_signature("wf", "v1", &steps2, &waits, 86400, "()", "()");
        assert_ne!(sig1, sig2);
    }

    #[test]
    fn test_derive_signature_changes_with_version() {
        let steps = BTreeSet::new();
        let waits = BTreeSet::new();

        let sig1 = derive_signature("wf", "v1", &steps, &waits, 86400, "()", "()");
        let sig2 = derive_signature("wf", "v2", &steps, &waits, 86400, "()", "()");
        assert_ne!(sig1, sig2);
    }

    #[test]
    fn test_derive_signature_changes_with_waits() {
        let steps = BTreeSet::new();
        let mut waits1 = BTreeSet::new();
        waits1.insert("payment_confirmed".to_string());
        let waits2 = BTreeSet::new();

        let sig1 = derive_signature("wf", "v1", &steps, &waits1, 86400, "()", "()");
        let sig2 = derive_signature("wf", "v1", &steps, &waits2, 86400, "()", "()");
        assert_ne!(sig1, sig2);
    }

    // Note: parse_workflow_attrs takes proc_macro::TokenStream which can't be used
    // outside of a proc macro context. Attribute parsing is tested via integration
    // tests (macro expansion in the demo example).
}

//! Shared builder helpers for Dioxus code generation.
//!
//! Both `types.rs` and `api.rs` generate constructor + setter methods for
//! structs. This module provides the shared logic so it can't diverge.

use forge_core::schema::RustType;

use crate::emit;

/// Get the parameter type for a builder method.
/// String fields use `impl Into<String>` for ergonomics.
pub fn param_type(rust_type: &RustType) -> String {
    let ty = emit::dioxus_type(rust_type);
    if ty == "String" {
        "impl Into<String>".into()
    } else {
        ty
    }
}

/// Get the value expression for a builder field assignment.
/// String fields call `.into()` to match the `impl Into<String>` parameter.
pub fn value_expr(name: &str, rust_type: &RustType) -> String {
    if emit::dioxus_type(rust_type) == "String" {
        format!("{name}.into()")
    } else {
        name.to_string()
    }
}

//! FORGE - The Rust Full-Stack Framework
//!
//! A batteries-included framework for building full-stack web applications
//! with Rust backend and Svelte 5 frontend.

mod runtime;

// Re-export forge_core for macro-generated code
#[doc(hidden)]
pub use forge_core;

// Re-export proc macros at crate root
pub use forge_macros::{action, cron, forge_enum, job, model, mutation, query, workflow};

// Re-export Migration type for programmatic migrations
pub use forge_runtime::migrations::Migration;

// Re-export testing assertion macros at crate root when testing feature is enabled.
// These macros use #[macro_export] which places them at forge_core crate root.
#[cfg(feature = "testing")]
pub use forge_core::{
    assert_err, assert_err_variant, assert_http_called, assert_http_not_called,
    assert_job_dispatched, assert_job_not_dispatched, assert_ok, assert_workflow_not_started,
    assert_workflow_started,
};

pub use runtime::prelude;
pub use runtime::{Forge, ForgeBuilder};

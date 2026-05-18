//! FORGE - The Rust Full-Stack Framework
//!
//! A batteries-included framework for building full-stack web applications
//! with a Rust backend and generated SvelteKit or Dioxus frontends.
//!
//! ## Features
//!
//! Cargo features control which subsystems are compiled in. The default
//! feature set is `full` — every subsystem enabled. To shrink your binary,
//! disable defaults and opt into a preset:
//!
//! ```toml
//! # Worker-only binary (no HTTP gateway)
//! forge = { version = "0.9", default-features = false, features = ["worker"] }
//!
//! # API server (no background workers)
//! forge = { version = "0.9", default-features = false, features = ["api"] }
//! ```
//!
//! Available presets: `full`, `worker`, `api`, `minimal`.
//! Available subsystems: `gateway`, `jobs`, `workflows`, `cron`, `daemons`,
//! `geoip`, `otel`.

mod auto_register;
#[cfg(feature = "embedded-frontend")]
mod embedded;
mod runtime;

// Re-export forge_core for macro-generated code
#[doc(hidden)]
pub use forge_core;

// Re-export schemars so user crates don't need it as a direct dep.
// The mcp_tool scaffold uses `#[schemars(crate = "forge::schemars")]` to point
// the derive at this re-export.
pub use forge_core::schemars;

// Re-export inventory for macro-generated auto-registration
#[doc(hidden)]
pub use inventory;

// Re-export auto-registration types for macro-generated code.
pub use auto_register::{AutoHandler, HandlerRegistries};

// Re-export embedded frontend handler
#[cfg(feature = "embedded-frontend")]
pub use embedded::serve_embedded_assets;

// Re-export proc macros at crate root
pub use forge_macros::{
    cron, daemon, forge_enum, job, mcp_tool, model, mutation, query, webhook, workflow,
};

// Re-export Migration type for programmatic migrations
pub use forge_runtime::pg::migration::Migration;

// Re-export testing utilities
pub use forge_core::testing;

// Re-export testing assertion macros
pub use forge_core::{
    assert_err, assert_err_matches, assert_err_variant, assert_http_called, assert_http_not_called,
    assert_job_dispatched, assert_job_not_dispatched, assert_ok, assert_workflow_not_started,
    assert_workflow_started,
};

/// All internal FORGE schema SQL concatenated.
///
/// For tests: apply before user migrations. In production, migration runner handles versioning.
pub fn get_internal_sql() -> String {
    forge_runtime::pg::migration::get_all_system_sql()
}

pub use runtime::prelude;
pub use runtime::{Forge, ForgeBuilder};

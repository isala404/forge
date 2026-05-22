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

#[doc(hidden)]
pub use forge_core;

// The mcp_tool scaffold uses `#[schemars(crate = "forge::schemars")]` to point
// the derive at this re-export.
pub use forge_core::schemars;

#[doc(hidden)]
pub use inventory;

pub use auto_register::{AutoHandler, HandlerRegistries, auto_register_all};

#[cfg(feature = "embedded-frontend")]
pub use embedded::serve_embedded_assets;

pub use forge_macros::{
    cron, daemon, forge_enum, job, mcp_tool, model, mutation, query, webhook, workflow,
};

pub use forge_runtime::pg::migration::Migration;

pub use forge_core::testing;

pub use forge_core::{
    assert_err, assert_err_matches, assert_err_variant, assert_http_called, assert_http_not_called,
    assert_job_dispatched, assert_job_not_dispatched, assert_ok, assert_workflow_not_started,
    assert_workflow_started,
};

/// All internal FORGE schema SQL concatenated. Apply before user migrations in tests.
pub fn get_internal_sql() -> String {
    forge_runtime::pg::migration::get_all_system_sql()
}

pub use runtime::prelude;
pub use runtime::{Forge, ForgeBuilder};

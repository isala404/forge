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

pub use runtime::prelude;
pub use runtime::{Forge, ForgeBuilder};

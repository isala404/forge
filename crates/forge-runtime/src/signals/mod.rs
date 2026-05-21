//! Built-in product analytics and frontend diagnostics.

pub mod bot;
pub mod collector;
pub mod device;
pub mod emit;
pub mod endpoints;
pub mod geoip;
pub mod partition;
pub mod rate_limit;
pub mod session;
pub mod visitor;

pub use collector::SignalsCollector;
pub use emit::{emit_diagnostic, emit_raw, emit_server_execution, install as install_global};

#[cfg(all(test, feature = "testcontainers"))]
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests;

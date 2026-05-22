//! In-process integration harness for Forge.
//!
//! Boots a real gateway + worker + reactor against a temporary Postgres
//! (via `IsolatedTestDb`) and exposes an HTTP+SSE client that matches the
//! wire contract the SvelteKit and Dioxus clients use. The goal is to give
//! agents and CI a fast, reliable way to catch regressions across the full
//! request → SSE → reactor → worker → DB → reactor → SSE loop without
//! Playwright.
//!
//! Auto-registration: every `#[forge::*]` handler defined in a crate that
//! links against `forge-harness` is picked up via `inventory`. Tests in
//! `crates/forge-harness/tests/*.rs` only need to define their handlers
//! inline (with `#[forge::query]` etc.) and start the app.
//!
//! ## Quick start
//!
//! ```ignore
//! #[forge::query(auth = "none", tables("widgets"))]
//! pub async fn list_widgets(_ctx: &QueryContext) -> Result<Vec<String>> {
//!     Ok(vec!["a".into(), "b".into()])
//! }
//!
//! #[tokio::test]
//! async fn lists_widgets() {
//!     let app = forge_harness::HarnessApp::start("lists_widgets").await.unwrap();
//!     let result: Vec<String> = app.client().call("list_widgets", ()).await.unwrap();
//!     assert_eq!(result, vec!["a", "b"]);
//! }
//! ```

mod app;
mod client;
mod error;
mod sse;

pub use app::{HarnessApp, HarnessAppBuilder};
pub use client::{HarnessClient, RpcEnvelope};
pub use error::HarnessError;
pub use sse::{HarnessSession, SseEvent};

/// Convenience result type for harness operations.
pub type Result<T> = std::result::Result<T, HarnessError>;

//! Automatic function registration via the `inventory` crate.
//!
//! Each `#[forge::query]`, `#[forge::mutation]`, etc. macro emits an
//! `inventory::submit!` call that registers a factory function. Calling
//! `ForgeBuilder::auto_register()` collects all submitted entries and
//! registers them with the appropriate registry.
//!
//! A single `AutoHandler` type replaces the previous per-kind structs.
//! The closure receives a `&mut HandlerRegistries` and routes itself to
//! the correct sub-registry, so there is only one `inventory::collect!`
//! call regardless of how many handler kinds are in use.

use forge_runtime::function::FunctionRegistry;

#[cfg(feature = "cron")]
use forge_runtime::cron::CronRegistry;
#[cfg(feature = "daemons")]
use forge_runtime::daemon::DaemonRegistry;
#[cfg(feature = "jobs")]
use forge_runtime::jobs::JobRegistry;
#[cfg(feature = "gateway")]
use forge_runtime::mcp::McpToolRegistry;
#[cfg(feature = "gateway")]
use forge_runtime::webhook::WebhookRegistry;
#[cfg(feature = "workflows")]
use forge_runtime::workflow::WorkflowRegistry;

/// All registries bundled into one struct so a single `AutoHandler` closure
/// can target the right sub-registry without needing a separate inventory type
/// per handler kind.
pub struct HandlerRegistries {
    /// Query and mutation handlers.
    pub functions: FunctionRegistry,
    #[cfg(feature = "jobs")]
    /// Background job handlers.
    pub jobs: JobRegistry,
    #[cfg(feature = "cron")]
    /// Scheduled cron handlers.
    pub crons: CronRegistry,
    #[cfg(feature = "workflows")]
    /// Durable workflow handlers.
    pub workflows: WorkflowRegistry,
    #[cfg(feature = "daemons")]
    /// Long-running daemon handlers.
    pub daemons: DaemonRegistry,
    #[cfg(feature = "gateway")]
    /// Inbound webhook handlers.
    pub webhooks: WebhookRegistry,
    #[cfg(feature = "gateway")]
    /// MCP tool handlers.
    pub mcp_tools: McpToolRegistry,
}

/// A single auto-registration entry.
///
/// Each `#[forge::query]` / `#[forge::mutation]` / … macro emits one of these
/// via `inventory::submit!`. `auto_register_all` iterates every submitted entry
/// and calls the enclosed closure.
pub struct AutoHandler(pub fn(&mut HandlerRegistries));

inventory::collect!(AutoHandler);

/// Register all auto-discovered handlers into the provided registries.
pub fn auto_register_all(registries: &mut HandlerRegistries) {
    for entry in inventory::iter::<AutoHandler> {
        (entry.0)(registries);
    }
}

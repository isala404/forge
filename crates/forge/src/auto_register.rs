//! Automatic function registration via the `inventory` crate.

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
    pub functions: FunctionRegistry,
    #[cfg(feature = "jobs")]
    pub jobs: JobRegistry,
    #[cfg(feature = "cron")]
    pub crons: CronRegistry,
    #[cfg(feature = "workflows")]
    pub workflows: WorkflowRegistry,
    #[cfg(feature = "daemons")]
    pub daemons: DaemonRegistry,
    #[cfg(feature = "gateway")]
    pub webhooks: WebhookRegistry,
    #[cfg(feature = "gateway")]
    pub mcp_tools: McpToolRegistry,
}

/// A single auto-registration entry emitted by each `#[forge::*]` macro via `inventory::submit!`.
pub struct AutoHandler(pub fn(&mut HandlerRegistries));

inventory::collect!(AutoHandler);

/// Register all auto-discovered handlers.
pub fn auto_register_all(registries: &mut HandlerRegistries) {
    for entry in inventory::iter::<AutoHandler> {
        (entry.0)(registries);
    }
}

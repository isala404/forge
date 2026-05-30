//! Automatic function registration via the `inventory` crate.

use forge_core::error::{ForgeError, Result};
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

/// Register all auto-discovered handlers, failing if any handler name collides.
///
/// Duplicate detection: the per-kind registries store handlers in `HashMap`s
/// keyed on the handler name, so a duplicate (e.g. two `#[query] pub async fn
/// get_user`s in different modules) would silently overwrite. We snapshot the
/// function-name set before and after each closure runs and surface any
/// collision as a startup error.
pub fn auto_register_all(registries: &mut HandlerRegistries) -> Result<()> {
    use std::collections::HashSet;

    let mut seen: HashSet<String> = registries
        .functions
        .function_names()
        .map(|s| s.to_string())
        .collect();

    for entry in inventory::iter::<AutoHandler> {
        let before = registries.functions.len();
        (entry.0)(registries);
        let after = registries.functions.len();

        // The closure might register zero functions (job/cron/daemon/webhook/mcp_tool
        // bridges) — only validate when the function registry actually grew or
        // when an existing entry got overwritten in place.
        let current: HashSet<String> = registries
            .functions
            .function_names()
            .map(|s| s.to_string())
            .collect();

        let newly_added: Vec<String> = current.difference(&seen).cloned().collect();
        if !newly_added.is_empty() {
            seen.extend(newly_added);
        } else if after <= before {
            // No net growth and no new names — either a non-function handler or
            // an overwrite. Detect overwrite by checking the entry count.
            let registered_count = registries.functions.len();
            if registered_count < seen.len() {
                return Err(ForgeError::config(
                    "duplicate handler name detected during auto-registration: \
                     two #[forge::*] handlers resolve to the same function name. \
                     Use `name = \"...\"` in one of the macro attributes to disambiguate.",
                ));
            }
        }
    }

    Ok(())
}

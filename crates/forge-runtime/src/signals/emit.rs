//! Process-wide signal emission helpers for background executions.
//!
//! The RPC path threads a [`SignalsCollector`] through [`FunctionRouter`]
//! directly, but jobs, crons, workflows, daemons, webhooks, and middleware
//! don't have that plumbing. This module lets them emit signals without
//! changing their call signatures.
//!
//! Callers use the thin helpers below ([`emit_server_execution`],
//! [`emit_diagnostic`], [`emit_raw`]). Emission is a no-op when signals are
//! disabled or no global collector has been installed, so handlers don't
//! need to branch on configuration.

use std::sync::OnceLock;

use forge_core::signals::SignalEvent;
use tracing::debug;

use super::collector::SignalsCollector;

/// Installed once at gateway startup. `None` means signals are disabled.
static GLOBAL_COLLECTOR: OnceLock<Option<SignalsCollector>> = OnceLock::new();

/// Install the process-wide collector. Idempotent: the first installation
/// wins (subsequent calls are no-ops). Pass `None` to explicitly disable.
pub fn install(collector: Option<SignalsCollector>) {
    if GLOBAL_COLLECTOR.set(collector).is_err() {
        debug!("signals::emit global collector already installed");
    }
}

/// Check if a collector is installed and active.
pub fn is_installed() -> bool {
    matches!(GLOBAL_COLLECTOR.get(), Some(Some(_)))
}

fn collector() -> Option<&'static SignalsCollector> {
    GLOBAL_COLLECTOR.get().and_then(|c| c.as_ref())
}

/// Emit a pre-built event. No-op if no collector is installed.
pub fn emit_raw(event: SignalEvent) {
    if let Some(c) = collector() {
        c.try_send(event);
    }
}

/// Emit a server-initiated execution event (job/cron/workflow/webhook/daemon).
///
/// `kind` values: `"job"`, `"cron"`, `"workflow"`, `"workflow_step"`,
/// `"webhook"`, `"daemon"`. `name` is the function/handler name.
pub fn emit_server_execution(
    name: &str,
    kind: &str,
    duration_ms: i32,
    success: bool,
    error_message: Option<String>,
) {
    if let Some(c) = collector() {
        c.try_send(SignalEvent::server_execution(
            name,
            kind,
            duration_ms,
            success,
            error_message,
        ));
    }
}

/// Emit a diagnostic event (auth failure, rate limit hit, panic, etc.) with
/// optional request context. Stored as a `track` event so it shows up in the
/// standard dashboards under the given event name.
#[allow(clippy::too_many_arguments)]
pub fn emit_diagnostic(
    event_name: &str,
    properties: serde_json::Value,
    client_ip: Option<String>,
    user_agent: Option<String>,
    visitor_id: Option<String>,
    user_id: Option<uuid::Uuid>,
    is_bot: bool,
) {
    if let Some(c) = collector() {
        c.try_send(SignalEvent::diagnostic(
            event_name, properties, client_ip, user_agent, visitor_id, user_id, is_bot,
        ));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn emit_is_noop_without_installed_collector() {
        // On a fresh process, GLOBAL_COLLECTOR may be unset. These calls
        // should simply not panic. We don't touch GLOBAL_COLLECTOR here so
        // other tests in this crate aren't affected.
        emit_server_execution("not_installed", "job", 10, true, None);
        emit_diagnostic(
            "not_installed",
            serde_json::json!({}),
            None,
            None,
            None,
            None,
            false,
        );
        emit_raw(SignalEvent::server_execution("x", "job", 1, true, None));
    }
}

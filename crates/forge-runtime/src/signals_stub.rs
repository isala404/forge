//! No-op signals stub used when the `gateway` feature is disabled.
//!
//! Always-on modules (rate_limit, jobs, daemon) call `crate::signals::emit_*`
//! to record diagnostic and execution events. Without a gateway there's no
//! ingestion endpoint, so emissions are dropped at compile time.

use forge_core::signals::SignalEvent;

#[inline]
pub fn emit_raw(_event: SignalEvent) {}

#[inline]
pub fn emit_server_execution(
    _name: &str,
    _kind: &str,
    _duration_ms: i32,
    _success: bool,
    _error_message: Option<String>,
) {
}

#[inline]
#[allow(clippy::too_many_arguments)]
pub fn emit_diagnostic(
    _event_name: &str,
    _properties: serde_json::Value,
    _client_ip: Option<String>,
    _user_agent: Option<String>,
    _visitor_id: Option<String>,
    _user_id: Option<uuid::Uuid>,
    _is_bot: bool,
) {
}

/// Stub `install` — accepts `None` always, ignores `Some` (the
/// `SignalsCollector` type doesn't exist without `gateway`).
#[inline]
pub fn install_global(_: Option<()>) {}

/// Re-export of the `bot` detection helper, since job/daemon code paths
/// reference it. Pure function, no infrastructure dependency.
pub mod bot {
    /// Without GA tracking infrastructure, we can't detect bots — return false.
    #[inline]
    pub fn is_bot(_user_agent: Option<&str>) -> bool {
        false
    }
}

/// Stub visitor module — produces deterministic empty IDs when called.
pub mod visitor {
    pub fn generate_visitor_id(
        _client_ip: Option<&str>,
        _user_agent: Option<&str>,
        _server_secret: &str,
    ) -> String {
        String::new()
    }
}

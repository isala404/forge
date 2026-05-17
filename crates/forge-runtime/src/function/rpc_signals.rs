//! RPC auto-capture signals: bridges the function router to the signals
//! collector so every successful or failed RPC call is recorded as a signal
//! event without each handler having to opt in.

#![cfg(feature = "gateway")]

use std::time::Duration;

use forge_core::{AuthContext, RequestMetadata, signals::SignalEvent};

use crate::signals::{SignalsCollector, bot::is_bot, visitor::generate_visitor_id};

/// Captured request/auth metadata required to emit an RPC signal after the
/// handler runs. Stored before the handler consumes auth/request.
pub struct RpcSignalContext {
    user_id: Option<uuid::Uuid>,
    tenant_id: Option<uuid::Uuid>,
    correlation_id: Option<String>,
    client_ip: Option<String>,
    user_agent: Option<String>,
}

impl RpcSignalContext {
    pub fn capture(auth: &AuthContext, request: &RequestMetadata) -> Self {
        Self {
            user_id: auth.user_id(),
            tenant_id: auth.tenant_id(),
            correlation_id: request.correlation_id().map(str::to_string),
            client_ip: request.client_ip().map(str::to_string),
            user_agent: request.user_agent().map(str::to_string),
        }
    }
}

/// Emit RPC signal events. Holds the collector handle and the server secret
/// used to derive a stable, GDPR-friendly visitor id.
pub struct RpcSignalsEmitter {
    collector: SignalsCollector,
    server_secret: String,
}

impl RpcSignalsEmitter {
    pub fn new(collector: SignalsCollector, server_secret: String) -> Self {
        Self {
            collector,
            server_secret,
        }
    }

    pub fn emit(
        &self,
        function_name: &str,
        function_kind: &str,
        duration: Duration,
        success: bool,
        ctx: RpcSignalContext,
    ) {
        let bot = is_bot(ctx.user_agent.as_deref());
        let visitor_id = ctx.client_ip.as_ref().map(|_| {
            generate_visitor_id(
                ctx.client_ip.as_deref(),
                ctx.user_agent.as_deref(),
                &self.server_secret,
            )
        });

        let event = SignalEvent::rpc_call(
            function_name,
            function_kind,
            duration.as_millis() as i32,
            success,
            ctx.user_id,
            ctx.tenant_id,
            ctx.correlation_id,
            ctx.client_ip,
            ctx.user_agent,
            visitor_id,
            bot,
        );
        self.collector.try_send(event);
    }
}

//! Structured logging for function execution outcomes.
//!
//! Pulled out of [`super::router::FunctionRouter`] so the router only owns
//! dispatch concerns. The logger is a pure function over the execution
//! outcome and the function's configured log level.

use std::time::Duration;

use forge_core::{FunctionInfo, FunctionKind, LogLevel};
use serde_json::Value;
use tracing::{debug, error, info, trace, warn};

/// Resolve the effective log level for a function: explicit override on the
/// function info wins, otherwise mutations/webhooks default to `Info` and
/// queries default to `Debug` (high volume).
pub fn level_for(info: Option<&FunctionInfo>) -> LogLevel {
    info.map(|i| {
        i.log_level.unwrap_or(match i.kind {
            FunctionKind::Mutation => LogLevel::Info,
            FunctionKind::Query => LogLevel::Debug,
            FunctionKind::Webhook => LogLevel::Info,
            _ => LogLevel::Info,
        })
    })
    .unwrap_or(LogLevel::Info)
}

/// Emit a structured log line for a function execution.
///
/// Failures always log at `error` regardless of the configured level so that
/// operator dashboards never miss a failed call. Successes log at the
/// configured level. The input payload is logged at `debug` so callers can
/// opt out of payload visibility by raising the global filter.
#[allow(clippy::too_many_arguments)]
pub fn log_completion(
    log_level: LogLevel,
    function_name: &str,
    kind: &str,
    input: &Value,
    duration: Duration,
    success: bool,
    error_msg: Option<&str>,
) {
    if !success {
        error!(
            function = function_name,
            kind = kind,
            duration_ms = duration.as_millis() as u64,
            error = error_msg,
            "Function failed"
        );
        debug!(
            function = function_name,
            input = %input,
            "Function input"
        );
        return;
    }

    macro_rules! log_fn {
        ($level:ident) => {{
            $level!(
                function = function_name,
                kind = kind,
                duration_ms = duration.as_millis() as u64,
                "Function executed"
            );
            debug!(
                function = function_name,
                input = %input,
                "Function input"
            );
        }};
    }

    match log_level {
        LogLevel::Off => {}
        LogLevel::Error => log_fn!(error),
        LogLevel::Warn => log_fn!(warn),
        LogLevel::Info => log_fn!(info),
        LogLevel::Debug => log_fn!(debug),
        LogLevel::Trace => log_fn!(trace),
        _ => log_fn!(trace),
    }
}

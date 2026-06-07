//! Automatic observability: every primitive op runs in a `tracing` span
//! `forge.<primitive>.<op>` and emits `metrics`. Spans carry only metadata (key
//! *hashes*, sizes, counts, outcome) — never raw keys, payloads, or tokens.

use crate::error::{ForgeError, Result};
use std::future::Future;
use std::time::Instant;
use tracing::Instrument;

/// Stable, secret-free variant label for spans and metrics. Never the message.
pub(crate) fn error_variant(e: &ForgeError) -> &'static str {
    match e {
        ForgeError::Config(_) => "config",
        ForgeError::Unavailable(_) => "unavailable",
        ForgeError::NotFound => "not_found",
        ForgeError::Precondition(_) => "precondition",
        ForgeError::Limit(_) => "limit",
        ForgeError::Invalid(_) => "invalid",
        ForgeError::Backend { .. } => "backend",
    }
}

/// `"ok"` on success, else the error variant label.
pub(crate) fn outcome_str<T>(r: &Result<T>) -> &'static str {
    match r {
        Ok(_) => "ok",
        Err(e) => error_variant(e),
    }
}

/// Emit op counter + latency histogram, plus an error counter on failure.
fn record_metrics(
    primitive: &'static str,
    op: &'static str,
    outcome: &'static str,
    started: Instant,
) {
    metrics::counter!("forge_ops_total", "primitive" => primitive, "op" => op, "outcome" => outcome)
        .increment(1);
    metrics::histogram!("forge_op_duration_seconds", "primitive" => primitive, "op" => op)
        .record(started.elapsed().as_secs_f64());
    if outcome != "ok" {
        metrics::counter!("forge_errors_total", "primitive" => primitive, "op" => op, "variant" => outcome)
            .increment(1);
    }
}

/// Run `fut` inside `span`, record outcome on the span, and emit metrics. The
/// span MUST pre-declare empty `outcome` and `error.variant` fields; op-specific
/// fields are recorded by the future itself via `tracing::Span::current()`.
pub(crate) async fn instrument<T, F>(
    primitive: &'static str,
    op: &'static str,
    span: tracing::Span,
    fut: F,
) -> Result<T>
where
    F: Future<Output = Result<T>>,
{
    let started = Instant::now();
    let result = fut.instrument(span.clone()).await;
    let outcome = outcome_str(&result);
    span.record("outcome", outcome);
    if let Err(e) = &result {
        span.record("error.variant", error_variant(e));
    }
    record_metrics(primitive, op, outcome, started);
    result
}

#[cfg(feature = "otel")]
mod otel;
#[cfg(feature = "otel")]
pub use otel::install_otlp;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_variants_have_stable_labels() {
        assert_eq!(error_variant(&ForgeError::NotFound), "not_found");
        assert_eq!(error_variant(&ForgeError::invalid("x")), "invalid");
        assert_eq!(error_variant(&ForgeError::limit("x")), "limit");
        assert_eq!(error_variant(&ForgeError::unavailable("x")), "unavailable");
        assert_eq!(
            error_variant(&ForgeError::precondition("x")),
            "precondition"
        );
        assert_eq!(error_variant(&ForgeError::config("x")), "config");
        assert_eq!(error_variant(&ForgeError::backend("x")), "backend");
    }

    #[tokio::test]
    async fn instrument_passes_through_ok_and_err() {
        let ok: Result<u8> = instrument("kv", "get", tracing::info_span!("forge.kv.get"), async {
            Ok(7u8)
        })
        .await;
        assert_eq!(ok.expect("ok"), 7);

        let err: Result<u8> = instrument("kv", "get", tracing::info_span!("forge.kv.get"), async {
            Err(ForgeError::NotFound)
        })
        .await;
        assert!(matches!(err, Err(ForgeError::NotFound)));
    }
}

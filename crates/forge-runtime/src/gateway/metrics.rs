//! HTTP metrics middleware for the gateway.

use std::sync::Arc;
use std::time::Instant;

use axum::{extract::State, middleware::Next, response::Response};
use forge_core::observability::Metric;

use crate::observability::ObservabilityState;

/// State for metrics middleware.
#[derive(Clone)]
pub struct MetricsState {
    /// Observability state for recording metrics.
    pub observability: ObservabilityState,
}

impl MetricsState {
    /// Create a new metrics state.
    pub fn new(observability: ObservabilityState) -> Self {
        Self { observability }
    }
}

/// Metrics middleware that records HTTP request metrics.
///
/// Records the following metrics:
/// - `http_requests_total`: Total number of HTTP requests (counter)
/// - `http_request_duration_seconds`: Request duration (gauge)
/// - `http_errors_total`: Total number of error responses (counter)
pub async fn metrics_middleware(
    State(state): State<Arc<MetricsState>>,
    req: axum::extract::Request,
    next: Next,
) -> Response {
    let start = Instant::now();
    let method = req.method().to_string();
    let path = req.uri().path().to_string();

    // Execute the request
    let response = next.run(req).await;

    let duration = start.elapsed();
    let status = response.status();
    let status_code = status.as_u16().to_string();

    // Record metrics asynchronously
    let obs = state.observability.clone();
    let method_clone = method.clone();
    let path_clone = path.clone();
    let status_clone = status_code.clone();

    tokio::spawn(async move {
        // Record request count
        let mut request_metric = Metric::counter("http_requests_total", 1.0);
        request_metric
            .labels
            .insert("method".to_string(), method_clone.clone());
        request_metric
            .labels
            .insert("path".to_string(), path_clone.clone());
        request_metric
            .labels
            .insert("status".to_string(), status_clone.clone());
        obs.record_metric(request_metric).await;

        // Record request duration
        let mut duration_metric =
            Metric::gauge("http_request_duration_seconds", duration.as_secs_f64());
        duration_metric
            .labels
            .insert("method".to_string(), method_clone.clone());
        duration_metric
            .labels
            .insert("path".to_string(), path_clone.clone());
        obs.record_metric(duration_metric).await;

        // Record errors if status >= 400
        if status.is_client_error() || status.is_server_error() {
            let mut error_metric = Metric::counter("http_errors_total", 1.0);
            error_metric
                .labels
                .insert("method".to_string(), method_clone);
            error_metric.labels.insert("path".to_string(), path_clone);
            error_metric
                .labels
                .insert("status".to_string(), status_clone);
            obs.record_metric(error_metric).await;
        }
    });

    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_metrics_state_new() {
        // Just verify the struct can be created
        // Full test would require database pool
    }
}

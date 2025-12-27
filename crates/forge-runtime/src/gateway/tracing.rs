use uuid::Uuid;

/// Header name for trace ID.
#[allow(dead_code)]
pub const TRACE_ID_HEADER: &str = "X-Trace-Id";
/// Header name for request ID.
#[allow(dead_code)]
pub const REQUEST_ID_HEADER: &str = "X-Request-Id";
/// Header name for parent span ID.
#[allow(dead_code)]
pub const SPAN_ID_HEADER: &str = "X-Span-Id";

/// Request tracing state.
#[derive(Debug, Clone)]
pub struct TracingState {
    /// Unique trace ID for distributed tracing.
    pub trace_id: String,
    /// Unique request ID.
    pub request_id: String,
    /// Parent span ID (if propagated).
    #[allow(dead_code)]
    pub parent_span_id: Option<String>,
    /// When the request started.
    #[allow(dead_code)]
    pub start_time: std::time::Instant,
}

impl TracingState {
    /// Create a new tracing state.
    pub fn new() -> Self {
        Self {
            trace_id: Uuid::new_v4().to_string(),
            request_id: Uuid::new_v4().to_string(),
            parent_span_id: None,
            start_time: std::time::Instant::now(),
        }
    }

    /// Create with an existing trace ID (for propagation).
    pub fn with_trace_id(trace_id: String) -> Self {
        Self {
            trace_id,
            request_id: Uuid::new_v4().to_string(),
            parent_span_id: None,
            start_time: std::time::Instant::now(),
        }
    }

    /// Set parent span ID.
    #[allow(dead_code)]
    pub fn with_parent_span(mut self, span_id: String) -> Self {
        self.parent_span_id = Some(span_id);
        self
    }

    /// Get elapsed time since request start.
    #[allow(dead_code)]
    pub fn elapsed(&self) -> std::time::Duration {
        self.start_time.elapsed()
    }
}

impl Default for TracingState {
    fn default() -> Self {
        Self::new()
    }
}

/// Tracing middleware marker type.
#[derive(Debug, Clone)]
#[allow(dead_code)]
pub struct TracingMiddleware;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tracing_state_new() {
        let state = TracingState::new();
        assert!(!state.trace_id.is_empty());
        assert!(!state.request_id.is_empty());
        assert!(state.parent_span_id.is_none());
    }

    #[test]
    fn test_tracing_state_with_trace_id() {
        let state = TracingState::with_trace_id("trace-123".to_string());
        assert_eq!(state.trace_id, "trace-123");
        assert!(!state.request_id.is_empty());
    }

    #[test]
    fn test_tracing_state_with_parent_span() {
        let state = TracingState::new().with_parent_span("span-456".to_string());
        assert_eq!(state.parent_span_id, Some("span-456".to_string()));
    }

    #[test]
    fn test_tracing_state_elapsed() {
        let state = TracingState::new();
        std::thread::sleep(std::time::Duration::from_millis(10));
        let elapsed = state.elapsed();
        assert!(elapsed.as_millis() >= 10);
    }
}

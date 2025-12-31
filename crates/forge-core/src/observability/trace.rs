use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Trace ID for distributed tracing.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TraceId(String);

impl TraceId {
    /// Create a new random trace ID.
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string().replace('-', ""))
    }

    /// Create from a string.
    pub fn from_string(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Get the trace ID as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for TraceId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for TraceId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Span ID within a trace.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpanId(String);

impl SpanId {
    /// Create a new random span ID.
    pub fn new() -> Self {
        Self(uuid::Uuid::new_v4().to_string().replace('-', "")[..16].to_string())
    }

    /// Create from a string.
    pub fn from_string(s: impl Into<String>) -> Self {
        Self(s.into())
    }

    /// Get the span ID as a string.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Default for SpanId {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Display for SpanId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Span context for propagation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanContext {
    /// Trace ID.
    pub trace_id: TraceId,
    /// Span ID.
    pub span_id: SpanId,
    /// Parent span ID if any.
    pub parent_span_id: Option<SpanId>,
    /// Trace flags (e.g., sampled).
    pub trace_flags: u8,
}

impl SpanContext {
    /// Create a new root context.
    pub fn new_root() -> Self {
        Self {
            trace_id: TraceId::new(),
            span_id: SpanId::new(),
            parent_span_id: None,
            trace_flags: 0x01, // sampled
        }
    }

    /// Create a child context.
    pub fn child(&self) -> Self {
        Self {
            trace_id: self.trace_id.clone(),
            span_id: SpanId::new(),
            parent_span_id: Some(self.span_id.clone()),
            trace_flags: self.trace_flags,
        }
    }

    /// Check if the trace is sampled.
    pub fn is_sampled(&self) -> bool {
        self.trace_flags & 0x01 != 0
    }

    /// Create a W3C traceparent header value.
    pub fn to_traceparent(&self) -> String {
        format!(
            "00-{}-{}-{:02x}",
            self.trace_id, self.span_id, self.trace_flags
        )
    }

    /// Parse from W3C traceparent header.
    pub fn from_traceparent(traceparent: &str) -> Option<Self> {
        let parts: Vec<&str> = traceparent.split('-').collect();
        if parts.len() != 4 || parts[0] != "00" {
            return None;
        }

        let trace_id = TraceId::from_string(parts[1]);
        let span_id = SpanId::from_string(parts[2]);
        let trace_flags = u8::from_str_radix(parts[3], 16).ok()?;

        Some(Self {
            trace_id,
            span_id,
            parent_span_id: None,
            trace_flags,
        })
    }
}

/// Span kind indicating the relationship.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpanKind {
    /// Internal operation.
    #[default]
    Internal,
    /// Server handling a request.
    Server,
    /// Client making a request.
    Client,
    /// Producer sending a message.
    Producer,
    /// Consumer receiving a message.
    Consumer,
}

impl std::fmt::Display for SpanKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Internal => write!(f, "internal"),
            Self::Server => write!(f, "server"),
            Self::Client => write!(f, "client"),
            Self::Producer => write!(f, "producer"),
            Self::Consumer => write!(f, "consumer"),
        }
    }
}

/// Span status.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SpanStatus {
    /// Unset status.
    #[default]
    Unset,
    /// Operation completed successfully.
    Ok,
    /// Operation failed with an error.
    Error,
}

impl std::fmt::Display for SpanStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unset => write!(f, "unset"),
            Self::Ok => write!(f, "ok"),
            Self::Error => write!(f, "error"),
        }
    }
}

/// A trace span.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Span {
    /// Span context.
    pub context: SpanContext,
    /// Span name.
    pub name: String,
    /// Span kind.
    pub kind: SpanKind,
    /// Span status.
    pub status: SpanStatus,
    /// Status message (for errors).
    pub status_message: Option<String>,
    /// Start time.
    pub start_time: chrono::DateTime<chrono::Utc>,
    /// End time.
    pub end_time: Option<chrono::DateTime<chrono::Utc>>,
    /// Attributes.
    pub attributes: HashMap<String, serde_json::Value>,
    /// Events within the span.
    pub events: Vec<SpanEvent>,
    /// Node ID that generated this span.
    pub node_id: Option<uuid::Uuid>,
}

impl Span {
    /// Create a new span.
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            context: SpanContext::new_root(),
            name: name.into(),
            kind: SpanKind::Internal,
            status: SpanStatus::Unset,
            status_message: None,
            start_time: chrono::Utc::now(),
            end_time: None,
            attributes: HashMap::new(),
            events: Vec::new(),
            node_id: None,
        }
    }

    /// Create a child span.
    pub fn child(&self, name: impl Into<String>) -> Self {
        Self {
            context: self.context.child(),
            name: name.into(),
            kind: SpanKind::Internal,
            status: SpanStatus::Unset,
            status_message: None,
            start_time: chrono::Utc::now(),
            end_time: None,
            attributes: HashMap::new(),
            events: Vec::new(),
            node_id: self.node_id,
        }
    }

    /// Set the span kind.
    pub fn with_kind(mut self, kind: SpanKind) -> Self {
        self.kind = kind;
        self
    }

    /// Add an attribute.
    pub fn with_attribute(mut self, key: impl Into<String>, value: impl Serialize) -> Self {
        if let Ok(v) = serde_json::to_value(value) {
            self.attributes.insert(key.into(), v);
        }
        self
    }

    /// Set the node ID.
    pub fn with_node_id(mut self, node_id: uuid::Uuid) -> Self {
        self.node_id = Some(node_id);
        self
    }

    /// Set from a parent context.
    pub fn with_parent(mut self, parent: &SpanContext) -> Self {
        self.context = parent.child();
        self
    }

    /// Add an event.
    pub fn add_event(&mut self, name: impl Into<String>) {
        self.events.push(SpanEvent {
            name: name.into(),
            timestamp: chrono::Utc::now(),
            attributes: HashMap::new(),
        });
    }

    /// End the span successfully.
    pub fn end_ok(&mut self) {
        self.status = SpanStatus::Ok;
        self.end_time = Some(chrono::Utc::now());
    }

    /// End the span with an error.
    pub fn end_error(&mut self, message: impl Into<String>) {
        self.status = SpanStatus::Error;
        self.status_message = Some(message.into());
        self.end_time = Some(chrono::Utc::now());
    }

    /// Get the duration in milliseconds.
    pub fn duration_ms(&self) -> Option<f64> {
        self.end_time
            .map(|end| (end - self.start_time).num_milliseconds() as f64)
    }

    /// Check if the span is complete.
    pub fn is_complete(&self) -> bool {
        self.end_time.is_some()
    }
}

/// An event within a span.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpanEvent {
    /// Event name.
    pub name: String,
    /// Event timestamp.
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Event attributes.
    pub attributes: HashMap<String, serde_json::Value>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_trace_id_generation() {
        let id1 = TraceId::new();
        let id2 = TraceId::new();
        assert_ne!(id1.as_str(), id2.as_str());
        assert!(!id1.as_str().contains('-'));
    }

    #[test]
    fn test_span_context_root() {
        let ctx = SpanContext::new_root();
        assert!(ctx.parent_span_id.is_none());
        assert!(ctx.is_sampled());
    }

    #[test]
    fn test_span_context_child() {
        let parent = SpanContext::new_root();
        let child = parent.child();

        assert_eq!(child.trace_id, parent.trace_id);
        assert_ne!(child.span_id, parent.span_id);
        assert_eq!(child.parent_span_id, Some(parent.span_id));
    }

    #[test]
    fn test_traceparent_roundtrip() {
        let ctx = SpanContext::new_root();
        let header = ctx.to_traceparent();
        let parsed = SpanContext::from_traceparent(&header).unwrap();

        assert_eq!(parsed.trace_id, ctx.trace_id);
        assert_eq!(parsed.span_id, ctx.span_id);
        assert_eq!(parsed.trace_flags, ctx.trace_flags);
    }

    #[test]
    fn test_span_lifecycle() {
        let mut span = Span::new("test_operation")
            .with_kind(SpanKind::Server)
            .with_attribute("http.method", "GET");

        assert!(!span.is_complete());
        assert!(span.duration_ms().is_none());

        span.add_event("started processing");
        span.end_ok();

        assert!(span.is_complete());
        assert!(span.duration_ms().is_some());
        assert_eq!(span.status, SpanStatus::Ok);
    }

    #[test]
    fn test_span_error() {
        let mut span = Span::new("failing_operation");
        span.end_error("Something went wrong");

        assert_eq!(span.status, SpanStatus::Error);
        assert_eq!(
            span.status_message,
            Some("Something went wrong".to_string())
        );
    }

    #[test]
    fn test_child_span() {
        let parent = Span::new("parent");
        let child = parent.child("child");

        assert_eq!(child.context.trace_id, parent.context.trace_id);
        assert_eq!(
            child.context.parent_span_id,
            Some(parent.context.span_id.clone())
        );
    }
}

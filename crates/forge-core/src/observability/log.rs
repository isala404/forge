use std::collections::HashMap;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

/// Log level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize, Default)]
#[serde(rename_all = "lowercase")]
pub enum LogLevel {
    /// Trace level (most verbose).
    Trace,
    /// Debug level.
    Debug,
    /// Info level.
    #[default]
    Info,
    /// Warning level.
    Warn,
    /// Error level.
    Error,
}

/// Error for parsing LogLevel from string.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseLogLevelError(pub String);

impl std::fmt::Display for ParseLogLevelError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid log level: {}", self.0)
    }
}

impl std::error::Error for ParseLogLevelError {}

impl FromStr for LogLevel {
    type Err = ParseLogLevelError;

    fn from_str(s: &str) -> std::result::Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "trace" => Ok(Self::Trace),
            "debug" => Ok(Self::Debug),
            "info" => Ok(Self::Info),
            "warn" | "warning" => Ok(Self::Warn),
            "error" => Ok(Self::Error),
            _ => Err(ParseLogLevelError(s.to_string())),
        }
    }
}

impl std::fmt::Display for LogLevel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Trace => write!(f, "trace"),
            Self::Debug => write!(f, "debug"),
            Self::Info => write!(f, "info"),
            Self::Warn => write!(f, "warn"),
            Self::Error => write!(f, "error"),
        }
    }
}

/// A structured log entry.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LogEntry {
    /// Log level.
    pub level: LogLevel,
    /// Log message.
    pub message: String,
    /// Target (module path).
    pub target: Option<String>,
    /// Structured fields.
    pub fields: HashMap<String, serde_json::Value>,
    /// Timestamp.
    pub timestamp: chrono::DateTime<chrono::Utc>,
    /// Trace ID if part of a trace.
    pub trace_id: Option<String>,
    /// Span ID if part of a span.
    pub span_id: Option<String>,
    /// Node ID that generated this log.
    pub node_id: Option<uuid::Uuid>,
}

impl LogEntry {
    /// Create a new log entry.
    pub fn new(level: LogLevel, message: impl Into<String>) -> Self {
        Self {
            level,
            message: message.into(),
            target: None,
            fields: HashMap::new(),
            timestamp: chrono::Utc::now(),
            trace_id: None,
            span_id: None,
            node_id: None,
        }
    }

    /// Create a trace log.
    pub fn trace(message: impl Into<String>) -> Self {
        Self::new(LogLevel::Trace, message)
    }

    /// Create a debug log.
    pub fn debug(message: impl Into<String>) -> Self {
        Self::new(LogLevel::Debug, message)
    }

    /// Create an info log.
    pub fn info(message: impl Into<String>) -> Self {
        Self::new(LogLevel::Info, message)
    }

    /// Create a warn log.
    pub fn warn(message: impl Into<String>) -> Self {
        Self::new(LogLevel::Warn, message)
    }

    /// Create an error log.
    pub fn error(message: impl Into<String>) -> Self {
        Self::new(LogLevel::Error, message)
    }

    /// Set the target.
    pub fn with_target(mut self, target: impl Into<String>) -> Self {
        self.target = Some(target.into());
        self
    }

    /// Add a field.
    pub fn with_field(mut self, key: impl Into<String>, value: impl Serialize) -> Self {
        if let Ok(v) = serde_json::to_value(value) {
            self.fields.insert(key.into(), v);
        }
        self
    }

    /// Add multiple fields.
    pub fn with_fields(mut self, fields: HashMap<String, serde_json::Value>) -> Self {
        self.fields.extend(fields);
        self
    }

    /// Set the trace ID.
    pub fn with_trace_id(mut self, trace_id: impl Into<String>) -> Self {
        self.trace_id = Some(trace_id.into());
        self
    }

    /// Set the span ID.
    pub fn with_span_id(mut self, span_id: impl Into<String>) -> Self {
        self.span_id = Some(span_id.into());
        self
    }

    /// Set the node ID.
    pub fn with_node_id(mut self, node_id: uuid::Uuid) -> Self {
        self.node_id = Some(node_id);
        self
    }

    /// Check if this log matches a minimum level filter.
    pub fn matches_level(&self, min_level: LogLevel) -> bool {
        self.level >= min_level
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_log_level_from_str() {
        assert_eq!("info".parse::<LogLevel>(), Ok(LogLevel::Info));
        assert_eq!("WARNING".parse::<LogLevel>(), Ok(LogLevel::Warn));
        assert_eq!("warn".parse::<LogLevel>(), Ok(LogLevel::Warn));
        assert!("unknown".parse::<LogLevel>().is_err());
    }

    #[test]
    fn test_log_level_ordering() {
        assert!(LogLevel::Trace < LogLevel::Debug);
        assert!(LogLevel::Debug < LogLevel::Info);
        assert!(LogLevel::Info < LogLevel::Warn);
        assert!(LogLevel::Warn < LogLevel::Error);
    }

    #[test]
    fn test_log_entry_creation() {
        let entry = LogEntry::info("Request processed")
            .with_target("forge::gateway")
            .with_field("duration_ms", 42)
            .with_field("status", 200);

        assert_eq!(entry.level, LogLevel::Info);
        assert_eq!(entry.message, "Request processed");
        assert_eq!(entry.target, Some("forge::gateway".to_string()));
        assert_eq!(
            entry.fields.get("duration_ms"),
            Some(&serde_json::json!(42))
        );
    }

    #[test]
    fn test_log_level_filter() {
        let debug_log = LogEntry::debug("Debug message");
        let info_log = LogEntry::info("Info message");
        let error_log = LogEntry::error("Error message");

        assert!(!debug_log.matches_level(LogLevel::Info));
        assert!(info_log.matches_level(LogLevel::Info));
        assert!(error_log.matches_level(LogLevel::Info));
    }
}

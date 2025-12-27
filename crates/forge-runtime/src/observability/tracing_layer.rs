//! Custom tracing layer that forwards logs to the LogCollector.

use std::sync::Arc;

use forge_core::observability::LogEntry;
use forge_core::LogLevel;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Subscriber};
use tracing_subscriber::layer::Context;
use tracing_subscriber::Layer;

use super::LogCollector;

/// A tracing layer that forwards log events to the LogCollector.
pub struct ForgeTracingLayer {
    collector: Arc<LogCollector>,
}

impl ForgeTracingLayer {
    /// Create a new tracing layer.
    pub fn new(collector: Arc<LogCollector>) -> Self {
        Self { collector }
    }
}

impl<S> Layer<S> for ForgeTracingLayer
where
    S: Subscriber,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        let metadata = event.metadata();
        let level = convert_level(*metadata.level());

        // Skip if below minimum level
        if level < self.collector.min_level() {
            return;
        }

        // Extract message and fields
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);

        let message = visitor.message.unwrap_or_default();

        let mut entry = LogEntry::new(level, message);
        entry.target = Some(metadata.target().to_string());
        entry.fields = visitor.fields;

        // Record asynchronously using a spawned task
        let collector = self.collector.clone();
        tokio::spawn(async move {
            collector.record(entry).await;
        });
    }
}

/// Convert tracing Level to LogLevel.
fn convert_level(level: Level) -> LogLevel {
    match level {
        Level::TRACE => LogLevel::Trace,
        Level::DEBUG => LogLevel::Debug,
        Level::INFO => LogLevel::Info,
        Level::WARN => LogLevel::Warn,
        Level::ERROR => LogLevel::Error,
    }
}

/// Visitor to extract fields from a tracing event.
#[derive(Default)]
struct FieldVisitor {
    message: Option<String>,
    fields: std::collections::HashMap<String, serde_json::Value>,
}

impl Visit for FieldVisitor {
    fn record_debug(&mut self, field: &Field, value: &dyn std::fmt::Debug) {
        let name = field.name();
        if name == "message" {
            self.message = Some(format!("{:?}", value));
        } else {
            self.fields.insert(
                name.to_string(),
                serde_json::Value::String(format!("{:?}", value)),
            );
        }
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        let name = field.name();
        if name == "message" {
            self.message = Some(value.to_string());
        } else {
            self.fields.insert(
                name.to_string(),
                serde_json::Value::String(value.to_string()),
            );
        }
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.fields.insert(
            field.name().to_string(),
            serde_json::Value::Number(value.into()),
        );
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.fields.insert(
            field.name().to_string(),
            serde_json::Value::Number(value.into()),
        );
    }

    fn record_bool(&mut self, field: &Field, value: bool) {
        self.fields
            .insert(field.name().to_string(), serde_json::Value::Bool(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        if let Some(n) = serde_json::Number::from_f64(value) {
            self.fields
                .insert(field.name().to_string(), serde_json::Value::Number(n));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_convert_level() {
        assert_eq!(convert_level(Level::TRACE), LogLevel::Trace);
        assert_eq!(convert_level(Level::DEBUG), LogLevel::Debug);
        assert_eq!(convert_level(Level::INFO), LogLevel::Info);
        assert_eq!(convert_level(Level::WARN), LogLevel::Warn);
        assert_eq!(convert_level(Level::ERROR), LogLevel::Error);
    }
}

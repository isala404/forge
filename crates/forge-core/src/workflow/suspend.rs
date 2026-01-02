use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Reason for workflow suspension.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum SuspendReason {
    /// Workflow is sleeping until a specific time.
    Sleep { wake_at: DateTime<Utc> },
    /// Workflow is waiting for an external event.
    WaitingEvent {
        event_name: String,
        timeout: Option<DateTime<Utc>>,
    },
}

impl SuspendReason {
    /// Get the wake time for this suspension.
    pub fn wake_at(&self) -> Option<DateTime<Utc>> {
        match self {
            Self::Sleep { wake_at } => Some(*wake_at),
            Self::WaitingEvent { timeout, .. } => *timeout,
        }
    }

    /// Check if this is a sleep suspension.
    pub fn is_sleep(&self) -> bool {
        matches!(self, Self::Sleep { .. })
    }

    /// Check if this is an event wait.
    pub fn is_event_wait(&self) -> bool {
        matches!(self, Self::WaitingEvent { .. })
    }

    /// Get the event name if waiting for an event.
    pub fn event_name(&self) -> Option<&str> {
        match self {
            Self::WaitingEvent { event_name, .. } => Some(event_name),
            _ => None,
        }
    }
}

/// A workflow event that can wake suspended workflows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowEvent {
    /// Event ID.
    pub id: Uuid,
    /// Event name/type.
    pub event_name: String,
    /// Correlation ID (typically workflow run ID).
    pub correlation_id: String,
    /// Event payload.
    pub payload: Option<serde_json::Value>,
    /// When the event was created.
    pub created_at: DateTime<Utc>,
}

impl WorkflowEvent {
    /// Create a new workflow event.
    pub fn new(
        event_name: impl Into<String>,
        correlation_id: impl Into<String>,
        payload: Option<serde_json::Value>,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            event_name: event_name.into(),
            correlation_id: correlation_id.into(),
            payload,
            created_at: Utc::now(),
        }
    }

    /// Get the payload as a typed value.
    pub fn payload_as<T: serde::de::DeserializeOwned>(&self) -> Option<T> {
        self.payload
            .as_ref()
            .and_then(|p| serde_json::from_value(p.clone()).ok())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_suspend_reason_sleep() {
        let wake_at = Utc::now() + chrono::Duration::hours(1);
        let reason = SuspendReason::Sleep { wake_at };

        assert!(reason.is_sleep());
        assert!(!reason.is_event_wait());
        assert_eq!(reason.wake_at(), Some(wake_at));
        assert!(reason.event_name().is_none());
    }

    #[test]
    fn test_suspend_reason_event() {
        let timeout = Utc::now() + chrono::Duration::days(7);
        let reason = SuspendReason::WaitingEvent {
            event_name: "payment_confirmed".to_string(),
            timeout: Some(timeout),
        };

        assert!(!reason.is_sleep());
        assert!(reason.is_event_wait());
        assert_eq!(reason.wake_at(), Some(timeout));
        assert_eq!(reason.event_name(), Some("payment_confirmed"));
    }

    #[test]
    fn test_workflow_event_creation() {
        let event = WorkflowEvent::new(
            "order_completed",
            "workflow-123",
            Some(serde_json::json!({"order_id": "ABC123"})),
        );

        assert_eq!(event.event_name, "order_completed");
        assert_eq!(event.correlation_id, "workflow-123");
        assert!(event.payload.is_some());
    }

    #[test]
    fn test_workflow_event_payload_typed() {
        #[derive(Debug, Deserialize, PartialEq)]
        struct OrderData {
            order_id: String,
        }

        let event = WorkflowEvent::new(
            "order_completed",
            "workflow-123",
            Some(serde_json::json!({"order_id": "ABC123"})),
        );

        let data: Option<OrderData> = event.payload_as();
        assert!(data.is_some());
        assert_eq!(
            data.unwrap(),
            OrderData {
                order_id: "ABC123".to_string()
            }
        );
    }
}

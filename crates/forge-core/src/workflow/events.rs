use std::future::Future;

use serde::Serialize;
use uuid::Uuid;

use crate::ForgeError;

/// Trait for sending workflow events.
pub trait WorkflowEventSender: Send + Sync {
    /// Send an event to a workflow.
    fn send_event(
        &self,
        event_name: &str,
        correlation_id: &str,
        payload: Option<serde_json::Value>,
    ) -> impl Future<Output = Result<Uuid, ForgeError>> + Send;
}

/// No-op event sender for contexts without event sending capability.
#[derive(Debug, Clone, Copy)]
pub struct NoOpEventSender;

impl WorkflowEventSender for NoOpEventSender {
    async fn send_event(
        &self,
        _event_name: &str,
        _correlation_id: &str,
        _payload: Option<serde_json::Value>,
    ) -> Result<Uuid, ForgeError> {
        Err(ForgeError::InvalidState(
            "Event sending not available in this context".into(),
        ))
    }
}

/// Helper function to serialize a payload.
pub fn serialize_payload<T: Serialize>(payload: &T) -> Result<serde_json::Value, ForgeError> {
    serde_json::to_value(payload).map_err(|e| ForgeError::Serialization(e.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_noop_sender() {
        let sender = NoOpEventSender;
        let result = sender.send_event("test", "123", None).await;
        assert!(result.is_err());
    }

    #[test]
    fn test_serialize_payload() {
        #[derive(Serialize)]
        struct TestPayload {
            value: i32,
        }

        let payload = TestPayload { value: 42 };
        let result = serialize_payload(&payload);
        assert!(result.is_ok());
        assert_eq!(result.unwrap(), serde_json::json!({"value": 42}));
    }
}

use chrono::{DateTime, Utc};
use sqlx::PgPool;
use uuid::Uuid;

use forge_core::workflow::{WorkflowEvent, WorkflowEventSender};
use forge_core::{ForgeError, Result};

/// Event store for durable workflow events.
pub struct EventStore {
    pool: PgPool,
}

impl EventStore {
    /// Create a new event store.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Send an event to a workflow.
    pub async fn send_event(
        &self,
        event_name: &str,
        correlation_id: &str,
        payload: Option<serde_json::Value>,
    ) -> Result<Uuid> {
        let id = Uuid::new_v4();

        sqlx::query!(
            r#"
            INSERT INTO forge_workflow_events (id, event_name, correlation_id, payload)
            VALUES ($1, $2, $3, $4)
            "#,
            id,
            event_name,
            correlation_id,
            payload as _,
        )
        .execute(&self.pool)
        .await
        .map_err(ForgeError::Database)?;

        // Send notification for immediate processing
        sqlx::query_scalar!(
            "SELECT pg_notify('forge_workflow_wakeup', $1)",
            format!("{}:{}", event_name, correlation_id),
        )
        .fetch_one(&self.pool)
        .await
        .map_err(ForgeError::Database)?;

        tracing::debug!(
            event_id = %id,
            event_name = %event_name,
            correlation_id = %correlation_id,
            "Workflow event sent"
        );

        Ok(id)
    }

    /// Consume an event for a workflow.
    #[allow(clippy::type_complexity)]
    pub async fn consume_event(
        &self,
        event_name: &str,
        correlation_id: &str,
        workflow_run_id: Uuid,
    ) -> Result<Option<WorkflowEvent>> {
        let result = sqlx::query!(
            r#"
                UPDATE forge_workflow_events
                SET consumed_at = NOW(), consumed_by = $3
                WHERE id = (
                    SELECT id FROM forge_workflow_events
                    WHERE event_name = $1 AND correlation_id = $2 AND consumed_at IS NULL
                    ORDER BY created_at ASC LIMIT 1
                    FOR UPDATE SKIP LOCKED
                )
                RETURNING id, event_name, correlation_id, payload, created_at
                "#,
            event_name,
            correlation_id,
            workflow_run_id
        )
        .fetch_optional(&self.pool)
        .await
        .map_err(ForgeError::Database)?;

        Ok(result.map(|row| WorkflowEvent {
            id: row.id,
            event_name: row.event_name,
            correlation_id: row.correlation_id,
            payload: row.payload,
            created_at: row.created_at,
        }))
    }

    /// Check if an event exists for a workflow (without consuming).
    pub async fn has_event(&self, event_name: &str, correlation_id: &str) -> Result<bool> {
        let result = sqlx::query_scalar!(
            r#"
            SELECT COUNT(*) FROM forge_workflow_events
            WHERE event_name = $1 AND correlation_id = $2 AND consumed_at IS NULL
            "#,
            event_name,
            correlation_id
        )
        .fetch_one(&self.pool)
        .await
        .map_err(ForgeError::Database)?;

        Ok(result.unwrap_or(0) > 0)
    }

    /// List pending events for a workflow.
    #[allow(clippy::type_complexity)]
    pub async fn list_pending_events(&self, correlation_id: &str) -> Result<Vec<WorkflowEvent>> {
        let results = sqlx::query!(
            r#"
                SELECT id, event_name, correlation_id, payload, created_at
                FROM forge_workflow_events
                WHERE correlation_id = $1 AND consumed_at IS NULL
                ORDER BY created_at ASC
                "#,
            correlation_id
        )
        .fetch_all(&self.pool)
        .await
        .map_err(ForgeError::Database)?;

        Ok(results
            .into_iter()
            .map(|row| WorkflowEvent {
                id: row.id,
                event_name: row.event_name,
                correlation_id: row.correlation_id,
                payload: row.payload,
                created_at: row.created_at,
            })
            .collect())
    }

    /// Clean up old consumed events.
    pub async fn cleanup_consumed_events(&self, older_than: DateTime<Utc>) -> Result<u64> {
        let result = sqlx::query!(
            r#"
            DELETE FROM forge_workflow_events
            WHERE consumed_at IS NOT NULL AND consumed_at < $1
            "#,
            older_than,
        )
        .execute(&self.pool)
        .await
        .map_err(ForgeError::Database)?;

        Ok(result.rows_affected())
    }
}

impl WorkflowEventSender for EventStore {
    async fn send_event(
        &self,
        event_name: &str,
        correlation_id: &str,
        payload: Option<serde_json::Value>,
    ) -> Result<Uuid> {
        EventStore::send_event(self, event_name, correlation_id, payload).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_event_store_creation() {
        // Just test that the struct can be created
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/test")
            .expect("Failed to create mock pool");

        let _store = EventStore::new(pool);
    }
}

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

        // Wakeup signal only — the scheduler polls for details.
        sqlx::query_scalar!("SELECT pg_notify('forge_workflow_wakeup', $1)", "",)
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
        Self::consume_event_in_conn(
            &mut *self.pool.acquire().await.map_err(ForgeError::Database)?,
            event_name,
            correlation_id,
            workflow_run_id,
        )
        .await
    }

    /// Consume an event on an existing connection (typically inside a transaction).
    ///
    /// Callers that need consume + claim + enqueue to be atomic should open a
    /// transaction and pass the connection here so all three writes share one
    /// transaction boundary — one network round-trip instead of three separate
    /// autocommit statements.
    pub async fn consume_event_in_conn(
        conn: &mut sqlx::PgConnection,
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
        .fetch_optional(&mut *conn)
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

#[cfg(all(test, feature = "testcontainers"))]
#[allow(
    clippy::unwrap_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::disallowed_methods
)]
mod integration_tests {
    use super::*;
    use forge_core::testing::{IsolatedTestDb, TestDatabase};

    async fn setup_db(test_name: &str) -> IsolatedTestDb {
        let base = TestDatabase::from_env()
            .await
            .expect("Failed to create test database");
        let db = base
            .isolated(test_name)
            .await
            .expect("Failed to create isolated db");
        let system_sql = crate::pg::migration::get_all_system_sql();
        db.run_sql(&system_sql)
            .await
            .expect("Failed to apply system schema");
        db
    }

    /// Insert a workflow_run so `consume_event.consumed_by` FK is satisfied.
    async fn make_workflow_run(pool: &PgPool) -> Uuid {
        let id = Uuid::new_v4();
        sqlx::query(
            "INSERT INTO forge_workflow_runs (id, workflow_name, workflow_version, workflow_signature)
             VALUES ($1, 'test_wf', '1', 'sig')",
        )
        .bind(id)
        .execute(pool)
        .await
        .unwrap();
        id
    }

    #[tokio::test]
    async fn send_event_persists_and_returns_id() {
        let db = setup_db("ws_send_event").await;
        let store = EventStore::new(db.pool().clone());

        let id = store
            .send_event(
                "payment.confirmed",
                "order-123",
                Some(serde_json::json!({"amount": 100})),
            )
            .await
            .unwrap();

        let (db_id, payload): (Uuid, Option<serde_json::Value>) = sqlx::query_as(
            "SELECT id, payload FROM forge_workflow_events WHERE correlation_id = 'order-123'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(db_id, id);
        assert_eq!(payload, Some(serde_json::json!({"amount": 100})));
    }

    #[tokio::test]
    async fn send_event_accepts_null_payload() {
        // Signal-only events (no payload) are valid — the column is nullable.
        let db = setup_db("ws_send_null_payload").await;
        let store = EventStore::new(db.pool().clone());

        let id = store.send_event("ping", "corr-1", None).await.unwrap();

        let payload: Option<serde_json::Value> =
            sqlx::query_scalar("SELECT payload FROM forge_workflow_events WHERE id = $1")
                .bind(id)
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert!(payload.is_none());
    }

    #[tokio::test]
    async fn consume_event_returns_none_when_no_match() {
        let db = setup_db("ws_consume_none").await;
        let store = EventStore::new(db.pool().clone());
        let wf_id = make_workflow_run(db.pool()).await;

        let consumed = store
            .consume_event("never_fired", "corr-x", wf_id)
            .await
            .unwrap();
        assert!(consumed.is_none());
    }

    #[tokio::test]
    async fn consume_event_marks_row_consumed_and_returns_it() {
        let db = setup_db("ws_consume_match").await;
        let store = EventStore::new(db.pool().clone());
        let wf_id = make_workflow_run(db.pool()).await;

        let sent_id = store
            .send_event(
                "payment.confirmed",
                "order-1",
                Some(serde_json::json!({"k": "v"})),
            )
            .await
            .unwrap();

        let consumed = store
            .consume_event("payment.confirmed", "order-1", wf_id)
            .await
            .unwrap()
            .expect("event was sent, must be consumable");

        assert_eq!(consumed.id, sent_id);
        assert_eq!(consumed.event_name, "payment.confirmed");
        assert_eq!(consumed.correlation_id, "order-1");
        assert_eq!(consumed.payload, Some(serde_json::json!({"k": "v"})));

        // The row's `consumed_at` and `consumed_by` must be populated.
        let row: (Option<DateTime<Utc>>, Option<Uuid>) = sqlx::query_as(
            "SELECT consumed_at, consumed_by FROM forge_workflow_events WHERE id = $1",
        )
        .bind(sent_id)
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert!(row.0.is_some(), "consumed_at must be set");
        assert_eq!(row.1, Some(wf_id), "consumed_by must record the workflow");
    }

    #[tokio::test]
    async fn consume_event_returns_oldest_first() {
        // Multiple events on the same (name, correlation) — FOR UPDATE SKIP
        // LOCKED with ORDER BY created_at ASC + LIMIT 1 means a single
        // consumer drains them in send order.
        let db = setup_db("ws_consume_order").await;
        let store = EventStore::new(db.pool().clone());
        let wf_id = make_workflow_run(db.pool()).await;

        let first = store
            .send_event("e", "c", Some(serde_json::json!({"n": 1})))
            .await
            .unwrap();
        // Force a real time gap so created_at ordering is well-defined even
        // under coarse-grained clock implementations.
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let _second = store
            .send_event("e", "c", Some(serde_json::json!({"n": 2})))
            .await
            .unwrap();

        let consumed = store.consume_event("e", "c", wf_id).await.unwrap().unwrap();
        assert_eq!(consumed.id, first, "oldest event must be consumed first");
    }

    #[tokio::test]
    async fn consume_event_does_not_re_consume_already_consumed_row() {
        // After one successful consume, the row is gone from the pending set.
        // A second consume on the same (name, correlation) must return None.
        let db = setup_db("ws_consume_once").await;
        let store = EventStore::new(db.pool().clone());
        let wf_id = make_workflow_run(db.pool()).await;

        store.send_event("e", "c", None).await.unwrap();
        let first = store.consume_event("e", "c", wf_id).await.unwrap();
        let second = store.consume_event("e", "c", wf_id).await.unwrap();

        assert!(first.is_some());
        assert!(second.is_none(), "already-consumed row must not re-fire");
    }

    #[tokio::test]
    async fn list_pending_events_includes_only_unconsumed_rows() {
        let db = setup_db("ws_list_pending").await;
        let store = EventStore::new(db.pool().clone());
        let wf_id = make_workflow_run(db.pool()).await;

        store
            .send_event("ev_a", "corr-1", Some(serde_json::json!(1)))
            .await
            .unwrap();
        store
            .send_event("ev_b", "corr-1", Some(serde_json::json!(2)))
            .await
            .unwrap();
        // Different correlation — must be excluded.
        store
            .send_event("ev_a", "corr-2", Some(serde_json::json!(99)))
            .await
            .unwrap();

        let pending = store.list_pending_events("corr-1").await.unwrap();
        assert_eq!(pending.len(), 2);
        assert!(
            pending.iter().all(|e| e.correlation_id == "corr-1"),
            "must filter by correlation_id"
        );

        // After consuming one, only one remains pending.
        store.consume_event("ev_a", "corr-1", wf_id).await.unwrap();
        let pending = store.list_pending_events("corr-1").await.unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].event_name, "ev_b");
    }

    #[tokio::test]
    async fn list_pending_events_orders_by_created_at_ascending() {
        let db = setup_db("ws_list_order").await;
        let store = EventStore::new(db.pool().clone());

        let a = store
            .send_event("e", "c", Some(serde_json::json!("first")))
            .await
            .unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(20)).await;
        let b = store
            .send_event("e", "c", Some(serde_json::json!("second")))
            .await
            .unwrap();

        let pending = store.list_pending_events("c").await.unwrap();
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].id, a);
        assert_eq!(pending[1].id, b);
    }

    #[tokio::test]
    async fn cleanup_consumed_events_deletes_only_old_consumed_rows() {
        // The retention cleanup must spare unconsumed events (they're still
        // needed by waiters) and recent consumed events (they may be useful
        // for debugging).
        let db = setup_db("ws_cleanup").await;
        let store = EventStore::new(db.pool().clone());
        let wf_id = make_workflow_run(db.pool()).await;

        // Send + consume two events.
        store.send_event("e1", "c", None).await.unwrap();
        store.send_event("e2", "c", None).await.unwrap();
        store.consume_event("e1", "c", wf_id).await.unwrap();
        store.consume_event("e2", "c", wf_id).await.unwrap();
        // Send a third that's never consumed.
        store.send_event("e3", "c", None).await.unwrap();

        // Backdate one of the consumed events.
        sqlx::query(
            "UPDATE forge_workflow_events SET consumed_at = NOW() - INTERVAL '1 day'
             WHERE event_name = 'e1'",
        )
        .execute(db.pool())
        .await
        .unwrap();

        let cutoff = Utc::now() - chrono::Duration::hours(1);
        let deleted = store.cleanup_consumed_events(cutoff).await.unwrap();
        assert_eq!(
            deleted, 1,
            "only the backdated consumed row should be deleted"
        );

        // The freshly-consumed e2 and the never-consumed e3 must remain.
        let remaining: i64 = sqlx::query_scalar("SELECT COUNT(*) FROM forge_workflow_events")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(remaining, 2);
    }
}

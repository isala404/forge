use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use tokio::sync::{broadcast, watch};

use forge_core::realtime::Change;

// Reserved Forge-owned NOTIFY channels. Documented here so apps don't squat
// on these names with their own LISTEN/NOTIFY traffic before the runtime
// claims them.
//
// - `forge_changes`           — table change events (this listener)
// - `forge_workflow_wakeup`   — workflow scheduler wakeups
// - `forge_channels`          — RESERVED for ephemeral pub-sub fan-out
// - `forge_auth_revocations`  — RESERVED for cluster-wide auth/role teardown

/// Change listener configuration.
#[derive(Debug, Clone)]
pub struct ListenerConfig {
    /// PostgreSQL channel name for change notifications.
    pub channel: String,
    /// Buffer size for change broadcast.
    pub buffer_size: usize,
}

impl Default for ListenerConfig {
    fn default() -> Self {
        Self {
            channel: "forge_changes".to_string(),
            buffer_size: 1024,
        }
    }
}

/// Listens for database changes via PostgreSQL LISTEN/NOTIFY.
pub struct ChangeListener {
    pool: sqlx::PgPool,
    config: ListenerConfig,
    running: Arc<AtomicBool>,
    change_tx: broadcast::Sender<Change>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
}

impl ChangeListener {
    /// Create a new change listener.
    pub fn new(pool: sqlx::PgPool, config: ListenerConfig) -> Self {
        let (change_tx, _) = broadcast::channel(config.buffer_size);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        Self {
            pool,
            config,
            running: Arc::new(AtomicBool::new(false)),
            change_tx,
            shutdown_tx,
            shutdown_rx,
        }
    }

    /// Subscribe to change notifications.
    pub fn subscribe(&self) -> broadcast::Receiver<Change> {
        self.change_tx.subscribe()
    }

    /// Check if the listener is running.
    pub fn is_running(&self) -> bool {
        self.running.load(Ordering::SeqCst)
    }

    /// Stop the listener.
    pub fn stop(&self) {
        let _ = self.shutdown_tx.send(true);
        self.running.store(false, Ordering::SeqCst);
    }

    /// Run the listener loop.
    pub async fn run(&self) -> forge_core::Result<()> {
        self.running.store(true, Ordering::SeqCst);

        // Create a dedicated listener connection
        let mut listener = sqlx::postgres::PgListener::connect_with(&self.pool)
            .await
            .map_err(forge_core::ForgeError::Database)?;

        // Subscribe to the change channel
        listener
            .listen(&self.config.channel)
            .await
            .map_err(forge_core::ForgeError::Database)?;

        tracing::debug!(channel = %self.config.channel, "Listening for changes");

        let mut shutdown_rx = self.shutdown_rx.clone();

        loop {
            tokio::select! {
                notification = listener.recv() => {
                    match notification {
                        Ok(notification) => {
                            let recv_time = std::time::Instant::now();
                            if let Some(change) = self.parse_notification(notification.payload()) {
                                tracing::trace!(table = %change.table, op = ?change.operation, "Change received");
                                crate::cluster::metrics::record_notification_processed(&change.table);
                                let _ = self.change_tx.send(change);
                                crate::cluster::metrics::record_notification_latency(recv_time.elapsed().as_secs_f64());
                            } else {
                                tracing::debug!(payload = %notification.payload(), "Failed to parse notification");
                            }
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "Error receiving notification");
                            // Try to reconnect
                            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
                        }
                    }
                }
                _ = shutdown_rx.changed() => {
                    if *shutdown_rx.borrow() {
                        tracing::debug!("Change listener shutting down");
                        break;
                    }
                }
            }
        }

        self.running.store(false, Ordering::SeqCst);
        Ok(())
    }

    /// Parse a notification payload into a Change.
    ///
    /// Expected format: `v1:table:OP:row_id[:col1,col2,...]`. The leading
    /// version tag lets future schema bumps coexist with v1 listeners during
    /// a rolling cluster upgrade. Payloads without a recognized version tag
    /// are dropped — they can't be safely interpreted by this build.
    fn parse_notification(&self, payload: &str) -> Option<Change> {
        let body = payload.strip_prefix("v1:")?;
        let parts: Vec<&str> = body.split(':').collect();

        let table = parts.first()?;
        let operation = parts.get(1)?.parse().ok()?;

        let mut change = Change::new(table.to_string(), operation);

        if let Some(&row_id_str) = parts.get(2)
            && let Ok(row_id) = uuid::Uuid::parse_str(row_id_str)
        {
            change = change.with_row_id(row_id);
        }

        if let Some(&col_str) = parts.get(3) {
            let columns: Vec<String> = col_str.split(',').map(|s| s.to_string()).collect();
            change = change.with_columns(columns);
        }

        Some(change)
    }

    /// Manually emit a change (for testing or manual triggering).
    pub fn emit_change(&self, change: Change) {
        let _ = self.change_tx.send(change);
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;
    use forge_core::realtime::ChangeOperation;

    #[test]
    fn test_listener_config_default() {
        let config = ListenerConfig::default();
        assert_eq!(config.channel, "forge_changes");
        assert_eq!(config.buffer_size, 1024);
    }

    #[tokio::test]
    async fn test_parse_notification_insert() {
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let listener = ChangeListener::new(pool, ListenerConfig::default());

        let payload = "v1:projects:INSERT:550e8400-e29b-41d4-a716-446655440000";
        let change = listener.parse_notification(payload).unwrap();

        assert_eq!(change.table, "projects");
        assert_eq!(change.operation, ChangeOperation::Insert);
        assert!(change.row_id.is_some());
    }

    #[tokio::test]
    async fn test_parse_notification_update_with_columns() {
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let listener = ChangeListener::new(pool, ListenerConfig::default());

        let payload = "v1:projects:UPDATE:550e8400-e29b-41d4-a716-446655440000:name,status";
        let change = listener.parse_notification(payload).unwrap();

        assert_eq!(change.table, "projects");
        assert_eq!(change.operation, ChangeOperation::Update);
        assert_eq!(change.changed_columns, vec!["name", "status"]);
    }

    #[tokio::test]
    async fn test_parse_notification_invalid() {
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let listener = ChangeListener::new(pool, ListenerConfig::default());

        let payload = "invalid";
        let change = listener.parse_notification(payload);
        assert!(change.is_none());
    }

    #[tokio::test]
    async fn test_parse_notification_rejects_unversioned() {
        // Drop pre-v1 payloads (or anything without a recognized version tag).
        // A peer running a different Forge version may emit a tag we don't
        // understand; better to drop than to misinterpret.
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let listener = ChangeListener::new(pool, ListenerConfig::default());

        let payload = "projects:INSERT:550e8400-e29b-41d4-a716-446655440000";
        assert!(listener.parse_notification(payload).is_none());

        let payload = "v2:projects:INSERT:550e8400-e29b-41d4-a716-446655440000";
        assert!(listener.parse_notification(payload).is_none());
    }
}

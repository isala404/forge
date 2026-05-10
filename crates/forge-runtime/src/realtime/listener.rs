use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

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
///
/// Tracks the last-seen change log sequence number so reconnects can
/// replay missed changes from `forge_change_log` instead of triggering
/// a full resync of all active subscriptions.
pub struct ChangeListener {
    pool: sqlx::PgPool,
    config: ListenerConfig,
    running: Arc<AtomicBool>,
    change_tx: broadcast::Sender<Change>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    last_seq: AtomicI64,
    /// Set when replay_missed detects the change log was trimmed past our
    /// last_seq. The reactor checks this to trigger an immediate full resync
    /// instead of waiting for the periodic sweep.
    needs_resync: AtomicBool,
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
            last_seq: AtomicI64::new(0),
            needs_resync: AtomicBool::new(false),
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

    /// Last-seen change log sequence number.
    pub fn last_seq(&self) -> i64 {
        self.last_seq.load(Ordering::Relaxed)
    }

    /// Returns true if the listener detected that change log entries were
    /// trimmed past our last-seen position, meaning replay couldn't recover
    /// all missed events. The reactor should trigger an immediate full resync.
    pub fn take_needs_resync(&self) -> bool {
        self.needs_resync.swap(false, Ordering::Relaxed)
    }

    /// Replay changes missed while disconnected by querying the durable
    /// change log. Returns the number of replayed changes, or `None` if
    /// the log read failed (e.g. the table is absent on first boot before
    /// v002 has applied).
    async fn replay_missed(&self) -> Option<usize> {
        use futures_util::stream::TryStreamExt;

        let since = self.last_seq.load(Ordering::Relaxed);
        if since == 0 {
            return None;
        }

        let rows = crate::pg::drain_change_log(&self.pool, since)
            .try_collect::<Vec<_>>()
            .await
            .ok()?;

        let count = rows.len();
        for row in &rows {
            let Ok(operation) = row.op.parse::<forge_core::realtime::ChangeOperation>() else {
                continue;
            };

            let mut change = Change::new(row.table_name.clone(), operation);
            if let Some(rid) = &row.row_id
                && let Ok(uuid) = uuid::Uuid::parse_str(rid)
            {
                change = change.with_row_id(uuid);
            }
            if let Some(cols) = &row.changed_cols {
                let columns: Vec<String> = cols.split(',').map(|s| s.to_string()).collect();
                change = change.with_columns(columns);
            }

            let _ = self.change_tx.send(change);
            self.last_seq.store(row.seq, Ordering::Relaxed);
        }

        if count > 0 {
            tracing::info!(
                replayed = count,
                from_seq = since,
                "Replayed missed changes from log"
            );
        } else if let Ok(Some(min)) = crate::pg::min_seq(&self.pool).await
            && min > since
        {
            // Zero rows but a non-zero high-water mark — retention has
            // trimmed past our position. Trigger a full resync.
            tracing::warn!(
                last_seen = since,
                log_min = min,
                "Change log trimmed past our position, requesting full resync"
            );
            self.needs_resync.store(true, Ordering::Relaxed);
        }

        Some(count)
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
                            if let Some((change, seq)) = self.parse_notification(notification.payload()) {
                                tracing::trace!(table = %change.table, op = ?change.operation, seq, "Change received");
                                crate::cluster::metrics::record_notification_processed(&change.table);
                                let _ = self.change_tx.send(change);
                                if seq > 0 {
                                    self.last_seq.store(seq, Ordering::Relaxed);
                                }
                                crate::cluster::metrics::record_notification_latency(recv_time.elapsed().as_secs_f64());
                            } else {
                                tracing::debug!(payload = %notification.payload(), "Failed to parse notification");
                            }
                        }
                        Err(e) => {
                            tracing::debug!(error = %e, "Error receiving notification, attempting recovery");
                            self.replay_missed().await;
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

    /// Parse a notification payload into a Change and optional sequence number.
    ///
    /// v1 format: `v1:table:OP:row_id[:col1,col2,...][#seq]`
    /// The `#seq` suffix is appended by the v002 trigger and enables gap
    /// recovery from `forge_change_log`. Pre-v002 payloads without `#seq`
    /// still parse correctly (seq = 0).
    fn parse_notification(&self, payload: &str) -> Option<(Change, i64)> {
        // Split off the optional #seq suffix
        let (body_with_version, seq) = match payload.rsplit_once('#') {
            Some((prefix, seq_str)) => {
                let seq = seq_str.parse::<i64>().unwrap_or(0);
                (prefix, seq)
            }
            None => (payload, 0),
        };

        let body = body_with_version.strip_prefix("v1:")?;
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

        Some((change, seq))
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
        let (change, seq) = listener.parse_notification(payload).unwrap();

        assert_eq!(change.table, "projects");
        assert_eq!(change.operation, ChangeOperation::Insert);
        assert!(change.row_id.is_some());
        assert_eq!(seq, 0);
    }

    #[tokio::test]
    async fn test_parse_notification_with_seq() {
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let listener = ChangeListener::new(pool, ListenerConfig::default());

        let payload = "v1:projects:INSERT:550e8400-e29b-41d4-a716-446655440000#42";
        let (change, seq) = listener.parse_notification(payload).unwrap();

        assert_eq!(change.table, "projects");
        assert_eq!(change.operation, ChangeOperation::Insert);
        assert!(change.row_id.is_some());
        assert_eq!(seq, 42);
    }

    #[tokio::test]
    async fn test_parse_notification_update_with_columns_and_seq() {
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let listener = ChangeListener::new(pool, ListenerConfig::default());

        let payload = "v1:projects:UPDATE:550e8400-e29b-41d4-a716-446655440000:name,status#1337";
        let (change, seq) = listener.parse_notification(payload).unwrap();

        assert_eq!(change.table, "projects");
        assert_eq!(change.operation, ChangeOperation::Update);
        assert_eq!(change.changed_columns, vec!["name", "status"]);
        assert_eq!(seq, 1337);
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
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let listener = ChangeListener::new(pool, ListenerConfig::default());

        let payload = "projects:INSERT:550e8400-e29b-41d4-a716-446655440000";
        assert!(listener.parse_notification(payload).is_none());

        let payload = "v2:projects:INSERT:550e8400-e29b-41d4-a716-446655440000";
        assert!(listener.parse_notification(payload).is_none());
    }

    #[tokio::test]
    async fn test_last_seq_tracking() {
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        let listener = ChangeListener::new(pool, ListenerConfig::default());

        assert_eq!(listener.last_seq(), 0);

        let payload = "v1:projects:INSERT:550e8400-e29b-41d4-a716-446655440000#99";
        let (_, seq) = listener.parse_notification(payload).unwrap();
        listener.last_seq.store(seq, Ordering::Relaxed);

        assert_eq!(listener.last_seq(), 99);
    }
}

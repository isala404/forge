use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use tokio::sync::{broadcast, watch};

use crate::pg::PgNotifyBus;
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

    /// Mark that a full resync is needed (e.g. after broadcast lag).
    pub fn set_needs_resync(&self) {
        self.needs_resync.store(true, Ordering::Relaxed);
    }

    /// Replay changes missed while disconnected by querying the durable
    /// change log. Returns the number of replayed changes, or `None` if
    /// the log read failed (e.g. the table is absent on first boot before
    /// the system schema has applied).
    async fn replay_missed(&self) -> Option<usize> {
        use futures_util::stream::TryStreamExt;

        let since = self.last_seq.load(Ordering::Relaxed);
        if since == 0 {
            return None;
        }

        let rows = match crate::pg::drain_change_log(&self.pool, since)
            .try_collect::<Vec<_>>()
            .await
        {
            Ok(rows) => rows,
            Err(e) => {
                tracing::warn!(error = %e, since_seq = since, "Failed to replay missed changes from log");
                self.needs_resync.store(true, Ordering::Relaxed);
                return Some(0);
            }
        };

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
    pub async fn run(&self, bus: &PgNotifyBus) -> forge_core::Result<()> {
        // Snapshot last_seq BEFORE subscribing. If we subscribed first and
        // then queried max_seq, a NOTIFY carrying a seq <= the snapshot could
        // arrive in the broadcast buffer between subscribe and max_seq; the
        // recv loop's dedup check (`seq <= last_seq`) would then silently
        // discard real events that were appended after we sampled max_seq.
        // Sampling first means every seq the buffer can later deliver is
        // strictly greater than `last_seq`, so the dedup check only filters
        // genuinely replayed entries.
        if self.last_seq.load(Ordering::Relaxed) == 0
            && let Ok(Some(seq)) = crate::pg::max_seq(&self.pool).await
        {
            self.last_seq.store(seq, Ordering::Relaxed);
        }

        // Snapshot the reconnect generation BEFORE subscribing to payloads so
        // a reconnect that happens after this point is visible as a strictly
        // greater value. Without the snapshot, the initial-connect tick
        // (generation 1) would look like a reconnect to a brand-new boot.
        let mut reconnect_rx = bus.subscribe_reconnects();
        let initial_generation = *reconnect_rx.borrow();

        let Some(mut rx) = bus.subscribe(&self.config.channel) else {
            return Err(forge_core::ForgeError::config(format!(
                "PgNotifyBus not configured for channel '{}'",
                self.config.channel
            )));
        };

        self.running.store(true, Ordering::SeqCst);

        tracing::debug!(channel = %self.config.channel, "Listening for changes");

        let mut shutdown_rx = self.shutdown_rx.clone();

        loop {
            tokio::select! {
                _ = reconnect_rx.changed() => {
                    let current = *reconnect_rx.borrow();
                    if current > initial_generation {
                        // PgNotifyBus reconnected after we attached. Any
                        // NOTIFY emitted while the connection was down is
                        // gone — replay from the durable change log to
                        // close the gap. `replay_missed` is a no-op when
                        // last_seq is still 0 (no prior progress) and sets
                        // needs_resync if the log was trimmed past us.
                        tracing::info!(
                            generation = current,
                            "PgNotifyBus reconnected; replaying missed changes"
                        );
                        self.replay_missed().await;
                    }
                }
                result = rx.recv() => {
                    match result {
                        Ok(payload) => {
                            let recv_time = std::time::Instant::now();
                            if let Some((change, seq)) = self.parse_notification(&payload) {
                                // Skip already-processed seqs to prevent
                                // double-processing during the seed window.
                                if seq > 0 && seq <= self.last_seq.load(Ordering::Relaxed) {
                                    continue;
                                }
                                tracing::trace!(table = %change.table, op = ?change.operation, seq, "Change received");
                                crate::cluster::metrics::record_notification_processed(&change.table);
                                let _ = self.change_tx.send(change);
                                if seq > 0 {
                                    self.last_seq.store(seq, Ordering::Relaxed);
                                }
                                crate::cluster::metrics::record_notification_latency(recv_time.elapsed().as_secs_f64());
                            } else {
                                tracing::debug!(payload = %payload, "Failed to parse notification");
                            }
                        }
                        Err(broadcast::error::RecvError::Lagged(n)) => {
                            tracing::debug!(missed = n, "Change listener lagged, attempting recovery");
                            self.replay_missed().await;
                        }
                        Err(broadcast::error::RecvError::Closed) => {
                            tracing::debug!("Change listener shutting down");
                            break;
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
    /// Row-level format: `v1:table:OP:row_id[:col1,col2,...][#seq]`
    /// Statement-level format: `v1s:table:OP`
    ///
    /// The `#seq` suffix is appended by the `forge_notify_change` trigger and
    /// enables gap recovery from `forge_change_log`. Payloads without `#seq`
    /// still parse correctly (seq = 0). Statement-level payloads never
    /// carry a seq (no change-log row is written).
    fn parse_notification(&self, payload: &str) -> Option<(Change, i64)> {
        if let Some(body) = payload.strip_prefix("v1s:") {
            let parts: Vec<&str> = body.split(':').collect();
            let table = parts.first()?;
            let operation = parts.get(1)?.parse().ok()?;
            return Some((Change::new(table.to_string(), operation), 0));
        }

        // Split off the optional #seq suffix
        let (body_with_version, seq) = match payload.rsplit_once('#') {
            Some((prefix, seq_str)) => match seq_str.parse::<i64>() {
                Ok(seq) => (prefix, seq),
                Err(_) => {
                    tracing::warn!(payload = %payload, "Malformed seq in notification, triggering resync");
                    self.needs_resync.store(true, Ordering::Relaxed);
                    return None;
                }
            },
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

    /// Manually emit a change (for testing).
    #[cfg(test)]
    pub(crate) fn emit_change(&self, change: Change) {
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

    // --- needs_resync semantics + parse edge cases ---

    fn make_listener() -> ChangeListener {
        let pool = sqlx::PgPool::connect_lazy("postgres://localhost/test").unwrap();
        ChangeListener::new(pool, ListenerConfig::default())
    }

    #[tokio::test]
    async fn parse_notification_with_malformed_seq_sets_needs_resync() {
        // A `#suffix` that isn't a number is treated as gap-recovery failure:
        // we drop the notification AND raise needs_resync so the reactor
        // schedules a full resync. Silently parsing a bad seq would advance
        // last_seq past real data and hide writes.
        let listener = make_listener();
        assert!(!listener.take_needs_resync());

        let payload = "v1:projects:INSERT:550e8400-e29b-41d4-a716-446655440000#notanumber";
        assert!(listener.parse_notification(payload).is_none());
        assert!(listener.take_needs_resync(), "must request resync");
    }

    #[tokio::test]
    async fn take_needs_resync_is_one_shot() {
        // The flag is consumed by `take_needs_resync` — the reactor reads
        // it once and resets, so a second read after a single trip must
        // return false.
        let listener = make_listener();
        listener.needs_resync.store(true, Ordering::Relaxed);

        assert!(listener.take_needs_resync());
        assert!(!listener.take_needs_resync());
    }

    #[tokio::test]
    async fn parse_notification_ignores_invalid_row_id_but_still_returns_change() {
        // Bad UUID in the row_id slot doesn't void the whole notification —
        // we still know what table changed, just not which row. Drop the row_id
        // and keep the change so invalidation still fires.
        let listener = make_listener();
        let payload = "v1:projects:INSERT:not-a-uuid";
        let (change, seq) = listener.parse_notification(payload).unwrap();
        assert_eq!(change.table, "projects");
        assert_eq!(change.operation, ChangeOperation::Insert);
        assert!(change.row_id.is_none());
        assert_eq!(seq, 0);
    }

    #[tokio::test]
    async fn parse_notification_rejects_unknown_operation() {
        let listener = make_listener();
        let payload = "v1:projects:NUKE:550e8400-e29b-41d4-a716-446655440000";
        assert!(listener.parse_notification(payload).is_none());
    }

    #[tokio::test]
    async fn parse_statement_level_notification() {
        let listener = make_listener();
        let payload = "v1s:orders:INSERT";
        let (change, seq) = listener.parse_notification(payload).unwrap();
        assert_eq!(change.table, "orders");
        assert_eq!(change.operation, ChangeOperation::Insert);
        assert!(change.row_id.is_none());
        assert!(change.changed_columns.is_empty());
        assert_eq!(seq, 0);
    }

    #[tokio::test]
    async fn parse_statement_level_update() {
        let listener = make_listener();
        let payload = "v1s:users:UPDATE";
        let (change, _) = listener.parse_notification(payload).unwrap();
        assert_eq!(change.table, "users");
        assert_eq!(change.operation, ChangeOperation::Update);
    }

    #[tokio::test]
    async fn parse_notification_handles_empty_columns_list() {
        // Trailing colon with no columns must not crash the parser. Empty
        // changed_columns is meaningful — column-filter check falls back to
        // "invalidate everything" downstream.
        let listener = make_listener();
        let payload = "v1:projects:UPDATE:550e8400-e29b-41d4-a716-446655440000:";
        let (change, _) = listener.parse_notification(payload).unwrap();
        // Empty after split → one empty-string element, not zero. This is the
        // contract the trigger and parser agree on.
        assert_eq!(change.changed_columns, vec![""]);
    }

    #[tokio::test]
    async fn subscribe_broadcasts_emitted_changes_to_all_receivers() {
        // emit_change feeds the same broadcast channel run() uses, so two
        // independent subscribers must both see the change. This is the
        // fan-out contract the listener relies on.
        let listener = make_listener();
        let mut rx1 = listener.subscribe();
        let mut rx2 = listener.subscribe();

        let change = Change::new("orders", ChangeOperation::Insert);
        listener.emit_change(change.clone());

        let got1 = rx1.try_recv().expect("rx1 receives");
        let got2 = rx2.try_recv().expect("rx2 receives");
        assert_eq!(got1.table, "orders");
        assert_eq!(got2.table, "orders");
    }

    #[tokio::test]
    async fn emit_change_without_subscribers_is_harmless() {
        // broadcast::send with no receivers returns Err — emit_change must
        // swallow it, not propagate. Otherwise reactor errors on startup
        // before any session attaches.
        let listener = make_listener();
        listener.emit_change(Change::new("anything", ChangeOperation::Delete));
        // No assertion required — the call returning is the test.
    }

    #[tokio::test]
    async fn stop_flips_running_flag_immediately() {
        // is_running() reflects in-flight loop state; stop() must mark it
        // false even when the loop never ran (no PG to talk to in tests).
        let listener = make_listener();
        // Running starts false (loop never entered in tests).
        assert!(!listener.is_running());
        // Pretend the loop is in flight.
        listener.running.store(true, Ordering::SeqCst);
        listener.stop();
        assert!(!listener.is_running());
    }
}

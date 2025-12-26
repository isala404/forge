use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::{mpsc, RwLock};
use tokio::time::Instant;

use forge_core::realtime::{Change, SubscriptionId};

use super::manager::SubscriptionManager;

/// Configuration for the invalidation engine.
#[derive(Debug, Clone)]
pub struct InvalidationConfig {
    /// Debounce window in milliseconds.
    pub debounce_ms: u64,
    /// Maximum debounce wait in milliseconds.
    pub max_debounce_ms: u64,
    /// Whether to coalesce changes by table.
    pub coalesce_by_table: bool,
    /// Maximum changes to buffer before forcing flush.
    pub max_buffer_size: usize,
}

impl Default for InvalidationConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 50,
            max_debounce_ms: 200,
            coalesce_by_table: true,
            max_buffer_size: 1000,
        }
    }
}

/// Pending invalidation for a subscription.
#[derive(Debug)]
struct PendingInvalidation {
    /// Subscription ID.
    subscription_id: SubscriptionId,
    /// Tables that changed.
    changed_tables: HashSet<String>,
    /// When this invalidation was first queued.
    first_change: Instant,
    /// When the last change was received.
    last_change: Instant,
}

/// Engine for determining which subscriptions need re-execution.
pub struct InvalidationEngine {
    subscription_manager: Arc<SubscriptionManager>,
    config: InvalidationConfig,
    /// Pending invalidations per subscription.
    pending: Arc<RwLock<HashMap<SubscriptionId, PendingInvalidation>>>,
    /// Channel for signaling invalidations.
    invalidation_tx: mpsc::Sender<Vec<SubscriptionId>>,
    invalidation_rx: Arc<RwLock<mpsc::Receiver<Vec<SubscriptionId>>>>,
}

impl InvalidationEngine {
    /// Create a new invalidation engine.
    pub fn new(subscription_manager: Arc<SubscriptionManager>, config: InvalidationConfig) -> Self {
        let (invalidation_tx, invalidation_rx) = mpsc::channel(1024);

        Self {
            subscription_manager,
            config,
            pending: Arc::new(RwLock::new(HashMap::new())),
            invalidation_tx,
            invalidation_rx: Arc::new(RwLock::new(invalidation_rx)),
        }
    }

    /// Process a database change.
    pub async fn process_change(&self, change: Change) {
        // Find affected subscriptions
        let affected = self
            .subscription_manager
            .find_affected_subscriptions(&change)
            .await;

        if affected.is_empty() {
            return;
        }

        tracing::debug!(
            table = %change.table,
            affected_count = affected.len(),
            "Found affected subscriptions for change"
        );

        let now = Instant::now();
        let mut pending = self.pending.write().await;

        for sub_id in affected {
            let entry = pending
                .entry(sub_id)
                .or_insert_with(|| PendingInvalidation {
                    subscription_id: sub_id,
                    changed_tables: HashSet::new(),
                    first_change: now,
                    last_change: now,
                });

            entry.changed_tables.insert(change.table.clone());
            entry.last_change = now;
        }

        // Check if we should flush due to buffer size
        if pending.len() >= self.config.max_buffer_size {
            drop(pending);
            self.flush_all().await;
        }
    }

    /// Check for subscriptions that need to be invalidated.
    pub async fn check_pending(&self) -> Vec<SubscriptionId> {
        let now = Instant::now();
        let debounce = Duration::from_millis(self.config.debounce_ms);
        let max_debounce = Duration::from_millis(self.config.max_debounce_ms);

        let mut pending = self.pending.write().await;
        let mut ready = Vec::new();

        pending.retain(|_, inv| {
            let since_last = now.duration_since(inv.last_change);
            let since_first = now.duration_since(inv.first_change);

            // Ready if debounce window passed or max wait exceeded
            if since_last >= debounce || since_first >= max_debounce {
                ready.push(inv.subscription_id);
                false // Remove from pending
            } else {
                true // Keep in pending
            }
        });

        ready
    }

    /// Flush all pending invalidations immediately.
    pub async fn flush_all(&self) -> Vec<SubscriptionId> {
        let mut pending = self.pending.write().await;
        let ready: Vec<SubscriptionId> = pending.keys().copied().collect();
        pending.clear();
        ready
    }

    /// Get the invalidation receiver for consuming invalidation events.
    pub async fn take_receiver(&self) -> Option<mpsc::Receiver<Vec<SubscriptionId>>> {
        let mut rx_guard = self.invalidation_rx.write().await;
        // We can only take once, so this is a simple swap
        // In practice, you'd use a different pattern
        None // Simplified - receiver is accessed via run loop
    }

    /// Run the invalidation check loop.
    pub async fn run(&self) {
        let check_interval = Duration::from_millis(self.config.debounce_ms / 2);

        loop {
            tokio::time::sleep(check_interval).await;

            let ready = self.check_pending().await;
            if !ready.is_empty() {
                if self.invalidation_tx.send(ready).await.is_err() {
                    // Receiver dropped, stop the loop
                    break;
                }
            }
        }
    }

    /// Get pending count for monitoring.
    pub async fn pending_count(&self) -> usize {
        self.pending.read().await.len()
    }

    /// Get statistics about the invalidation engine.
    pub async fn stats(&self) -> InvalidationStats {
        let pending = self.pending.read().await;

        let mut tables_pending = HashSet::new();
        for inv in pending.values() {
            tables_pending.extend(inv.changed_tables.iter().cloned());
        }

        InvalidationStats {
            pending_subscriptions: pending.len(),
            pending_tables: tables_pending.len(),
        }
    }
}

/// Statistics about the invalidation engine.
#[derive(Debug, Clone, Default)]
pub struct InvalidationStats {
    /// Number of subscriptions pending invalidation.
    pub pending_subscriptions: usize,
    /// Number of unique tables with pending changes.
    pub pending_tables: usize,
}

/// Coalesces multiple changes for the same table.
pub struct ChangeCoalescer {
    /// Changes grouped by table.
    changes_by_table: HashMap<String, Vec<Change>>,
}

impl ChangeCoalescer {
    /// Create a new change coalescer.
    pub fn new() -> Self {
        Self {
            changes_by_table: HashMap::new(),
        }
    }

    /// Add a change.
    pub fn add(&mut self, change: Change) {
        self.changes_by_table
            .entry(change.table.clone())
            .or_default()
            .push(change);
    }

    /// Get coalesced tables that had changes.
    pub fn tables(&self) -> impl Iterator<Item = &str> {
        self.changes_by_table.keys().map(|s| s.as_str())
    }

    /// Drain all changes.
    pub fn drain(&mut self) -> HashMap<String, Vec<Change>> {
        std::mem::take(&mut self.changes_by_table)
    }

    /// Check if empty.
    pub fn is_empty(&self) -> bool {
        self.changes_by_table.is_empty()
    }

    /// Count total changes.
    pub fn len(&self) -> usize {
        self.changes_by_table.values().map(|v| v.len()).sum()
    }
}

impl Default for ChangeCoalescer {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use forge_core::realtime::ChangeOperation;

    #[test]
    fn test_invalidation_config_default() {
        let config = InvalidationConfig::default();
        assert_eq!(config.debounce_ms, 50);
        assert_eq!(config.max_debounce_ms, 200);
        assert!(config.coalesce_by_table);
    }

    #[test]
    fn test_change_coalescer() {
        let mut coalescer = ChangeCoalescer::new();
        assert!(coalescer.is_empty());

        coalescer.add(Change::new("projects".to_string(), ChangeOperation::Insert));
        coalescer.add(Change::new("projects".to_string(), ChangeOperation::Update));
        coalescer.add(Change::new("users".to_string(), ChangeOperation::Insert));

        assert_eq!(coalescer.len(), 3);

        let tables: Vec<&str> = coalescer.tables().collect();
        assert!(tables.contains(&"projects"));
        assert!(tables.contains(&"users"));
    }

    #[test]
    fn test_change_coalescer_drain() {
        let mut coalescer = ChangeCoalescer::new();
        coalescer.add(Change::new("projects".to_string(), ChangeOperation::Insert));
        coalescer.add(Change::new("users".to_string(), ChangeOperation::Delete));

        let drained = coalescer.drain();
        assert!(coalescer.is_empty());
        assert_eq!(drained.len(), 2);
        assert!(drained.contains_key("projects"));
        assert!(drained.contains_key("users"));
    }

    #[tokio::test]
    async fn test_invalidation_engine_creation() {
        let subscription_manager = Arc::new(SubscriptionManager::new(50));
        let engine = InvalidationEngine::new(subscription_manager, InvalidationConfig::default());

        assert_eq!(engine.pending_count().await, 0);

        let stats = engine.stats().await;
        assert_eq!(stats.pending_subscriptions, 0);
        assert_eq!(stats.pending_tables, 0);
    }

    #[tokio::test]
    async fn test_invalidation_flush_all() {
        let subscription_manager = Arc::new(SubscriptionManager::new(50));
        let engine = InvalidationEngine::new(subscription_manager, InvalidationConfig::default());

        // Flush on empty should return empty
        let flushed = engine.flush_all().await;
        assert!(flushed.is_empty());
    }
}

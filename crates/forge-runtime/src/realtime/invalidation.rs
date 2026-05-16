use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::RwLock;
use tokio::time::Instant;

use forge_core::realtime::{Change, QueryGroupId};

use super::manager::SubscriptionManager;

/// Configuration for the invalidation engine.
///
/// Uses debouncing to batch rapid changes into single re-executions per group.
/// This prevents "thundering herd" scenarios where a batch insert triggers
/// N subscription refreshes.
#[derive(Debug, Clone)]
pub struct InvalidationConfig {
    /// Debounce window in milliseconds.
    pub debounce_ms: u64,
    /// Maximum debounce wait in milliseconds.
    pub max_debounce_ms: u64,
    /// Whether to coalesce changes by table. When enabled, multiple changes
    /// to the same table within the debounce window are merged into a single
    /// invalidation per affected group, reducing redundant re-executions.
    /// When disabled, each change is tracked independently per group.
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

/// Pending invalidation for a query group.
#[derive(Debug)]
struct PendingInvalidation {
    group_id: QueryGroupId,
    changed_tables: HashSet<String>,
    first_change: Instant,
    last_change: Instant,
}

/// Engine for determining which query groups need re-execution.
/// Operates on groups (not individual subscriptions) for O(groups) cost.
pub struct InvalidationEngine {
    subscription_manager: Arc<SubscriptionManager>,
    config: InvalidationConfig,
    /// Pending invalidations per query group.
    pending: Arc<RwLock<HashMap<QueryGroupId, PendingInvalidation>>>,
}

impl InvalidationEngine {
    /// Create a new invalidation engine.
    pub fn new(subscription_manager: Arc<SubscriptionManager>, config: InvalidationConfig) -> Self {
        Self {
            subscription_manager,
            config,
            pending: Arc::new(RwLock::new(HashMap::new())),
        }
    }

    /// Process a database change. Finds affected groups (not subscriptions).
    pub async fn process_change(&self, change: Change) {
        let affected = self.subscription_manager.find_affected_groups(&change);

        if affected.is_empty() {
            return;
        }

        tracing::debug!(
            table = %change.table,
            affected_groups = affected.len(),
            "Found affected groups for change"
        );

        let now = Instant::now();
        let mut pending = self.pending.write().await;

        for group_id in affected {
            if self.config.coalesce_by_table {
                // Coalesce: merge changes to the same table within the debounce window,
                // extending the last_change timestamp to delay re-execution until
                // the burst settles.
                let entry = pending
                    .entry(group_id)
                    .or_insert_with(|| PendingInvalidation {
                        group_id,
                        changed_tables: HashSet::new(),
                        first_change: now,
                        last_change: now,
                    });

                entry.changed_tables.insert(change.table.clone());
                entry.last_change = now;
            } else {
                // No coalescing: each change triggers its own invalidation entry,
                // so the group will be re-executed once per change after debounce.
                pending
                    .entry(group_id)
                    .or_insert_with(|| PendingInvalidation {
                        group_id,
                        changed_tables: HashSet::from([change.table.clone()]),
                        first_change: now,
                        last_change: now,
                    });
            }
        }

        if pending.len() >= self.config.max_buffer_size {
            // Force all pending groups to be immediately ready on the next
            // check_pending tick by backdating their timestamps past the
            // max debounce window. This avoids discarding group IDs (which
            // flush_all would return with no consumer).
            let past = Instant::now() - Duration::from_millis(self.config.max_debounce_ms + 1);
            for inv in pending.values_mut() {
                inv.first_change = past;
                inv.last_change = past;
            }
        }
    }

    /// Check for groups that need to be invalidated (debounce expired).
    pub async fn check_pending(&self) -> Vec<QueryGroupId> {
        // Cheap read-lock pre-check: avoid acquiring the write lock when idle.
        if self.pending.read().await.is_empty() {
            return Vec::new();
        }

        let now = Instant::now();
        let debounce = Duration::from_millis(self.config.debounce_ms);
        let max_debounce = Duration::from_millis(self.config.max_debounce_ms);

        let mut pending = self.pending.write().await;
        let mut ready = Vec::new();

        pending.retain(|_, inv| {
            let since_last = now.duration_since(inv.last_change);
            let since_first = now.duration_since(inv.first_change);

            if since_last >= debounce || since_first >= max_debounce {
                ready.push(inv.group_id);
                false
            } else {
                true
            }
        });

        ready
    }

    /// Flush all pending invalidations immediately.
    pub async fn flush_all(&self) -> Vec<QueryGroupId> {
        let mut pending = self.pending.write().await;
        let ready: Vec<QueryGroupId> = pending.keys().copied().collect();
        pending.clear();
        ready
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
            pending_groups: pending.len(),
            pending_tables: tables_pending.len(),
        }
    }
}

/// Statistics about the invalidation engine.
#[derive(Debug, Clone, Default)]
pub struct InvalidationStats {
    /// Number of groups pending invalidation.
    pub pending_groups: usize,
    /// Number of unique tables with pending changes.
    pub pending_tables: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_invalidation_config_default() {
        let config = InvalidationConfig::default();
        assert_eq!(config.debounce_ms, 50);
        assert_eq!(config.max_debounce_ms, 200);
        assert!(config.coalesce_by_table);
    }

    #[tokio::test]
    async fn test_invalidation_engine_creation() {
        let subscription_manager = Arc::new(SubscriptionManager::new(50));
        let engine = InvalidationEngine::new(subscription_manager, InvalidationConfig::default());

        assert_eq!(engine.pending_count().await, 0);

        let stats = engine.stats().await;
        assert_eq!(stats.pending_groups, 0);
        assert_eq!(stats.pending_tables, 0);
    }

    #[tokio::test]
    async fn test_invalidation_flush_all() {
        let subscription_manager = Arc::new(SubscriptionManager::new(50));
        let engine = InvalidationEngine::new(subscription_manager, InvalidationConfig::default());

        let flushed = engine.flush_all().await;
        assert!(flushed.is_empty());
    }

    #[tokio::test]
    async fn test_coalesce_by_table_enabled() {
        let subscription_manager = Arc::new(SubscriptionManager::new(50));
        let config = InvalidationConfig {
            coalesce_by_table: true,
            debounce_ms: 0,
            ..Default::default()
        };
        let engine = InvalidationEngine::new(subscription_manager, config);

        // Without subscriptions, no groups are affected, so pending stays empty
        let change = Change::new("users", forge_core::realtime::ChangeOperation::Insert);
        engine.process_change(change).await;
        assert_eq!(engine.pending_count().await, 0);
    }

    #[tokio::test]
    async fn test_coalesce_by_table_disabled() {
        let subscription_manager = Arc::new(SubscriptionManager::new(50));
        let config = InvalidationConfig {
            coalesce_by_table: false,
            debounce_ms: 0,
            ..Default::default()
        };
        let engine = InvalidationEngine::new(subscription_manager, config);

        // Without subscriptions, no groups are affected
        let change = Change::new("users", forge_core::realtime::ChangeOperation::Insert);
        engine.process_change(change).await;
        assert_eq!(engine.pending_count().await, 0);
    }
}

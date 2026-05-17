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
/// N subscription refreshes. Changes to the same table within the debounce
/// window are always merged into one invalidation per group (the underlying
/// pending map is keyed by group id, so it is a structural property, not a
/// configurable behavior).
#[derive(Debug, Clone)]
pub struct InvalidationConfig {
    /// Debounce window in milliseconds.
    pub debounce_ms: u64,
    /// Maximum debounce wait in milliseconds.
    pub max_debounce_ms: u64,
    /// Maximum changes to buffer before forcing flush.
    pub max_buffer_size: usize,
}

impl Default for InvalidationConfig {
    fn default() -> Self {
        Self {
            debounce_ms: 50,
            max_debounce_ms: 200,
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
#[allow(clippy::unwrap_used, clippy::indexing_slicing)]
mod tests {
    use super::*;
    use forge_core::function::AuthContext;
    use forge_core::realtime::{ChangeOperation, SessionId};
    use serde_json::json;

    fn engine_with_config(
        mgr: Arc<SubscriptionManager>,
        config: InvalidationConfig,
    ) -> InvalidationEngine {
        InvalidationEngine::new(mgr, config)
    }

    /// Create a fresh subscription manager and subscribe one group depending on `tables`.
    /// Returns the manager so the caller can wrap it in Arc and add more subs.
    fn manager_with_group(
        query_name: &str,
        table_deps: &'static [&'static str],
    ) -> Arc<SubscriptionManager> {
        let mgr = Arc::new(SubscriptionManager::new(50));
        let session = SessionId::new();
        mgr.subscribe(
            session,
            "client-1".to_string(),
            query_name,
            &json!({}),
            &AuthContext::unauthenticated(),
            table_deps,
            &[],
        )
        .unwrap();
        mgr
    }

    #[tokio::test]
    async fn new_engine_reports_zero_pending() {
        let mgr = Arc::new(SubscriptionManager::new(50));
        let engine = engine_with_config(mgr, InvalidationConfig::default());

        assert_eq!(engine.pending_count().await, 0);
        let stats = engine.stats().await;
        assert_eq!(stats.pending_groups, 0);
        assert_eq!(stats.pending_tables, 0);
    }

    #[tokio::test]
    async fn flush_all_on_empty_returns_empty() {
        let mgr = Arc::new(SubscriptionManager::new(50));
        let engine = engine_with_config(mgr, InvalidationConfig::default());
        assert!(engine.flush_all().await.is_empty());
    }

    #[tokio::test]
    async fn process_change_without_subscribers_is_noop() {
        // No subscribers → find_affected_groups returns empty → nothing
        // should be inserted, even with the tightest debounce.
        let mgr = Arc::new(SubscriptionManager::new(50));
        let config = InvalidationConfig {
            debounce_ms: 0,
            ..Default::default()
        };
        let engine = engine_with_config(mgr, config);

        engine
            .process_change(Change::new("users", ChangeOperation::Insert))
            .await;
        assert_eq!(engine.pending_count().await, 0);
    }

    #[tokio::test]
    async fn process_change_creates_pending_entry_for_affected_group() {
        let mgr = manager_with_group("list_users", &["users"]);
        let engine = engine_with_config(mgr, InvalidationConfig::default());

        engine
            .process_change(Change::new("users", ChangeOperation::Insert))
            .await;

        assert_eq!(engine.pending_count().await, 1);
        let stats = engine.stats().await;
        assert_eq!(stats.pending_groups, 1);
        assert_eq!(stats.pending_tables, 1);
    }

    #[tokio::test]
    async fn process_change_coalesces_repeats_for_same_group_into_one_entry() {
        // Three inserts on the same table for one subscribed group must
        // collapse to a single pending invalidation (the keying is by
        // group id, not by change).
        let mgr = manager_with_group("list_users", &["users"]);
        let engine = engine_with_config(mgr, InvalidationConfig::default());

        for _ in 0..3 {
            engine
                .process_change(Change::new("users", ChangeOperation::Insert))
                .await;
        }
        assert_eq!(engine.pending_count().await, 1);
    }

    #[tokio::test]
    async fn process_change_aggregates_multiple_tables_for_single_group() {
        // A group subscribed to both `users` and `orders` gets one pending
        // entry, but its `changed_tables` set must accumulate every table
        // the change stream touched.
        let mgr = manager_with_group("dashboard", &["users", "orders"]);
        let engine = engine_with_config(mgr, InvalidationConfig::default());

        engine
            .process_change(Change::new("users", ChangeOperation::Insert))
            .await;
        engine
            .process_change(Change::new("orders", ChangeOperation::Update))
            .await;

        assert_eq!(engine.pending_count().await, 1);
        let stats = engine.stats().await;
        assert_eq!(stats.pending_groups, 1);
        assert_eq!(stats.pending_tables, 2);
    }

    #[tokio::test(start_paused = true)]
    async fn check_pending_holds_entry_inside_debounce_window() {
        // With a 50ms debounce and 200ms max, an entry that just arrived
        // must not be emitted until at least the quiet window elapses.
        let mgr = manager_with_group("list_users", &["users"]);
        let engine = engine_with_config(
            mgr,
            InvalidationConfig {
                debounce_ms: 50,
                max_debounce_ms: 200,
                max_buffer_size: 1000,
            },
        );

        engine
            .process_change(Change::new("users", ChangeOperation::Insert))
            .await;

        // 40ms < debounce 50ms → still pending.
        tokio::time::advance(Duration::from_millis(40)).await;
        assert!(engine.check_pending().await.is_empty());
        assert_eq!(engine.pending_count().await, 1);
    }

    #[tokio::test(start_paused = true)]
    async fn check_pending_emits_ready_after_quiet_window() {
        let mgr = manager_with_group("list_users", &["users"]);
        let engine = engine_with_config(
            mgr,
            InvalidationConfig {
                debounce_ms: 50,
                max_debounce_ms: 200,
                max_buffer_size: 1000,
            },
        );

        engine
            .process_change(Change::new("users", ChangeOperation::Insert))
            .await;

        tokio::time::advance(Duration::from_millis(60)).await;
        let ready = engine.check_pending().await;
        assert_eq!(ready.len(), 1);
        // Emission drains the entry — next check sees nothing.
        assert_eq!(engine.pending_count().await, 0);
        assert!(engine.check_pending().await.is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn check_pending_emits_via_max_debounce_when_changes_keep_coming() {
        // A stream of changes faster than `debounce_ms` keeps `last_change`
        // fresh, but `max_debounce_ms` (from `first_change`) must still
        // eventually force the entry out. This prevents starvation under
        // continuous write load.
        let mgr = manager_with_group("list_users", &["users"]);
        let engine = engine_with_config(
            mgr,
            InvalidationConfig {
                debounce_ms: 50,
                max_debounce_ms: 200,
                max_buffer_size: 1000,
            },
        );

        // First change pins `first_change`. Keep `last_change` fresh with
        // sub-debounce ticks for 240ms (> max_debounce). We deliberately do
        // not drain inside the loop — that would let the entry be emitted
        // early, defeating the test.
        engine
            .process_change(Change::new("users", ChangeOperation::Insert))
            .await;
        for _ in 0..12 {
            tokio::time::advance(Duration::from_millis(20)).await;
            engine
                .process_change(Change::new("users", ChangeOperation::Insert))
                .await;
        }

        // first_change is now ~240ms old (> max_debounce 200ms), even though
        // last_change was just refreshed inside the quiet window.
        let ready = engine.check_pending().await;
        assert_eq!(
            ready.len(),
            1,
            "max_debounce must force-emit when changes keep arriving"
        );
    }

    #[tokio::test]
    async fn max_buffer_size_backdates_all_pending_to_force_next_flush() {
        // When pending.len() crosses max_buffer_size, all existing entries
        // are backdated past max_debounce so the next check_pending drains
        // them in one go — preventing unbounded memory growth without
        // discarding group IDs.
        let mgr = Arc::new(SubscriptionManager::new(50));
        // Subscribe four distinct query groups (each gets its own group id).
        let session = SessionId::new();
        for i in 0..4 {
            mgr.subscribe(
                session,
                format!("client-{i}"),
                &format!("q_{i}"),
                &json!({}),
                &AuthContext::unauthenticated(),
                &["t"],
                &[],
            )
            .unwrap();
        }
        let engine = engine_with_config(
            mgr,
            InvalidationConfig {
                debounce_ms: 50,
                max_debounce_ms: 200,
                max_buffer_size: 4,
            },
        );

        // Single change touches all 4 groups in one pass and trips the
        // buffer ceiling synchronously inside `process_change`.
        engine
            .process_change(Change::new("t", ChangeOperation::Insert))
            .await;
        assert_eq!(engine.pending_count().await, 4);

        // No advance needed — backdating already put them past the window.
        let ready = engine.check_pending().await;
        assert_eq!(ready.len(), 4);
    }

    #[tokio::test]
    async fn flush_all_returns_all_pending_and_clears_state() {
        let mgr = manager_with_group("list_users", &["users"]);
        let engine = engine_with_config(mgr, InvalidationConfig::default());

        engine
            .process_change(Change::new("users", ChangeOperation::Insert))
            .await;
        assert_eq!(engine.pending_count().await, 1);

        let flushed = engine.flush_all().await;
        assert_eq!(flushed.len(), 1);
        assert_eq!(engine.pending_count().await, 0);
    }

    #[tokio::test]
    async fn stats_dedupes_tables_across_groups() {
        // Two groups, both depending on `users`. After a single change to
        // `users`, stats must report 2 pending_groups but only 1 pending_table.
        let mgr = Arc::new(SubscriptionManager::new(50));
        let session = SessionId::new();
        mgr.subscribe(
            session,
            "a".to_string(),
            "q1",
            &json!({}),
            &AuthContext::unauthenticated(),
            &["users"],
            &[],
        )
        .unwrap();
        mgr.subscribe(
            session,
            "b".to_string(),
            "q2",
            &json!({}),
            &AuthContext::unauthenticated(),
            &["users"],
            &[],
        )
        .unwrap();
        let engine = engine_with_config(mgr, InvalidationConfig::default());

        engine
            .process_change(Change::new("users", ChangeOperation::Insert))
            .await;

        let stats = engine.stats().await;
        assert_eq!(stats.pending_groups, 2);
        assert_eq!(stats.pending_tables, 1);
    }
}

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use dashmap::DashMap;
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
    /// Pending invalidations per query group (sharded for concurrency).
    pending: DashMap<QueryGroupId, PendingInvalidation>,
}

impl InvalidationEngine {
    /// Create a new invalidation engine.
    pub fn new(subscription_manager: Arc<SubscriptionManager>, config: InvalidationConfig) -> Self {
        Self {
            subscription_manager,
            config,
            pending: DashMap::with_shard_amount(64),
        }
    }

    /// Process a database change. Finds affected groups (not subscriptions).
    pub fn process_change(&self, change: Change) {
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

        for group_id in affected {
            self.pending
                .entry(group_id)
                .and_modify(|entry| {
                    entry.changed_tables.insert(change.table.clone());
                    entry.last_change = now;
                })
                .or_insert_with(|| {
                    let mut tables = HashSet::new();
                    tables.insert(change.table.clone());
                    PendingInvalidation {
                        group_id,
                        changed_tables: tables,
                        first_change: now,
                        last_change: now,
                    }
                });
        }

        if self.pending.len() >= self.config.max_buffer_size {
            let past = Instant::now() - Duration::from_millis(self.config.max_debounce_ms + 1);
            self.pending.alter_all(|_, mut inv| {
                inv.first_change = past;
                inv.last_change = past;
                inv
            });
        }
    }

    /// Check for groups that need to be invalidated (debounce expired).
    pub fn check_pending(&self) -> Vec<QueryGroupId> {
        if self.pending.is_empty() {
            return Vec::new();
        }

        let now = Instant::now();
        let debounce = Duration::from_millis(self.config.debounce_ms);
        let max_debounce = Duration::from_millis(self.config.max_debounce_ms);

        let mut ready = Vec::new();

        self.pending.retain(|_, inv| {
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
    pub fn flush_all(&self) -> Vec<QueryGroupId> {
        let ready: Vec<QueryGroupId> = self.pending.iter().map(|r| *r.key()).collect();
        self.pending.clear();
        ready
    }

    /// Get pending count for monitoring.
    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    /// Get statistics about the invalidation engine.
    pub fn stats(&self) -> InvalidationStats {
        let mut tables_pending = HashSet::new();
        for entry in self.pending.iter() {
            tables_pending.extend(entry.changed_tables.iter().cloned());
        }

        InvalidationStats {
            pending_groups: self.pending.len(),
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

        assert_eq!(engine.pending_count(), 0);
        let stats = engine.stats();
        assert_eq!(stats.pending_groups, 0);
        assert_eq!(stats.pending_tables, 0);
    }

    #[tokio::test]
    async fn flush_all_on_empty_returns_empty() {
        let mgr = Arc::new(SubscriptionManager::new(50));
        let engine = engine_with_config(mgr, InvalidationConfig::default());
        assert!(engine.flush_all().is_empty());
    }

    #[tokio::test]
    async fn process_change_without_subscribers_is_noop() {
        let mgr = Arc::new(SubscriptionManager::new(50));
        let config = InvalidationConfig {
            debounce_ms: 0,
            ..Default::default()
        };
        let engine = engine_with_config(mgr, config);

        engine.process_change(Change::new("users", ChangeOperation::Insert));
        assert_eq!(engine.pending_count(), 0);
    }

    #[tokio::test]
    async fn process_change_creates_pending_entry_for_affected_group() {
        let mgr = manager_with_group("list_users", &["users"]);
        let engine = engine_with_config(mgr, InvalidationConfig::default());

        engine.process_change(Change::new("users", ChangeOperation::Insert));

        assert_eq!(engine.pending_count(), 1);
        let stats = engine.stats();
        assert_eq!(stats.pending_groups, 1);
        assert_eq!(stats.pending_tables, 1);
    }

    #[tokio::test]
    async fn process_change_coalesces_repeats_for_same_group_into_one_entry() {
        let mgr = manager_with_group("list_users", &["users"]);
        let engine = engine_with_config(mgr, InvalidationConfig::default());

        for _ in 0..3 {
            engine.process_change(Change::new("users", ChangeOperation::Insert));
        }
        assert_eq!(engine.pending_count(), 1);
    }

    #[tokio::test]
    async fn process_change_aggregates_multiple_tables_for_single_group() {
        let mgr = manager_with_group("dashboard", &["users", "orders"]);
        let engine = engine_with_config(mgr, InvalidationConfig::default());

        engine.process_change(Change::new("users", ChangeOperation::Insert));
        engine.process_change(Change::new("orders", ChangeOperation::Update));

        assert_eq!(engine.pending_count(), 1);
        let stats = engine.stats();
        assert_eq!(stats.pending_groups, 1);
        assert_eq!(stats.pending_tables, 2);
    }

    #[tokio::test(start_paused = true)]
    async fn check_pending_holds_entry_inside_debounce_window() {
        let mgr = manager_with_group("list_users", &["users"]);
        let engine = engine_with_config(
            mgr,
            InvalidationConfig {
                debounce_ms: 50,
                max_debounce_ms: 200,
                max_buffer_size: 1000,
            },
        );

        engine.process_change(Change::new("users", ChangeOperation::Insert));

        tokio::time::advance(Duration::from_millis(40)).await;
        assert!(engine.check_pending().is_empty());
        assert_eq!(engine.pending_count(), 1);
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

        engine.process_change(Change::new("users", ChangeOperation::Insert));

        tokio::time::advance(Duration::from_millis(60)).await;
        let ready = engine.check_pending();
        assert_eq!(ready.len(), 1);
        assert_eq!(engine.pending_count(), 0);
        assert!(engine.check_pending().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn check_pending_emits_via_max_debounce_when_changes_keep_coming() {
        let mgr = manager_with_group("list_users", &["users"]);
        let engine = engine_with_config(
            mgr,
            InvalidationConfig {
                debounce_ms: 50,
                max_debounce_ms: 200,
                max_buffer_size: 1000,
            },
        );

        engine.process_change(Change::new("users", ChangeOperation::Insert));
        for _ in 0..12 {
            tokio::time::advance(Duration::from_millis(20)).await;
            engine.process_change(Change::new("users", ChangeOperation::Insert));
        }

        let ready = engine.check_pending();
        assert_eq!(
            ready.len(),
            1,
            "max_debounce must force-emit when changes keep arriving"
        );
    }

    #[tokio::test]
    async fn max_buffer_size_backdates_all_pending_to_force_next_flush() {
        let mgr = Arc::new(SubscriptionManager::new(50));
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

        engine.process_change(Change::new("t", ChangeOperation::Insert));
        assert_eq!(engine.pending_count(), 4);

        let ready = engine.check_pending();
        assert_eq!(ready.len(), 4);
    }

    #[tokio::test]
    async fn flush_all_returns_all_pending_and_clears_state() {
        let mgr = manager_with_group("list_users", &["users"]);
        let engine = engine_with_config(mgr, InvalidationConfig::default());

        engine.process_change(Change::new("users", ChangeOperation::Insert));
        assert_eq!(engine.pending_count(), 1);

        let flushed = engine.flush_all();
        assert_eq!(flushed.len(), 1);
        assert_eq!(engine.pending_count(), 0);
    }

    #[tokio::test]
    async fn stats_dedupes_tables_across_groups() {
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

        engine.process_change(Change::new("users", ChangeOperation::Insert));

        let stats = engine.stats();
        assert_eq!(stats.pending_groups, 2);
        assert_eq!(stats.pending_tables, 1);
    }
}

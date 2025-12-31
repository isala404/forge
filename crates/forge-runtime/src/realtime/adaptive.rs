use std::collections::{HashMap, HashSet};
use std::time::Duration;

use tokio::sync::RwLock;

use forge_core::realtime::TrackingMode;

/// Configuration for adaptive tracking.
#[derive(Debug, Clone)]
pub struct AdaptiveTrackingConfig {
    /// Threshold to switch from table to row tracking.
    pub row_threshold: usize,
    /// Threshold to switch from row to table tracking.
    pub table_threshold: usize,
    /// Maximum number of rows to track per table.
    pub max_tracked_rows: usize,
    /// How often to re-evaluate tracking mode.
    pub evaluation_interval: Duration,
}

impl Default for AdaptiveTrackingConfig {
    fn default() -> Self {
        Self {
            row_threshold: 100,
            table_threshold: 50,
            max_tracked_rows: 10_000,
            evaluation_interval: Duration::from_secs(60),
        }
    }
}

/// Adaptive tracker that switches between table and row-level tracking.
///
/// When few subscriptions exist for a table, track at row level.
/// When many subscriptions exist, switch to table level.
pub struct AdaptiveTracker {
    config: AdaptiveTrackingConfig,
    /// Current tracking mode per table.
    table_modes: RwLock<HashMap<String, TrackingMode>>,
    /// Rows being tracked per table.
    tracked_rows: RwLock<HashMap<String, HashSet<String>>>,
    /// Subscription count per table.
    subscription_counts: RwLock<HashMap<String, usize>>,
    /// Row subscription count per table.
    row_subscription_counts: RwLock<HashMap<String, usize>>,
}

impl AdaptiveTracker {
    /// Create a new adaptive tracker.
    pub fn new(config: AdaptiveTrackingConfig) -> Self {
        Self {
            config,
            table_modes: RwLock::new(HashMap::new()),
            tracked_rows: RwLock::new(HashMap::new()),
            subscription_counts: RwLock::new(HashMap::new()),
            row_subscription_counts: RwLock::new(HashMap::new()),
        }
    }

    /// Record a subscription for a table.
    pub async fn record_subscription(&self, table: &str, row_ids: Option<Vec<String>>) {
        // Update subscription counts
        {
            let mut counts = self.subscription_counts.write().await;
            *counts.entry(table.to_string()).or_insert(0) += 1;
        }

        // Track specific rows if provided
        if let Some(ids) = row_ids {
            let mut tracked = self.tracked_rows.write().await;
            let rows = tracked.entry(table.to_string()).or_default();
            let mut row_counts = self.row_subscription_counts.write().await;

            for id in ids {
                if rows.len() < self.config.max_tracked_rows {
                    rows.insert(id);
                    *row_counts.entry(table.to_string()).or_insert(0) += 1;
                }
            }
        }

        // Evaluate if mode should change
        self.evaluate_table(table).await;
    }

    /// Remove a subscription.
    pub async fn remove_subscription(&self, table: &str, row_ids: Option<Vec<String>>) {
        // Update subscription counts
        {
            let mut counts = self.subscription_counts.write().await;
            if let Some(count) = counts.get_mut(table) {
                *count = count.saturating_sub(1);
            }
        }

        // Remove tracked rows if provided
        if let Some(ids) = row_ids {
            let mut tracked = self.tracked_rows.write().await;
            if let Some(rows) = tracked.get_mut(table) {
                let mut row_counts = self.row_subscription_counts.write().await;
                for id in ids {
                    if rows.remove(&id) {
                        if let Some(count) = row_counts.get_mut(table) {
                            *count = count.saturating_sub(1);
                        }
                    }
                }
            }
        }

        // Evaluate if mode should change
        self.evaluate_table(table).await;
    }

    /// Evaluate and potentially switch tracking mode for a table.
    pub async fn evaluate_table(&self, table: &str) {
        let subscription_count = {
            let counts = self.subscription_counts.read().await;
            *counts.get(table).unwrap_or(&0)
        };

        let row_count = {
            let row_counts = self.row_subscription_counts.read().await;
            *row_counts.get(table).unwrap_or(&0)
        };

        let new_mode = if subscription_count == 0 {
            TrackingMode::None
        } else if row_count > self.config.row_threshold {
            TrackingMode::Table
        } else if row_count < self.config.table_threshold {
            TrackingMode::Row
        } else {
            // Stay in current mode (hysteresis)
            let modes = self.table_modes.read().await;
            modes.get(table).copied().unwrap_or(TrackingMode::Row)
        };

        let mut modes = self.table_modes.write().await;
        let old_mode = modes.get(table).copied();

        if old_mode != Some(new_mode) {
            modes.insert(table.to_string(), new_mode);
            tracing::debug!(
                table = %table,
                old_mode = ?old_mode,
                new_mode = ?new_mode,
                subscription_count = subscription_count,
                row_count = row_count,
                "Tracking mode changed"
            );
        }
    }

    /// Evaluate all tables.
    pub async fn evaluate(&self) {
        let tables: Vec<String> = {
            let counts = self.subscription_counts.read().await;
            counts.keys().cloned().collect()
        };

        for table in tables {
            self.evaluate_table(&table).await;
        }
    }

    /// Check if a change should be invalidated.
    pub async fn should_invalidate(&self, table: &str, row_id: &str) -> bool {
        let mode = {
            let modes = self.table_modes.read().await;
            modes.get(table).copied().unwrap_or(TrackingMode::None)
        };

        match mode {
            TrackingMode::None => false,
            TrackingMode::Table | TrackingMode::Adaptive => true,
            TrackingMode::Row => {
                let tracked = self.tracked_rows.read().await;
                tracked
                    .get(table)
                    .map(|rows| rows.contains(row_id))
                    .unwrap_or(false)
            }
        }
    }

    /// Get the current tracking mode for a table.
    pub async fn get_mode(&self, table: &str) -> TrackingMode {
        let modes = self.table_modes.read().await;
        modes.get(table).copied().unwrap_or(TrackingMode::None)
    }

    /// Get tracking statistics.
    pub async fn stats(&self) -> AdaptiveTrackingStats {
        let modes = self.table_modes.read().await;
        let tracked = self.tracked_rows.read().await;
        let counts = self.subscription_counts.read().await;

        let tables_by_mode =
            |mode: TrackingMode| -> usize { modes.values().filter(|&&m| m == mode).count() };

        let total_tracked_rows: usize = tracked.values().map(|rows| rows.len()).sum();

        AdaptiveTrackingStats {
            tables_none: tables_by_mode(TrackingMode::None),
            tables_row: tables_by_mode(TrackingMode::Row),
            tables_table: tables_by_mode(TrackingMode::Table),
            total_tracked_rows,
            total_subscriptions: counts.values().sum(),
        }
    }

    /// Clear all tracking state.
    pub async fn clear(&self) {
        self.table_modes.write().await.clear();
        self.tracked_rows.write().await.clear();
        self.subscription_counts.write().await.clear();
        self.row_subscription_counts.write().await.clear();
    }
}

/// Statistics about adaptive tracking.
#[derive(Debug, Clone, Default)]
pub struct AdaptiveTrackingStats {
    /// Tables with no tracking.
    pub tables_none: usize,
    /// Tables with row-level tracking.
    pub tables_row: usize,
    /// Tables with table-level tracking.
    pub tables_table: usize,
    /// Total rows being tracked.
    pub total_tracked_rows: usize,
    /// Total active subscriptions.
    pub total_subscriptions: usize,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_adaptive_tracker_creation() {
        let config = AdaptiveTrackingConfig::default();
        let tracker = AdaptiveTracker::new(config);

        let stats = tracker.stats().await;
        assert_eq!(stats.tables_none, 0);
        assert_eq!(stats.total_tracked_rows, 0);
    }

    #[tokio::test]
    async fn test_subscription_tracking() {
        let config = AdaptiveTrackingConfig {
            row_threshold: 5,
            table_threshold: 2,
            ..Default::default()
        };
        let tracker = AdaptiveTracker::new(config);

        // Add a row subscription
        tracker
            .record_subscription("users", Some(vec!["user-1".to_string()]))
            .await;

        let mode = tracker.get_mode("users").await;
        assert_eq!(mode, TrackingMode::Row);

        // Should invalidate for tracked row
        assert!(tracker.should_invalidate("users", "user-1").await);
        // Should not invalidate for untracked row
        assert!(!tracker.should_invalidate("users", "user-2").await);
    }

    #[tokio::test]
    async fn test_mode_switch_to_table() {
        let config = AdaptiveTrackingConfig {
            row_threshold: 3,
            table_threshold: 1,
            ..Default::default()
        };
        let tracker = AdaptiveTracker::new(config);

        // Add many row subscriptions
        for i in 0..5 {
            tracker
                .record_subscription("users", Some(vec![format!("user-{}", i)]))
                .await;
        }

        let mode = tracker.get_mode("users").await;
        assert_eq!(mode, TrackingMode::Table);

        // Should invalidate for any row in table mode
        assert!(tracker.should_invalidate("users", "user-999").await);
    }
}

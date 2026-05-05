//! Real-time subscription engine and SSE transport configuration.

use serde::{Deserialize, Serialize};

/// Configuration for the real-time subscription engine and SSE transport.
///
/// All fields have production-safe defaults; only set these to tune behaviour.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RealtimeConfig {
    /// Maximum concurrent query re-executions during an invalidation flush.
    #[serde(default = "default_max_concurrent_reexecutions")]
    pub max_concurrent_reexecutions: usize,

    /// Periodic resync interval. Re-evaluates every active query group to recover
    /// from dropped NOTIFY payloads. "0s" disables the sweep (e.g. "60s", "5m").
    #[serde(default = "default_resync_interval")]
    pub resync_interval: String,

    /// Broadcast channel buffer for raw change notifications from PG.
    #[serde(
        default = "default_postgres_change_buffer_size",
        alias = "listener_channel_buffer"
    )]
    pub postgres_change_buffer_size: usize,

    /// Debounce quiet window duration. Changes arriving within this window are
    /// coalesced into a single flush (e.g. "50ms", "100ms").
    #[serde(default = "default_debounce_quiet_window", alias = "debounce_quiet")]
    pub debounce_quiet_window: String,

    /// Absolute maximum debounce wait before forcing a flush (e.g. "200ms", "1s").
    #[serde(default = "default_debounce_max_wait", alias = "debounce_max")]
    pub debounce_max_wait: String,

    /// Maximum concurrent SSE sessions across all clients.
    #[serde(default = "default_sse_max_sessions")]
    pub sse_max_sessions: usize,

    /// Maximum subscriptions per SSE session.
    #[serde(default = "default_subscription_max_per_session")]
    pub subscription_max_per_session: usize,

    /// Row-count threshold for switching from row-level to table-level
    /// change tracking per table. Lowering reduces memory; raising reduces
    /// false-positive invalidations on write-heavy tables.
    #[serde(
        default = "default_change_tracking_row_threshold",
        alias = "adaptive_row_threshold"
    )]
    pub change_tracking_row_threshold: usize,

    /// Number of DashMap shards for the subscription manager. Higher values
    /// reduce lock contention at the cost of memory.
    #[serde(default = "default_shard_count")]
    pub shard_count: usize,

    /// Maximum concurrent SSE sessions per authenticated user.
    #[serde(default = "default_max_sessions_per_user")]
    pub max_sessions_per_user: usize,

    /// Maximum concurrent SSE sessions per source IP.
    #[serde(default = "default_max_sessions_per_ip")]
    pub max_sessions_per_ip: usize,

    /// Cap on a user's total subscriptions across every active session.
    #[serde(default = "default_max_subscriptions_per_user")]
    pub max_subscriptions_per_user: usize,

    /// Per-query cached-result memory ceiling (bytes). Cached results
    /// exceeding this size are dropped after re-execution.
    #[serde(default = "default_max_cached_result_bytes")]
    pub max_cached_result_bytes: usize,
}

fn default_max_concurrent_reexecutions() -> usize {
    64
}
fn default_resync_interval() -> String {
    "60s".to_string()
}
fn default_postgres_change_buffer_size() -> usize {
    1024
}
fn default_debounce_quiet_window() -> String {
    "50ms".to_string()
}
fn default_debounce_max_wait() -> String {
    "200ms".to_string()
}
fn default_sse_max_sessions() -> usize {
    10_000
}
fn default_subscription_max_per_session() -> usize {
    100
}
fn default_change_tracking_row_threshold() -> usize {
    200
}
fn default_shard_count() -> usize {
    64
}
fn default_max_sessions_per_user() -> usize {
    8
}
fn default_max_sessions_per_ip() -> usize {
    32
}
fn default_max_subscriptions_per_user() -> usize {
    500
}
fn default_max_cached_result_bytes() -> usize {
    1_048_576
}

impl Default for RealtimeConfig {
    fn default() -> Self {
        Self {
            max_concurrent_reexecutions: default_max_concurrent_reexecutions(),
            resync_interval: default_resync_interval(),
            postgres_change_buffer_size: default_postgres_change_buffer_size(),
            debounce_quiet_window: default_debounce_quiet_window(),
            debounce_max_wait: default_debounce_max_wait(),
            sse_max_sessions: default_sse_max_sessions(),
            subscription_max_per_session: default_subscription_max_per_session(),
            change_tracking_row_threshold: default_change_tracking_row_threshold(),
            shard_count: default_shard_count(),
            max_sessions_per_user: default_max_sessions_per_user(),
            max_sessions_per_ip: default_max_sessions_per_ip(),
            max_subscriptions_per_user: default_max_subscriptions_per_user(),
            max_cached_result_bytes: default_max_cached_result_bytes(),
        }
    }
}

impl RealtimeConfig {
    /// Resync interval in seconds, parsed from the `resync_interval` string.
    pub fn resync_interval_secs(&self) -> u64 {
        super::parse_duration_secs(&self.resync_interval, 60)
    }

    /// Debounce quiet window in milliseconds, parsed from the `debounce_quiet_window` string.
    pub fn debounce_quiet_ms(&self) -> u64 {
        super::parse_duration_millis(&self.debounce_quiet_window, 50)
    }

    /// Absolute maximum debounce wait in milliseconds, parsed from the `debounce_max_wait` string.
    pub fn debounce_max_ms(&self) -> u64 {
        super::parse_duration_millis(&self.debounce_max_wait, 200)
    }
}

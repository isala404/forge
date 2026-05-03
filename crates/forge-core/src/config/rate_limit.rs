//! Rate-limiter configuration.

use serde::{Deserialize, Serialize};

/// Rate-limiter mode selector.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RateLimitMode {
    /// Per-node DashMap fast path with PG fallback for `Global` keys.
    /// User/IP limits are approximate across N nodes — right for DDoS protection.
    #[default]
    Hybrid,
    /// Every check round-trips to PG. Cluster-wide correct — right for billing-grade quotas.
    Strict,
}

/// `[rate_limit]` configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RateLimitSettings {
    /// Rate-limiter mode. Defaults to `hybrid`.
    #[serde(default)]
    pub mode: RateLimitMode,

    /// Maximum local (in-memory) rate limit buckets before eviction.
    #[serde(default = "default_max_local_buckets")]
    pub max_local_buckets: usize,
}

impl Default for RateLimitSettings {
    fn default() -> Self {
        Self {
            mode: RateLimitMode::default(),
            max_local_buckets: default_max_local_buckets(),
        }
    }
}

fn default_max_local_buckets() -> usize {
    100_000
}

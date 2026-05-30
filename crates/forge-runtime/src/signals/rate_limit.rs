//! In-memory per-IP rate limiter for signal ingestion endpoints.
//!
//! Signal traffic is high-volume (every page view, every web vital, every
//! tracked event) and unauthenticated. Routing each request through the
//! Postgres-backed `StrictRateLimiter` would add a DB round-trip per signal,
//! which is unacceptable at scale. This limiter uses a fixed 60-second window
//! per IP, kept entirely in process memory. Across a cluster the per-node
//! limit is effectively `nodes * max_per_window`, which is fine for abuse
//! protection — billing-grade limits are not the goal here.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicI64, AtomicU32, Ordering};

use dashmap::DashMap;

/// Default ceiling on how many signal requests a single IP may submit per
/// minute. Generous enough to absorb legitimate bursts (page-view + web-vital
/// flush + a handful of tracked events on a navigation) while still capping
/// runaway clients.
///
/// TODO(signals-config): expose `SignalsConfig::rate_limit_per_minute` and
/// thread it through `gateway::server` so operators can tune this in
/// forge.toml. Until then, callers can override via `with_limit(...)`.
const DEFAULT_MAX_REQUESTS_PER_WINDOW: u32 = 600;

/// Window length in seconds.
const WINDOW_SECS: i64 = 60;

/// Soft cap on the number of distinct IPs the limiter tracks. When exceeded,
/// the oldest-window entries are dropped so the map cannot grow unbounded.
const MAX_TRACKED_IPS: usize = 100_000;

/// Fixed-window per-IP rate limiter.
pub struct SignalRateLimiter {
    max_per_window: u32,
    buckets: DashMap<String, IpBucket>,
    /// Insertion-order queue. When `buckets.len()` exceeds `MAX_TRACKED_IPS`
    /// we pop the oldest entry off the front and remove it from the map.
    /// O(1) amortized — replaces the previous O(n) `evict_oldest` sweep that
    /// ran inline on every new-IP miss.
    insertion_order: Mutex<VecDeque<String>>,
}

struct IpBucket {
    /// Unix timestamp (seconds) of the current window's start.
    window_start: AtomicI64,
    /// Number of requests counted in the current window.
    count: AtomicU32,
}

impl SignalRateLimiter {
    pub fn new() -> Self {
        Self::with_limit(DEFAULT_MAX_REQUESTS_PER_WINDOW)
    }

    pub fn with_limit(max_per_window: u32) -> Self {
        Self {
            max_per_window,
            buckets: DashMap::new(),
            insertion_order: Mutex::new(VecDeque::new()),
        }
    }

    /// Returns `true` if the request from `ip` is within the allowed quota,
    /// `false` if it should be rejected. `None` (unknown IP) is always
    /// allowed — better than a single shared bucket that legitimate
    /// proxy-collapsed clients would all share.
    pub fn check(&self, ip: Option<&str>) -> bool {
        let Some(ip) = ip else {
            return true;
        };

        let now = chrono::Utc::now().timestamp();

        if let Some(bucket) = self.buckets.get(ip) {
            let window_start = bucket.window_start.load(Ordering::Relaxed);
            if now - window_start >= WINDOW_SECS {
                bucket.window_start.store(now, Ordering::Relaxed);
                bucket.count.store(1, Ordering::Relaxed);
                return true;
            }
            // Note: fetch_add followed by the prev < max comparison is a
            // benign race — two concurrent callers can both observe a value
            // just under the ceiling and both succeed, leaving the bucket a
            // small constant over the configured max. Acceptable for abuse
            // protection; billing-grade limits are explicitly out of scope.
            let prev = bucket.count.fetch_add(1, Ordering::Relaxed);
            return prev < self.max_per_window;
        }

        self.insert_new_bucket(ip, now);
        true
    }

    /// Insert a freshly-seen IP. Evicts the oldest tracked IP in O(1) when the
    /// map is at capacity (FIFO; not strictly LRU but good enough — abuse-
    /// driven floods churn the queue fast enough that any stale entries fall
    /// off naturally).
    fn insert_new_bucket(&self, ip: &str, now: i64) {
        if self.buckets.len() >= MAX_TRACKED_IPS
            && let Ok(mut order) = self.insertion_order.lock()
            && let Some(victim) = order.pop_front()
        {
            self.buckets.remove(&victim);
        }
        self.buckets.insert(
            ip.to_string(),
            IpBucket {
                window_start: AtomicI64::new(now),
                count: AtomicU32::new(1),
            },
        );
        if let Ok(mut order) = self.insertion_order.lock() {
            order.push_back(ip.to_string());
        }
    }
}

impl Default for SignalRateLimiter {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn allows_unknown_ip() {
        let limiter = SignalRateLimiter::with_limit(2);
        assert!(limiter.check(None));
        assert!(limiter.check(None));
        assert!(limiter.check(None));
    }

    #[tokio::test]
    async fn enforces_per_ip_limit() {
        let limiter = SignalRateLimiter::with_limit(3);
        assert!(limiter.check(Some("1.2.3.4")));
        assert!(limiter.check(Some("1.2.3.4")));
        assert!(limiter.check(Some("1.2.3.4")));
        assert!(!limiter.check(Some("1.2.3.4")));
    }

    #[tokio::test]
    async fn isolates_different_ips() {
        let limiter = SignalRateLimiter::with_limit(1);
        assert!(limiter.check(Some("1.2.3.4")));
        assert!(!limiter.check(Some("1.2.3.4")));
        assert!(limiter.check(Some("5.6.7.8")));
    }
}

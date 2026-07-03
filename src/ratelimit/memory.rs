use super::algo::{
    SlidingState, check_bucket, check_key, check_limit, is_soft_error, resolve_fail_open,
    sliding_step, synthetic_allow, token_bucket_step,
};
use super::{Algo, Decision, FailMode, Limit, RateLimit};
use crate::backend::{BackendLifecycle, Primitive};
use crate::error::Result;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Entries untouched this long are dropped by `maintain`. An idle bucket has refilled
/// to full or its window has aged out by then, so dropping it is observably identical to
/// keeping it: a re-check starts from a fresh full bucket either way. Mirrors the
/// Postgres sweep window.
const IDLE_PURGE_SECS: u64 = 24 * 60 * 60;

/// One subject's mutable limiter state. The entry holds both algorithms' state (like the
/// Postgres row that carries every column), so switching algorithm on a key reads the
/// other algorithm's fresh state. Whichever `check` runs touches only its own fields.
struct Bucket {
    /// Token-bucket level. `None` until the first token-bucket check, where it reads as a
    /// fresh full bucket (mirrors the row's `NULL` tokens default).
    tokens: Option<f64>,
    /// Sliding-window state. `None` until the first sliding-window check.
    sliding: Option<SlidingState>,
    /// Last time this entry was touched. Drives the token-bucket refill and the
    /// idle-purge sweep.
    updated_at: Instant,
}

impl Bucket {
    fn fresh(now: Instant) -> Self {
        Self {
            tokens: None,
            sliding: None,
            updated_at: now,
        }
    }
}

pub(crate) struct MemRateLimit {
    state: Mutex<HashMap<(String, String), Bucket>>,
    /// Prefix joined to every bucket as `<namespace>:<bucket>`. Empty = no prefix.
    namespace: String,
    /// Instance default for what happens on a *soft* backend error. In-process bucket math
    /// is infallible, so this never fires today; it exists for parity with
    /// [`super::PgRateLimit`] and stays honored if a fallible path is ever added.
    fail_open: bool,
    /// Fixed origin for the sliding-window clock: `now_epoch` is seconds since this
    /// instant. The math needs only a monotonic seconds source; the absolute offset just
    /// fixes where `per`-aligned window boundaries fall.
    origin: Instant,
}

impl MemRateLimit {
    pub(crate) fn new(namespace: String, fail_open: bool) -> Self {
        Self {
            state: Mutex::new(HashMap::new()),
            namespace,
            fail_open,
            origin: Instant::now(),
        }
    }

    /// Take the map lock, recovering the guard if a previous holder panicked. Critical
    /// sections are short and synchronous (no `await` across the lock), so a poisoned lock
    /// never reflects a half-updated invariant worth aborting for.
    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<(String, String), Bucket>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Namespaced bucket value. The namespace is colon-free, so `<ns>:<bucket>` never
    /// collides across distinct `(ns, bucket)`.
    fn ns_bucket(&self, bucket: &str) -> String {
        crate::util::namespaced(&self.namespace, bucket)
    }

    /// Drop entries untouched for longer than the idle window. Idempotent; a dropped
    /// entry is observably a fresh one, so a later check resurrects it harmlessly.
    pub(crate) fn purge_idle(&self) {
        let now = Instant::now();
        let cutoff = Duration::from_secs(IDLE_PURGE_SECS);
        self.lock()
            .retain(|_, b| now.saturating_duration_since(b.updated_at) < cutoff);
    }

    /// Token-bucket check-and-consume against this subject's locked entry. Returns
    /// `Result` only to share the fail-open path with the Postgres backend; the in-memory
    /// math itself cannot fail.
    fn check_token_bucket(&self, ns_bucket: &str, subject: &str, limit: Limit) -> Result<Decision> {
        let now = Instant::now();
        let mut state = self.lock();
        let entry = state
            .entry((ns_bucket.to_string(), subject.to_string()))
            .or_insert_with(|| Bucket::fresh(now));
        // A just-inserted entry has `updated_at == now`, so `elapsed == 0` and a `None`
        // token level reads as a full bucket, like the freshly-inserted Postgres row.
        let elapsed = now
            .saturating_duration_since(entry.updated_at)
            .as_secs_f64();
        let (new_tokens, decision) = token_bucket_step(entry.tokens, elapsed, limit);
        entry.tokens = Some(new_tokens);
        entry.updated_at = now;
        Ok(decision)
    }

    /// Sliding-window check-and-consume against this subject's locked entry.
    fn check_sliding(&self, ns_bucket: &str, subject: &str, limit: Limit) -> Result<Decision> {
        let now = Instant::now();
        let now_epoch = now.saturating_duration_since(self.origin).as_secs_f64();
        let mut state = self.lock();
        let entry = state
            .entry((ns_bucket.to_string(), subject.to_string()))
            .or_insert_with(|| Bucket::fresh(now));
        let (new_state, decision) = sliding_step(entry.sliding.clone(), now_epoch, limit);
        entry.sliding = Some(new_state);
        entry.updated_at = now;
        Ok(decision)
    }
}

#[async_trait]
impl RateLimit for MemRateLimit {
    async fn check_with(
        &self,
        bucket: &str,
        key: &str,
        limit: Limit,
        fail: FailMode,
    ) -> Result<Decision> {
        let fail_open = resolve_fail_open(fail, self.fail_open);
        // Caller bugs (`Invalid`/`Limit`) always surface, regardless of fail mode.
        check_bucket(bucket)?;
        check_key(key)?;
        check_limit(&limit)?;
        let ns_bucket = self.ns_bucket(bucket);
        let result = match limit.algo {
            Algo::TokenBucket => self.check_token_bucket(&ns_bucket, key, limit),
            Algo::SlidingWindow => self.check_sliding(&ns_bucket, key, limit),
            // `Algo` is `#[non_exhaustive]`; default to token bucket.
            #[allow(unreachable_patterns)]
            _ => self.check_token_bucket(&ns_bucket, key, limit),
        };
        match result {
            Ok(d) => Ok(d),
            // The in-process math is infallible, so this branch never fires today; it
            // mirrors the Postgres backend so a future fallible path obeys the same
            // `ratelimit_fail_open` semantics.
            Err(e) if fail_open && is_soft_error(&e) => {
                tracing::warn!(error = %e, "ratelimit backend error; failing open (allowing)");
                Ok(synthetic_allow(limit))
            }
            Err(e) => Err(e),
        }
    }
}

#[async_trait]
impl BackendLifecycle for MemRateLimit {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn primitive(&self) -> Primitive {
        Primitive::RateLimit
    }
    fn durable(&self) -> bool {
        false
    }
    fn caveats(&self) -> &'static str {
        "per-process buckets, not shared across processes"
    }
    async fn maintain(&self) -> Result<()> {
        self.purge_idle();
        Ok(())
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::super::{MAX_BUCKET_BYTES, MAX_KEY_BYTES};
    use super::*;
    use crate::error::ForgeError;

    fn tb(max: u32, per_secs: u64) -> Limit {
        Limit::per_duration(max, Duration::from_secs(per_secs))
    }

    fn sw(max: u32, per_secs: u64) -> Limit {
        tb(max, per_secs).with_algo(Algo::SlidingWindow)
    }

    #[tokio::test]
    async fn token_bucket_consumes_budget_then_denies() {
        let rl = MemRateLimit::new(String::new(), true);
        // A large window keeps refill over the test's microseconds far below one token,
        // so the first three checks drain the budget deterministically.
        let limit = tb(3, 3600);
        for expected_remaining in [2, 1, 0] {
            let d = rl.check("api", "user1", limit).await.unwrap();
            assert!(d.allowed);
            assert_eq!(d.remaining, expected_remaining);
            assert_eq!(d.limit, 3);
            assert!(d.retry_after.is_none());
        }
        let denied = rl.check("api", "user1", limit).await.unwrap();
        assert!(!denied.allowed, "fourth call exhausts the bucket");
        assert_eq!(denied.remaining, 0);
        assert!(denied.retry_after.is_some(), "a denial carries retry_after");
    }

    #[tokio::test]
    async fn sliding_window_caps_then_denies_within_window() {
        let rl = MemRateLimit::new(String::new(), true);
        let limit = sw(2, 100);
        let d1 = rl.check("login", "ip", limit).await.unwrap();
        assert!(d1.allowed && d1.remaining == 1);
        let d2 = rl.check("login", "ip", limit).await.unwrap();
        assert!(d2.allowed && d2.remaining == 0);
        let d3 = rl.check("login", "ip", limit).await.unwrap();
        assert!(!d3.allowed, "third call in the window is denied");
        assert!(d3.retry_after.is_some());
    }

    #[tokio::test]
    async fn distinct_subjects_have_independent_budgets() {
        let rl = MemRateLimit::new(String::new(), true);
        let limit = tb(1, 3600);
        assert!(rl.check("api", "alice", limit).await.unwrap().allowed);
        assert!(
            !rl.check("api", "alice", limit).await.unwrap().allowed,
            "alice is now exhausted"
        );
        assert!(
            rl.check("api", "bob", limit).await.unwrap().allowed,
            "bob has his own bucket"
        );
    }

    #[tokio::test]
    async fn distinct_buckets_have_independent_budgets() {
        let rl = MemRateLimit::new(String::new(), true);
        let limit = tb(1, 3600);
        assert!(rl.check("send", "u", limit).await.unwrap().allowed);
        assert!(!rl.check("send", "u", limit).await.unwrap().allowed);
        assert!(
            rl.check("read", "u", limit).await.unwrap().allowed,
            "a different bucket for the same subject is independent"
        );
    }

    #[tokio::test]
    async fn namespaces_isolate_buckets() {
        let a = MemRateLimit::new("tenant_a".to_string(), true);
        let b = MemRateLimit::new("tenant_b".to_string(), true);
        let limit = tb(1, 3600);
        assert!(a.check("api", "shared", limit).await.unwrap().allowed);
        assert!(
            !a.check("api", "shared", limit).await.unwrap().allowed,
            "tenant_a is exhausted"
        );
        assert!(
            b.check("api", "shared", limit).await.unwrap().allowed,
            "tenant_b's namespaced bucket is untouched"
        );
    }

    #[tokio::test]
    async fn invalid_inputs_surface_as_errors_regardless_of_fail_open() {
        // fail_open = true must NOT swallow caller bugs.
        let rl = MemRateLimit::new(String::new(), true);
        let limit = tb(1, 60);
        assert!(matches!(
            rl.check("", "u", limit).await,
            Err(ForgeError::Invalid(_))
        ));
        assert!(matches!(
            rl.check("api", "", limit).await,
            Err(ForgeError::Invalid(_))
        ));
        assert!(matches!(
            rl.check("api", "u", tb(0, 60)).await,
            Err(ForgeError::Invalid(_))
        ));
        assert!(matches!(
            rl.check("api", "u", Limit::per_duration(1, Duration::ZERO))
                .await,
            Err(ForgeError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn oversized_bucket_and_key_are_limit_errors() {
        let rl = MemRateLimit::new(String::new(), true);
        let limit = tb(1, 60);
        let big_bucket = "b".repeat(MAX_BUCKET_BYTES + 1);
        let big_key = "k".repeat(MAX_KEY_BYTES + 1);
        assert!(matches!(
            rl.check(&big_bucket, "u", limit).await,
            Err(ForgeError::Limit(_))
        ));
        assert!(matches!(
            rl.check("api", &big_key, limit).await,
            Err(ForgeError::Limit(_))
        ));
    }

    #[tokio::test]
    async fn purge_idle_keeps_recent_entries() {
        let rl = MemRateLimit::new(String::new(), true);
        rl.check("api", "u", tb(1, 60)).await.unwrap();
        // Nothing is idle yet, so a sweep must not drop the live bucket: the next check
        // sees the consumed budget rather than a fresh one.
        rl.purge_idle();
        assert!(
            !rl.check("api", "u", tb(1, 60)).await.unwrap().allowed,
            "the recent bucket survived the sweep and is still exhausted"
        );
    }
}

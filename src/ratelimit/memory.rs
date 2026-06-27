//! In-process `ratelimit` backend. Contract: docs/contracts/ratelimit.md.
//!
//! Per-process limiter state behind a `Mutex<HashMap>`, keyed by the same namespaced
//! `(bucket, subject)` the Postgres backend keys its row on, running the same pure
//! algorithm step. Nothing survives a restart and buckets are not shared across
//! processes; the observable [`Decision`] contract matches [`super::PgRateLimit`].

use super::{Algo, Decision, FailMode, Limit, MAX_BUCKET_BYTES, MAX_KEY_BYTES, RateLimit};
use crate::backend::{BackendLifecycle, Primitive};
use crate::error::{ForgeError, Result};
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Upper bound on `Limit.per` (~100 years), matching the Postgres backend so a policy
/// that fits one fits the other. Over => `Limit`.
const MAX_PER_SECS: f64 = 100.0 * 365.0 * 24.0 * 60.0 * 60.0;
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
        let fail_open = match fail {
            FailMode::Default => self.fail_open,
            FailMode::Open => true,
            FailMode::Closed => false,
            // `FailMode` is `#[non_exhaustive]`; an unknown mode falls back to the
            // instance default.
            #[allow(unreachable_patterns)]
            _ => self.fail_open,
        };
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

/// Soft errors (transient/backend) are swallowed by fail-open; `Invalid`/`Limit` (caller
/// bugs) always surface regardless of failure mode.
fn is_soft_error(e: &ForgeError) -> bool {
    matches!(e, ForgeError::Unavailable(_) | ForgeError::Backend { .. })
}

fn synthetic_allow(limit: Limit) -> Decision {
    Decision::new(true, limit.max, limit.max, limit.per, None)
}

fn check_bucket(bucket: &str) -> Result<()> {
    if bucket.is_empty() {
        return Err(ForgeError::invalid("ratelimit bucket must not be empty"));
    }
    if bucket.len() > MAX_BUCKET_BYTES {
        return Err(ForgeError::limit(format!(
            "bucket is {} bytes; max is {MAX_BUCKET_BYTES}",
            bucket.len()
        )));
    }
    Ok(())
}

fn check_key(key: &str) -> Result<()> {
    if key.is_empty() {
        return Err(ForgeError::invalid("ratelimit key must not be empty"));
    }
    if key.len() > MAX_KEY_BYTES {
        return Err(ForgeError::limit(format!(
            "key is {} bytes; max is {MAX_KEY_BYTES}",
            key.len()
        )));
    }
    Ok(())
}

fn check_limit(limit: &Limit) -> Result<()> {
    if limit.max == 0 {
        return Err(ForgeError::invalid("Limit.max must be > 0"));
    }
    if limit.per.is_zero() {
        return Err(ForgeError::invalid("Limit.per must be > 0"));
    }
    if limit.per.as_secs_f64() > MAX_PER_SECS {
        return Err(ForgeError::limit("Limit.per exceeds the maximum"));
    }
    Ok(())
}

/// One token-bucket step. `stored` is the current token count (`None` = fresh full
/// bucket). Returns the tokens to persist and the resulting `Decision`. Identical to the
/// Postgres backend's step so a given `(state, elapsed, limit)` yields the same decision.
fn token_bucket_step(stored: Option<f64>, elapsed_secs: f64, limit: Limit) -> (f64, Decision) {
    let max = f64::from(limit.max);
    let per = limit.per.as_secs_f64().max(1.0);
    let rate = max / per;
    let tokens = stored.unwrap_or(max);
    let refilled = (tokens + elapsed_secs.max(0.0) * rate).min(max);
    let allowed = refilled >= 1.0;
    let new_tokens = if allowed { refilled - 1.0 } else { refilled };
    let remaining = new_tokens.max(0.0).floor() as u32;
    let reset_after = Duration::from_secs_f64(((max - new_tokens) / rate).max(0.0));
    let retry_after =
        (!allowed).then(|| Duration::from_secs_f64(((1.0 - refilled) / rate).max(0.0)));
    (
        new_tokens,
        Decision::new(allowed, limit.max, remaining, reset_after, retry_after),
    )
}

/// Sliding-window state for one subject. Mirrors the Postgres row's window columns.
#[derive(Clone)]
struct SlidingState {
    window_start: f64,
    cur: i64,
    prev: i64,
}

/// One sliding-window step (fixed window with weighted prior, the standard approximate
/// sliding count). Returns the state to persist and the `Decision`. Identical to the
/// Postgres backend's step.
fn sliding_step(
    stored: Option<SlidingState>,
    now_epoch: f64,
    limit: Limit,
) -> (SlidingState, Decision) {
    let max = f64::from(limit.max);
    let per = limit.per.as_secs_f64().max(1.0);
    let cur_index = (now_epoch / per).floor() as i64;
    let window_start = cur_index as f64 * per;

    let (mut cur, prev) = match stored {
        Some(s) => {
            let stored_index = (s.window_start / per).floor() as i64;
            if stored_index == cur_index {
                (s.cur, s.prev)
            } else if stored_index == cur_index - 1 {
                (0, s.cur)
            } else {
                (0, 0)
            }
        }
        None => (0, 0),
    };

    let elapsed_in_win = (now_epoch - window_start).clamp(0.0, per);
    let weight = ((per - elapsed_in_win) / per).clamp(0.0, 1.0);
    let weighted = prev as f64 * weight + cur as f64;
    let allowed = weighted < max;
    if allowed {
        cur += 1;
    }
    let used = weighted + if allowed { 1.0 } else { 0.0 };
    let remaining = (max - used).max(0.0).floor() as u32;
    let reset_after = Duration::from_secs_f64((per - elapsed_in_win).max(0.0));
    let retry_after = (!allowed).then_some(reset_after);
    (
        SlidingState {
            window_start,
            cur,
            prev,
        },
        Decision::new(allowed, limit.max, remaining, reset_after, retry_after),
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
    use super::*;

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

    #[test]
    fn token_bucket_step_refills_over_time() {
        // 60 tokens / 60s = 1 token/sec; from empty, 5s of refill yields 5 tokens, one of
        // which this call consumes.
        let limit = tb(60, 60);
        let (_t, d) = token_bucket_step(Some(0.0), 5.0, limit);
        assert!(d.allowed);
        assert_eq!(d.remaining, 4);
    }

    #[test]
    fn sliding_step_resets_in_a_later_window() {
        let limit = sw(1, 100);
        let t = 1_000_000.0;
        let (s1, d1) = sliding_step(None, t, limit);
        assert!(d1.allowed);
        let (_s2, d2) = sliding_step(Some(s1.clone()), t, limit);
        assert!(!d2.allowed, "second call in the same window is denied");
        // A gap larger than one window is treated as fresh and allowed again.
        let (_s3, d3) = sliding_step(Some(s1), t + 10_000.0, limit);
        assert!(d3.allowed);
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

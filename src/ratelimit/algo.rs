//! Validation, failure-mode resolution, and the pure algorithm steps shared by
//! every ratelimit backend. Both the Postgres and in-memory limiters call into
//! this module, so a given `(state, elapsed, limit)` yields the same decision
//! everywhere and the backends cannot drift apart.

use super::{Decision, FailMode, Limit, MAX_BUCKET_BYTES, MAX_KEY_BYTES};
use crate::error::{ForgeError, Result};
use std::time::Duration;

/// Upper bound on `Limit.per` (~100 years), matching the kv TTL ceiling for
/// cross-vendor agreement. Over => `Limit`.
pub(super) const MAX_PER_SECS: f64 = 100.0 * 365.0 * 24.0 * 60.0 * 60.0;

/// Resolve a per-call [`FailMode`] against the instance default.
pub(super) fn resolve_fail_open(fail: FailMode, instance_default: bool) -> bool {
    match fail {
        FailMode::Default => instance_default,
        FailMode::Open => true,
        FailMode::Closed => false,
        // `FailMode` is `#[non_exhaustive]`; an unknown mode falls back to the
        // instance default.
        #[allow(unreachable_patterns)]
        _ => instance_default,
    }
}

/// Soft errors (transient/backend) are swallowed by fail-open; `Invalid`/`Limit`
/// (caller bugs) always surface regardless of failure mode.
pub(super) fn is_soft_error(e: &ForgeError) -> bool {
    matches!(e, ForgeError::Unavailable(_) | ForgeError::Backend { .. })
}

pub(super) fn synthetic_allow(limit: Limit) -> Decision {
    Decision::new(true, limit.max, limit.max, limit.per, None)
}

pub(super) fn check_bucket(bucket: &str) -> Result<()> {
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

pub(super) fn check_key(key: &str) -> Result<()> {
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

pub(super) fn check_limit(limit: &Limit) -> Result<()> {
    if limit.max == 0 {
        return Err(ForgeError::invalid("Limit.max must be > 0"));
    }
    if limit.per.is_zero() {
        return Err(ForgeError::invalid("Limit.per must be > 0"));
    }
    // The steps work in whole seconds; a shorter window would be silently
    // reinterpreted as 1s (a stricter limit than asked for), so reject it.
    if limit.per < Duration::from_secs(1) {
        return Err(ForgeError::invalid(
            "Limit.per must be at least 1 second (the limiter has seconds precision)",
        ));
    }
    if limit.per.as_secs_f64() > MAX_PER_SECS {
        return Err(ForgeError::limit("Limit.per exceeds the maximum"));
    }
    Ok(())
}

/// One token-bucket step. `stored` is the current token count (`None` = fresh full
/// bucket). Returns the tokens to persist and the resulting `Decision`.
pub(super) fn token_bucket_step(
    stored: Option<f64>,
    elapsed_secs: f64,
    limit: Limit,
) -> (f64, Decision) {
    let max = f64::from(limit.max);
    // `check_limit` guarantees `per >= 1s`; the clamp only guards direct callers.
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
pub(super) struct SlidingState {
    pub(super) window_start: f64,
    pub(super) cur: i64,
    pub(super) prev: i64,
}

/// One sliding-window step (fixed window with weighted prior, the standard
/// approximate sliding count). Returns the state to persist and the `Decision`.
pub(super) fn sliding_step(
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
    use super::super::Algo;
    use super::*;

    fn tb(max: u32, per_secs: u64) -> Limit {
        Limit::per_duration(max, Duration::from_secs(per_secs))
    }

    #[test]
    fn token_bucket_consumes_then_denies_when_empty() {
        let limit = tb(3, 60);
        let (t1, d1) = token_bucket_step(None, 0.0, limit);
        assert!(d1.allowed && d1.remaining == 2);
        let (t2, d2) = token_bucket_step(Some(t1), 0.0, limit);
        assert!(d2.allowed && d2.remaining == 1);
        let (t3, d3) = token_bucket_step(Some(t2), 0.0, limit);
        assert!(d3.allowed && d3.remaining == 0);
        let (_t4, d4) = token_bucket_step(Some(t3), 0.0, limit);
        assert!(!d4.allowed && d4.remaining == 0);
        assert!(d4.retry_after.is_some());
    }

    #[test]
    fn token_bucket_refills_over_time() {
        // 60 tokens / 60s = 1 token/sec; from empty, 5s of refill yields 5 tokens,
        // one of which this call consumes.
        let limit = tb(60, 60);
        let (_t, d) = token_bucket_step(Some(0.0), 5.0, limit);
        assert!(d.allowed);
        assert_eq!(d.remaining, 4);
    }

    #[test]
    fn sliding_window_caps_per_window() {
        let limit = tb(2, 100).with_algo(Algo::SlidingWindow);
        let t = 1_000_000.0;
        let (s1, d1) = sliding_step(None, t, limit);
        assert!(d1.allowed);
        let (s2, d2) = sliding_step(Some(s1), t, limit);
        assert!(d2.allowed);
        let (_s3, d3) = sliding_step(Some(s2), t, limit);
        assert!(!d3.allowed, "third call in the window is denied");
        assert!(d3.retry_after.is_some());
    }

    #[test]
    fn sliding_window_resets_next_window() {
        let limit = tb(1, 100).with_algo(Algo::SlidingWindow);
        let t = 1_000_000.0;
        let (s1, d1) = sliding_step(None, t, limit);
        assert!(d1.allowed);
        let (_s2, d2) = sliding_step(Some(s1.clone()), t, limit);
        assert!(!d2.allowed);
        // Far in the future (gap > 1 window): treated as fresh, allowed again.
        let (_s3, d3) = sliding_step(Some(s1), t + 10_000.0, limit);
        assert!(d3.allowed);
    }

    #[test]
    fn sub_second_windows_are_rejected_not_reinterpreted() {
        let ok = tb(10, 1);
        assert!(check_limit(&ok).is_ok());
        let short = Limit::per_duration(10, Duration::from_millis(250));
        assert!(matches!(check_limit(&short), Err(ForgeError::Invalid(_))));
        let zero = Limit::per_duration(10, Duration::ZERO);
        assert!(matches!(check_limit(&zero), Err(ForgeError::Invalid(_))));
    }
}

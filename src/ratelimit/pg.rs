//! Postgres `ratelimit` backend. Contract: docs/contracts/ratelimit.md.
//!
//! Each `check` is atomic against its one `(bucket, subject)` row: the row is locked
//! `FOR UPDATE` inside a transaction, the algorithm math runs in Rust against the
//! locked state, and the new state is written before commit — so concurrent checks on
//! one key serialize and never double-spend. The math is pure (see `*_step`), so it is
//! unit-tested without a database.

use super::{Algo, Decision, FailMode, Limit, MAX_BUCKET_BYTES, MAX_KEY_BYTES, RateLimit};
use crate::error::{ForgeError, Result};
use crate::obs;
use crate::util::key_hash;
use async_trait::async_trait;
use sqlx::PgPool;
use std::time::Duration;
use tracing::field::Empty;

/// Upper bound on `Limit.per` (~100 years), matching the kv TTL ceiling for
/// cross-vendor agreement. Over => `Limit`.
const MAX_PER_SECS: f64 = 100.0 * 365.0 * 24.0 * 60.0 * 60.0;
/// Idle rows untouched this long are purged by the sweep; an idle bucket refills to
/// full / a window ages out long before this, so dropping it changes nothing.
const IDLE_PURGE_SECS: f64 = 24.0 * 60.0 * 60.0;

/// Postgres-backed [`RateLimit`].
pub(crate) struct PgRateLimit {
    pool: PgPool,
    namespace: String,
    fail_open: bool,
}

impl PgRateLimit {
    pub(crate) fn new(pool: PgPool, namespace: String, fail_open: bool) -> Self {
        Self {
            pool,
            namespace,
            fail_open,
        }
    }

    /// Namespaced bucket value. The namespace is colon-free, so `<ns>:<bucket>` never
    /// collides across distinct `(ns, bucket)`.
    fn ns_bucket(&self, bucket: &str) -> String {
        if self.namespace.is_empty() {
            bucket.to_string()
        } else {
            format!("{}:{}", self.namespace, bucket)
        }
    }

    /// Delete rows untouched for longer than the idle window. Idempotent.
    pub(crate) async fn sweep(&self) -> Result<u64> {
        let r = sqlx::query!(
            "DELETE FROM forge_ratelimit \
             WHERE updated_at <= now() - make_interval(secs => $1)",
            IDLE_PURGE_SECS
        )
        .execute(&self.pool)
        .await?;
        Ok(r.rows_affected())
    }

    async fn check_token_bucket(
        &self,
        bucket: &str,
        subject: &str,
        limit: Limit,
    ) -> Result<Decision> {
        let mut tx = self.pool.begin().await?;
        sqlx::query!(
            "INSERT INTO forge_ratelimit (bucket, subject) VALUES ($1, $2) \
             ON CONFLICT (bucket, subject) DO NOTHING",
            bucket,
            subject,
        )
        .execute(&mut *tx)
        .await?;
        let row = sqlx::query!(
            r#"SELECT tokens,
                      EXTRACT(EPOCH FROM (now() - updated_at))::float8 AS "elapsed!"
               FROM forge_ratelimit WHERE bucket = $1 AND subject = $2 FOR UPDATE"#,
            bucket,
            subject,
        )
        .fetch_one(&mut *tx)
        .await?;

        let (new_tokens, decision) = token_bucket_step(row.tokens, row.elapsed, limit);
        sqlx::query!(
            "UPDATE forge_ratelimit SET tokens = $3, updated_at = now() \
             WHERE bucket = $1 AND subject = $2",
            bucket,
            subject,
            new_tokens,
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(decision)
    }

    async fn check_sliding(&self, bucket: &str, subject: &str, limit: Limit) -> Result<Decision> {
        let mut tx = self.pool.begin().await?;
        sqlx::query!(
            "INSERT INTO forge_ratelimit (bucket, subject) VALUES ($1, $2) \
             ON CONFLICT (bucket, subject) DO NOTHING",
            bucket,
            subject,
        )
        .execute(&mut *tx)
        .await?;
        let row = sqlx::query!(
            r#"SELECT window_start, cur_count, prev_count,
                      EXTRACT(EPOCH FROM now())::float8 AS "now_epoch!"
               FROM forge_ratelimit WHERE bucket = $1 AND subject = $2 FOR UPDATE"#,
            bucket,
            subject,
        )
        .fetch_one(&mut *tx)
        .await?;

        let stored = row.window_start.map(|ws| SlidingState {
            window_start: ws,
            cur: i64::from(row.cur_count.unwrap_or(0)),
            prev: i64::from(row.prev_count.unwrap_or(0)),
        });
        let (state, decision) = sliding_step(stored, row.now_epoch, limit);
        sqlx::query!(
            "UPDATE forge_ratelimit \
             SET window_start = $3, cur_count = $4, prev_count = $5, updated_at = now() \
             WHERE bucket = $1 AND subject = $2",
            bucket,
            subject,
            state.window_start,
            i32::try_from(state.cur).unwrap_or(i32::MAX),
            i32::try_from(state.prev).unwrap_or(i32::MAX),
        )
        .execute(&mut *tx)
        .await?;
        tx.commit().await?;
        Ok(decision)
    }
}

impl crate::sealed::Sealed for PgRateLimit {}

#[async_trait]
impl RateLimit for PgRateLimit {
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
        };
        let span = tracing::info_span!(
            "forge.ratelimit.check",
            ratelimit.bucket = %bucket,
            ratelimit.key_hash = %key_hash(key),
            ratelimit.algo = algo_label(limit.algo),
            ratelimit.limit = limit.max,
            ratelimit.allowed = Empty,
            ratelimit.remaining = Empty,
            ratelimit.reset_after_secs = Empty,
            ratelimit.retry_after_secs = Empty,
            ratelimit.fail_open = Empty,
            ratelimit.outcome = Empty,
            outcome = Empty,
            error.variant = Empty,
        );
        obs::instrument("ratelimit", "check", span, async move {
            check_bucket(bucket)?;
            check_key(key)?;
            check_limit(&limit)?;
            let ns_bucket = self.ns_bucket(bucket);
            let result = match limit.algo {
                Algo::TokenBucket => self.check_token_bucket(&ns_bucket, key, limit).await,
                Algo::SlidingWindow => self.check_sliding(&ns_bucket, key, limit).await,
            };
            let decision = match result {
                Ok(d) => d,
                Err(e) if fail_open && is_soft_error(&e) => {
                    tracing::warn!(error = %e, "ratelimit backend error; failing open (allowing)");
                    tracing::Span::current().record("ratelimit.fail_open", true);
                    synthetic_allow(limit)
                }
                Err(e) => return Err(e),
            };
            let s = tracing::Span::current();
            s.record("ratelimit.allowed", decision.allowed);
            s.record("ratelimit.remaining", decision.remaining);
            s.record("ratelimit.reset_after_secs", decision.reset_after.as_secs());
            if let Some(ra) = decision.retry_after {
                s.record("ratelimit.retry_after_secs", ra.as_secs());
            }
            s.record(
                "ratelimit.outcome",
                if decision.allowed {
                    "allowed"
                } else {
                    "denied"
                },
            );
            Ok(decision)
        })
        .await
    }
}

fn algo_label(algo: Algo) -> &'static str {
    match algo {
        Algo::TokenBucket => "token_bucket",
        Algo::SlidingWindow => "sliding_window",
    }
}

/// Soft errors (transient/backend) are swallowed by fail-open; `Invalid`/`Limit`
/// (caller bugs) always surface regardless of failure mode.
fn is_soft_error(e: &ForgeError) -> bool {
    matches!(e, ForgeError::Unavailable(_) | ForgeError::Backend { .. })
}

fn synthetic_allow(limit: Limit) -> Decision {
    Decision {
        allowed: true,
        limit: limit.max,
        remaining: limit.max,
        reset_after: limit.per,
        retry_after: None,
    }
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
/// bucket). Returns the tokens to persist and the resulting `Decision`.
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
        Decision {
            allowed,
            limit: limit.max,
            remaining,
            reset_after,
            retry_after,
        },
    )
}

/// Sliding-window state stored in the row.
#[derive(Clone)]
struct SlidingState {
    window_start: f64,
    cur: i64,
    prev: i64,
}

/// One sliding-window step (fixed window with weighted prior — the standard
/// approximate sliding count). Returns the state to persist and the `Decision`.
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
        Decision {
            allowed,
            limit: limit.max,
            remaining,
            reset_after,
            retry_after,
        },
    )
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::panic)]
mod tests {
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
}

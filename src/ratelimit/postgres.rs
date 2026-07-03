use super::algo::{
    SlidingState, check_bucket, check_key, check_limit, is_soft_error, resolve_fail_open,
    sliding_step, synthetic_allow, token_bucket_step,
};
use super::{Algo, Decision, FailMode, Limit, RateLimit};
use crate::error::Result;
use crate::obs;
use crate::util::key_hash;
use async_trait::async_trait;
use sqlx::PgPool;
use tracing::field::Empty;

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
        crate::util::namespaced(&self.namespace, bucket)
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

#[async_trait]
impl RateLimit for PgRateLimit {
    async fn check_with(
        &self,
        bucket: &str,
        key: &str,
        limit: Limit,
        fail: FailMode,
    ) -> Result<Decision> {
        let fail_open = resolve_fail_open(fail, self.fail_open);
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
                // `Algo` is `#[non_exhaustive]`; default to token bucket.
                #[allow(unreachable_patterns)]
                _ => self.check_token_bucket(&ns_bucket, key, limit).await,
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
        #[allow(unreachable_patterns)]
        _ => "unknown",
    }
}

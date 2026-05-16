use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use chrono::{DateTime, Utc};
use dashmap::DashMap;
use sqlx::PgPool;

use forge_core::rate_limit::{RateLimitConfig, RateLimitKey, RateLimitResult, RateLimiterBackend};
use forge_core::{AuthContext, ForgeError, RequestMetadata, Result};

/// Strict rate limiter backed entirely by PostgreSQL.
///
/// Every check round-trips to PG, so limits are cluster-wide correct at the
/// cost of one query per rate-limited request. Right for billing-grade
/// quotas; for DDoS protection prefer [`HybridRateLimiter`].
///
/// Implements token bucket algorithm with atomic updates.
pub struct StrictRateLimiter {
    pool: PgPool,
}

impl StrictRateLimiter {
    /// Create a new rate limiter.
    pub fn new(pool: PgPool) -> Self {
        Self { pool }
    }

    /// Check rate limit for a bucket key.
    pub async fn check(
        &self,
        bucket_key: &str,
        config: &RateLimitConfig,
    ) -> Result<RateLimitResult> {
        let max_tokens = config.requests as f64;
        let refill_rate = config.refill_rate();

        // Atomic upsert with token bucket logic
        let result = sqlx::query!(
            r#"
            INSERT INTO forge_rate_limits (bucket_key, tokens, last_refill, max_tokens, refill_rate)
            VALUES ($1, $2 - 1, NOW(), $2, $3)
            ON CONFLICT (bucket_key) DO UPDATE SET
                tokens = LEAST(
                    forge_rate_limits.max_tokens::double precision,
                    forge_rate_limits.tokens +
                        (EXTRACT(EPOCH FROM (NOW() - forge_rate_limits.last_refill)) * forge_rate_limits.refill_rate)
                ) - 1,
                last_refill = NOW()
            RETURNING tokens, max_tokens, last_refill, (tokens >= 0) as "allowed!"
            "#,
            bucket_key,
            max_tokens as i32,
            refill_rate
        )
        .fetch_one(&self.pool)
        .await
        .map_err(ForgeError::Database)?;

        let tokens = result.tokens;
        let last_refill = result.last_refill;
        let allowed = result.allowed;

        let remaining = tokens.max(0.0) as u32;
        let reset_at =
            last_refill + chrono::Duration::seconds(((max_tokens - tokens) / refill_rate) as i64);

        if allowed {
            Ok(RateLimitResult::allowed(remaining, reset_at))
        } else {
            let retry_after = Duration::from_secs_f64((1.0 - tokens) / refill_rate);
            Ok(RateLimitResult::denied(remaining, reset_at, retry_after))
        }
    }

    /// Build a bucket key for the given parameters.
    pub fn build_key(
        &self,
        key_type: RateLimitKey,
        action_name: &str,
        auth: &AuthContext,
        request: &RequestMetadata,
    ) -> String {
        match key_type {
            RateLimitKey::User => {
                let user_id = auth.user_id().map(|u| u.to_string()).unwrap_or_else(|| {
                    let ip = request.client_ip().unwrap_or("unknown");
                    format!("anon-{ip}")
                });
                format!("user:{}:{}", user_id, action_name)
            }
            RateLimitKey::Ip => {
                let ip = request.client_ip().unwrap_or("unknown");
                format!("ip:{}:{}", ip, action_name)
            }
            RateLimitKey::Tenant => {
                let tenant_id = auth
                    .claim("tenant_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("none");
                format!("tenant:{}:{}", tenant_id, action_name)
            }
            RateLimitKey::UserAction => {
                let user_id = auth
                    .user_id()
                    .map(|u| u.to_string())
                    .unwrap_or_else(|| "anonymous".to_string());
                format!("user_action:{}:{}", user_id, action_name)
            }
            RateLimitKey::Global => {
                format!("global:{}", action_name)
            }
            RateLimitKey::Custom(claim_name) => {
                let value = auth
                    .claim(&claim_name)
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown");
                format!("custom:{}:{}:{}", claim_name, value, action_name)
            }
            // RateLimitKey is #[non_exhaustive]; future keys collapse to a
            // global bucket until the runtime adds an explicit handler.
            _ => format!("global:{}", action_name),
        }
    }

    /// Check rate limit and return an error if exceeded.
    pub async fn enforce(
        &self,
        bucket_key: &str,
        config: &RateLimitConfig,
    ) -> Result<RateLimitResult> {
        let result = self.check(bucket_key, config).await?;
        if !result.allowed {
            #[cfg(feature = "gateway")]
            crate::signals::emit_diagnostic(
                "rate_limit.exceeded",
                serde_json::json!({
                    "bucket": bucket_key,
                    "limit": config.requests,
                    "remaining": result.remaining,
                    "retry_after_ms": result
                        .retry_after
                        .unwrap_or(Duration::from_secs(1))
                        .as_millis() as u64,
                }),
                None,
                None,
                None,
                None,
                false,
            );
            return Err(ForgeError::RateLimitExceeded {
                retry_after: result.retry_after.unwrap_or(Duration::from_secs(1)),
                limit: config.requests,
                remaining: result.remaining,
            });
        }
        Ok(result)
    }

    /// Reset a rate limit bucket.
    pub async fn reset(&self, bucket_key: &str) -> Result<()> {
        sqlx::query!(
            "DELETE FROM forge_rate_limits WHERE bucket_key = $1",
            bucket_key
        )
        .execute(&self.pool)
        .await
        .map_err(ForgeError::Database)?;
        Ok(())
    }

    /// Clean up old rate limit entries.
    pub async fn cleanup(&self, older_than: DateTime<Utc>) -> Result<u64> {
        let result = sqlx::query!(
            r#"
            DELETE FROM forge_rate_limits
            WHERE created_at < $1
            "#,
            older_than,
        )
        .execute(&self.pool)
        .await
        .map_err(ForgeError::Database)?;

        Ok(result.rows_affected())
    }
}

struct LocalBucket {
    tokens: f64,
    max_tokens: f64,
    refill_rate: f64,
    last_refill: std::time::Instant,
}

impl LocalBucket {
    fn new(max_tokens: f64, refill_rate: f64) -> Self {
        Self {
            tokens: max_tokens,
            max_tokens,
            refill_rate,
            last_refill: std::time::Instant::now(),
        }
    }

    fn try_consume(&mut self) -> bool {
        let now = std::time::Instant::now();
        let elapsed = now.duration_since(self.last_refill).as_secs_f64();
        self.tokens = (self.tokens + elapsed * self.refill_rate).min(self.max_tokens);
        self.last_refill = now;

        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }

    fn remaining(&self) -> u32 {
        self.tokens.max(0.0) as u32
    }

    fn time_until_token(&self) -> Duration {
        if self.tokens >= 1.0 {
            Duration::ZERO
        } else {
            Duration::from_secs_f64((1.0 - self.tokens) / self.refill_rate)
        }
    }
}

/// Hybrid rate limiter with in-memory fast path and periodic DB sync.
///
/// Per-user/per-IP checks use a local DashMap for sub-microsecond decisions,
/// so a `100 req/min` limit becomes `100 × N` across an N-node cluster. Right
/// for DDoS protection where the threshold is approximate. For cluster-wide
/// correctness (e.g. billing quotas) use [`StrictRateLimiter`].
///
/// `Global` keys always hit the database for cross-node consistency.
///
/// DESIGN: Per-node rate limiting. Cluster-wide consistency trades latency
/// for accuracy. With N nodes, effective limit is N× per-key. Keep per-node
/// budgets low.
pub struct HybridRateLimiter {
    local: DashMap<String, LocalBucket>,
    db_limiter: StrictRateLimiter,
    max_local_buckets: usize,
}

impl HybridRateLimiter {
    pub fn new(pool: PgPool) -> Self {
        Self::with_max_buckets(pool, 100_000)
    }

    /// Create a hybrid rate limiter with a custom local bucket limit.
    pub fn with_max_buckets(pool: PgPool, max_local_buckets: usize) -> Self {
        Self {
            local: DashMap::new(),
            db_limiter: StrictRateLimiter::new(pool),
            max_local_buckets,
        }
    }

    /// Check rate limit. Uses local fast path for per-user/per-IP keys,
    /// database for global keys.
    pub async fn check(
        &self,
        bucket_key: &str,
        config: &RateLimitConfig,
    ) -> Result<RateLimitResult> {
        if config.key == RateLimitKey::Global {
            return self.db_limiter.check(bucket_key, config).await;
        }

        let max_tokens = config.requests as f64;
        let refill_rate = config.refill_rate();

        // Evict idle buckets when the map gets too large to prevent memory exhaustion
        if self.local.len() > self.max_local_buckets {
            self.cleanup_local(Duration::from_secs(300)); // evict entries idle > 5 min
        }

        let mut bucket = self
            .local
            .entry(bucket_key.to_string())
            .or_insert_with(|| LocalBucket::new(max_tokens, refill_rate));

        let allowed = bucket.try_consume();
        let remaining = bucket.remaining();
        let reset_at = Utc::now()
            + chrono::Duration::seconds(((max_tokens - bucket.tokens) / refill_rate) as i64);

        if allowed {
            Ok(RateLimitResult::allowed(remaining, reset_at))
        } else {
            let retry_after = bucket.time_until_token();
            Ok(RateLimitResult::denied(remaining, reset_at, retry_after))
        }
    }

    pub fn build_key(
        &self,
        key_type: RateLimitKey,
        action_name: &str,
        auth: &AuthContext,
        request: &RequestMetadata,
    ) -> String {
        self.db_limiter
            .build_key(key_type, action_name, auth, request)
    }

    pub async fn enforce(
        &self,
        bucket_key: &str,
        config: &RateLimitConfig,
    ) -> Result<RateLimitResult> {
        let result = self.check(bucket_key, config).await?;
        if !result.allowed {
            return Err(ForgeError::RateLimitExceeded {
                retry_after: result.retry_after.unwrap_or(Duration::from_secs(1)),
                limit: config.requests,
                remaining: result.remaining,
            });
        }
        Ok(result)
    }

    /// Clean up expired local buckets (call periodically).
    pub fn cleanup_local(&self, max_idle: Duration) {
        let cutoff = std::time::Instant::now()
            .checked_sub(max_idle)
            .unwrap_or(std::time::Instant::now());
        self.local.retain(|_, bucket| bucket.last_refill > cutoff);
    }
}

impl RateLimiterBackend for StrictRateLimiter {
    fn check<'a>(
        &'a self,
        bucket_key: &'a str,
        config: &'a RateLimitConfig,
    ) -> Pin<Box<dyn Future<Output = Result<RateLimitResult>> + Send + 'a>> {
        Box::pin(StrictRateLimiter::check(self, bucket_key, config))
    }

    fn build_key(
        &self,
        key_type: RateLimitKey,
        action_name: &str,
        auth: &AuthContext,
        request: &RequestMetadata,
    ) -> String {
        StrictRateLimiter::build_key(self, key_type, action_name, auth, request)
    }

    fn enforce<'a>(
        &'a self,
        bucket_key: &'a str,
        config: &'a RateLimitConfig,
    ) -> Pin<Box<dyn Future<Output = Result<RateLimitResult>> + Send + 'a>> {
        Box::pin(StrictRateLimiter::enforce(self, bucket_key, config))
    }
}

impl RateLimiterBackend for HybridRateLimiter {
    fn check<'a>(
        &'a self,
        bucket_key: &'a str,
        config: &'a RateLimitConfig,
    ) -> Pin<Box<dyn Future<Output = Result<RateLimitResult>> + Send + 'a>> {
        Box::pin(HybridRateLimiter::check(self, bucket_key, config))
    }

    fn build_key(
        &self,
        key_type: RateLimitKey,
        action_name: &str,
        auth: &AuthContext,
        request: &RequestMetadata,
    ) -> String {
        HybridRateLimiter::build_key(self, key_type, action_name, auth, request)
    }

    fn enforce<'a>(
        &'a self,
        bucket_key: &'a str,
        config: &'a RateLimitConfig,
    ) -> Pin<Box<dyn Future<Output = Result<RateLimitResult>> + Send + 'a>> {
        Box::pin(HybridRateLimiter::enforce(self, bucket_key, config))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_rate_limiter_creation() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/test")
            .expect("Failed to create mock pool");

        let _limiter = StrictRateLimiter::new(pool);
    }

    #[tokio::test]
    async fn test_build_key() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/test")
            .expect("Failed to create mock pool");

        let limiter = StrictRateLimiter::new(pool);
        let auth = AuthContext::unauthenticated();
        let request = RequestMetadata::default();

        let key = limiter.build_key(RateLimitKey::Global, "test_action", &auth, &request);
        assert_eq!(key, "global:test_action");

        let key = limiter.build_key(RateLimitKey::User, "test_action", &auth, &request);
        assert!(key.starts_with("user:"));
    }
}

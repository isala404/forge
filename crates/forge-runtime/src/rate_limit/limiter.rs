use std::time::Duration;

use chrono::{DateTime, Utc};
use sqlx::PgPool;

use forge_core::rate_limit::{RateLimitConfig, RateLimitKey, RateLimitResult};
use forge_core::{AuthContext, ForgeError, RequestMetadata, Result};

/// Rate limiter using PostgreSQL for state storage.
///
/// Implements token bucket algorithm with atomic updates.
pub struct RateLimiter {
    pool: PgPool,
}

impl RateLimiter {
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
        let result: (f64, i32, DateTime<Utc>, bool) = sqlx::query_as(
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
            RETURNING tokens, max_tokens, last_refill, (tokens >= 0) as allowed
            "#,
        )
        .bind(bucket_key)
        .bind(max_tokens as i32)
        .bind(refill_rate)
        .fetch_one(&self.pool)
        .await
        .map_err(|e| ForgeError::Database(e.to_string()))?;

        let (tokens, _max, last_refill, allowed) = result;

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
                let user_id = auth
                    .user_id()
                    .map(|u| u.to_string())
                    .unwrap_or_else(|| "anonymous".to_string());
                format!("user:{}:{}", user_id, action_name)
            }
            RateLimitKey::Ip => {
                let ip = request.client_ip.as_deref().unwrap_or("unknown");
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
        sqlx::query("DELETE FROM forge_rate_limits WHERE bucket_key = $1")
            .bind(bucket_key)
            .execute(&self.pool)
            .await
            .map_err(|e| ForgeError::Database(e.to_string()))?;
        Ok(())
    }

    /// Clean up old rate limit entries.
    pub async fn cleanup(&self, older_than: DateTime<Utc>) -> Result<u64> {
        let result = sqlx::query(
            r#"
            DELETE FROM forge_rate_limits
            WHERE created_at < $1
            "#,
        )
        .bind(older_than)
        .execute(&self.pool)
        .await
        .map_err(|e| ForgeError::Database(e.to_string()))?;

        Ok(result.rows_affected())
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

        let _limiter = RateLimiter::new(pool);
    }

    #[tokio::test]
    async fn test_build_key() {
        let pool = sqlx::postgres::PgPoolOptions::new()
            .max_connections(1)
            .connect_lazy("postgres://localhost/test")
            .expect("Failed to create mock pool");

        let limiter = RateLimiter::new(pool);
        let auth = AuthContext::unauthenticated();
        let request = RequestMetadata::default();

        let key = limiter.build_key(RateLimitKey::Global, "test_action", &auth, &request);
        assert_eq!(key, "global:test_action");

        let key = limiter.build_key(RateLimitKey::User, "test_action", &auth, &request);
        assert!(key.starts_with("user:"));
    }
}

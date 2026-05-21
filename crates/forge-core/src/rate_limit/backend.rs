use std::future::Future;
use std::pin::Pin;

use crate::function::{AuthContext, RequestMetadata};
use crate::rate_limit::{RateLimitConfig, RateLimitKey, RateLimitResult};
use crate::{ForgeError, Result};

/// Pluggable rate-limiter implementation.
///
/// The runtime exposes two implementations:
/// - `HybridRateLimiter`: per-node DashMap fast path with PG fallback for
///   `Global` keys. Approximate under multi-node deployments — user/IP limits
///   multiply by the node count. Right for DDoS protection.
/// - `StrictRateLimiter`: every check round-trips to PostgreSQL. Cluster-wide
///   correct. Right for billing-grade or quota enforcement.
///
/// Both ship with the framework. Users implement this trait themselves only
/// when their backing store sits outside the runtime's PG-only contract.
pub trait RateLimiterBackend: Send + Sync + 'static {
    /// Check whether a single token is available for the given bucket.
    fn check<'a>(
        &'a self,
        bucket_key: &'a str,
        config: &'a RateLimitConfig,
    ) -> Pin<Box<dyn Future<Output = Result<RateLimitResult>> + Send + 'a>>;

    /// Build the bucket key string for a (key kind, action, auth, request) tuple.
    fn build_key(
        &self,
        key_type: RateLimitKey,
        action_name: &str,
        auth: &AuthContext,
        request: &RequestMetadata,
    ) -> String;

    /// Check and convert a denial into a [`ForgeError::RateLimitExceeded`].
    fn enforce<'a>(
        &'a self,
        bucket_key: &'a str,
        config: &'a RateLimitConfig,
    ) -> Pin<Box<dyn Future<Output = Result<RateLimitResult>> + Send + 'a>> {
        Box::pin(async move {
            let result = self.check(bucket_key, config).await?;
            if !result.allowed {
                return Err(ForgeError::RateLimitExceeded {
                    retry_after: result
                        .retry_after
                        .unwrap_or(std::time::Duration::from_secs(1)),
                    limit: config.requests,
                    remaining: result.remaining,
                });
            }
            Ok(result)
        })
    }
}

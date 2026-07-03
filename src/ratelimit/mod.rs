use crate::error::Result;
use async_trait::async_trait;
use std::time::Duration;

/// Largest allowed bucket name, in bytes. Over => [`crate::error::ForgeError::Limit`].
pub const MAX_BUCKET_BYTES: usize = 128;
/// Largest allowed subject key, in bytes. Over => [`crate::error::ForgeError::Limit`].
pub const MAX_KEY_BYTES: usize = 512;

/// Rate-limit algorithm.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Algo {
    /// Continuous refill at `max / per`, bursting up to `max`.
    #[default]
    TokenBucket,
    /// Approximate trailing-window count (current + weighted prior window).
    SlidingWindow,
}

/// Per-check override of what happens when the limiter backend errors (not when a
/// request is merely denied, which is always `Ok(Decision { allowed: false })`).
/// A backend outage should fail-open for a high-volume best-effort bucket (a chat
/// message) but fail-closed for an abuse- or payment-sensitive one (login, OTP), so
/// one `RateLimit` can mix both without a global flag.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FailMode {
    /// Use the Forge-instance default (`forge.toml`'s `ratelimit.fail_open`).
    #[default]
    Default,
    /// On a soft/transient backend error, allow the request (and warn).
    Open,
    /// On any backend error, surface it (deny by erroring).
    Closed,
}

/// A rate-limit policy, passed per `check`. Policy lives in caller code, not server
/// config; the stored row tracks only consumption.
#[non_exhaustive]
#[derive(Debug, Clone, Copy)]
pub struct Limit {
    /// Max units admitted per window / bucket capacity. Must be `> 0`.
    pub max: u32,
    /// Window length (sliding) / refill period for `max` tokens (bucket). Must be
    /// `> 0`; seconds precision.
    pub per: Duration,
    /// Which algorithm to apply.
    pub algo: Algo,
}

impl Limit {
    /// `max` units per `per`, token-bucket (the common default). `const` so a typed
    /// `forgelib::RateBucket` policy can be declared as a `const`/`static`.
    pub const fn per_duration(max: u32, per: Duration) -> Self {
        Self {
            max,
            per,
            algo: Algo::TokenBucket,
        }
    }

    /// Switch the algorithm.
    pub const fn with_algo(mut self, algo: Algo) -> Self {
        self.algo = algo;
        self
    }
}

/// The outcome of a [`RateLimit::check`]. Laid out to map 1:1 onto the IETF
/// `RateLimit` header fields (see the contract).
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision {
    /// Whether this call was admitted (and consumed one unit).
    pub allowed: bool,
    /// Echoes `Limit.max` (`RateLimit-Limit`).
    pub limit: u32,
    /// Units left after this call (`RateLimit-Remaining`).
    pub remaining: u32,
    /// Time until the limit fully resets (`RateLimit-Reset`).
    pub reset_after: Duration,
    /// Earliest retry, set iff `!allowed` (`Retry-After`).
    pub retry_after: Option<Duration>,
}

impl Decision {
    /// Construct a decision. For backend implementors; app code receives this from
    /// [`RateLimit::check`].
    pub fn new(
        allowed: bool,
        limit: u32,
        remaining: u32,
        reset_after: Duration,
        retry_after: Option<Duration>,
    ) -> Self {
        Self {
            allowed,
            limit,
            remaining,
            reset_after,
            retry_after,
        }
    }
}

/// An atomic check-and-consume rate limiter. Lineage: token bucket / GCRA + IETF
/// RateLimit headers. Object-safe; the facade hands out `Arc<dyn RateLimit>`.
///
/// Exact algorithm math, failure modes, and limits: <https://tryforge.dev/primitives/#rate-limit>.
#[async_trait]
pub trait RateLimit: Send + Sync {
    /// Atomic check-and-consume of one unit against `limit` for subject `key` under
    /// namespace `bucket`. A denied request is `Ok(Decision { allowed: false, .. })`,
    /// never an `Err`. On a backend error the configured failure mode applies
    /// (fail-open by default: returns a synthetic allow and logs a warning).
    async fn check(&self, bucket: &str, key: &str, limit: Limit) -> Result<Decision> {
        self.check_with(bucket, key, limit, FailMode::Default).await
    }

    /// Like [`RateLimit::check`] but with a per-call [`FailMode`] overriding the
    /// instance default for what happens on a backend error. Only soft/transient
    /// errors are ever swallowed by fail-open; caller bugs (`Invalid`/`Limit`)
    /// always surface regardless of mode.
    async fn check_with(
        &self,
        bucket: &str,
        key: &str,
        limit: Limit,
        fail: FailMode,
    ) -> Result<Decision>;
}

mod algo;
mod memory;
mod postgres;
pub(crate) use memory::MemRateLimit;
pub(crate) use postgres::PgRateLimit;

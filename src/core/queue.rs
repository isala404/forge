//! `queue` — lineage: AWS SQS. See `docs/contracts/queue.md`.
//!
//! AT-LEAST-ONCE delivery: a job may be delivered more than once. Consumers
//! MUST be idempotent. Attempts increment on redelivery, never on claim.

use crate::error::{ForgeError, Result};
use async_trait::async_trait;
use bytes::Bytes;
use serde::Serialize;
use serde::de::DeserializeOwned;
use std::fmt;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// Largest allowed payload (256 KiB — the SQS `SendMessage` ceiling, enforced
/// so a future SQS backend stays honest). Over => [`crate::ForgeError::Limit`].
pub const MAX_PAYLOAD_BYTES: usize = 256 * 1024;

/// Longest a `dequeue` long-poll may wait (SQS max). Larger is clamped, not rejected.
pub const MAX_WAIT: Duration = Duration::from_secs(20);

/// Largest visibility timeout / lease (SQS max). Out of range => `Invalid`.
pub const MAX_VISIBILITY_TIMEOUT: Duration = Duration::from_secs(12 * 60 * 60);

/// Opaque job identifier (a UUID under the hood).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct JobId(pub Uuid);

impl JobId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }
}

impl Default for JobId {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Display for JobId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(f)
    }
}

/// Retry backoff strategy. Default is exponential with jitter.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Backoff {
    /// The same delay every retry.
    Fixed(Duration),
    /// `step * attempt`.
    Linear(Duration),
    /// `base * 2^(attempt-1)`, capped at `cap`.
    Exponential { base: Duration, cap: Duration },
}

impl Default for Backoff {
    fn default() -> Self {
        Self::Exponential {
            base: Duration::from_secs(1),
            cap: Duration::from_secs(300),
        }
    }
}

impl Backoff {
    /// Delay before the `attempt`-th retry (1-based), with ±25% jitter at
    /// millisecond precision. `seed` decorrelates a fleet retrying after a
    /// shared outage; pass something per-job (e.g. id bytes) for that effect.
    /// Saturating throughout — no panic at high attempt counts.
    pub fn delay_for_attempt(&self, attempt: u32, seed: u64) -> Duration {
        let n = attempt.max(1);
        let base_ms: u64 = match self {
            Backoff::Fixed(d) => duration_ms(*d),
            Backoff::Linear(step) => duration_ms(*step).saturating_mul(n as u64),
            Backoff::Exponential { base, cap } => {
                // 2^(n-1), saturating past 63 shifts.
                let factor = 1u64.checked_shl(n - 1).unwrap_or(u64::MAX);
                duration_ms(*base)
                    .saturating_mul(factor)
                    .min(duration_ms(*cap))
            }
        };
        Duration::from_millis(jitter_ms(base_ms, seed))
    }
}

/// `Duration` to whole milliseconds, saturating into `u64`.
fn duration_ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

/// Apply ±25% jitter to `base_ms` via a deterministic LCG seeded by `seed`. No
/// `rand` dependency; same seed yields same factor, keeping tests deterministic.
fn jitter_ms(base_ms: u64, seed: u64) -> u64 {
    if base_ms == 0 {
        return 0;
    }
    let r = seed
        .wrapping_mul(6_364_136_223_846_793_005)
        .wrapping_add(1_442_695_040_888_963_407);
    let frac = (r >> 11) as f64 / (1u64 << 53) as f64;
    let factor = 0.75 + 0.5 * frac; // [0.75, 1.25)
    (base_ms as f64 * factor) as u64
}

/// Options for [`Queue::enqueue`].
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct EnqueueOpts {
    /// SQS `DelaySeconds`: the job is invisible until `now + delay`.
    pub delay: Duration,
    /// SQS redrive `maxReceiveCount`: deliveries before dead-lettering.
    pub max_attempts: u32,
    /// Redelivery backoff.
    pub backoff: Backoff,
    /// SQS `MessageDeduplicationId`: dedups enqueues per `(queue, dedup_id)`
    /// within the dedup window. `None` disables dedup.
    pub dedup_id: Option<String>,
}

impl Default for EnqueueOpts {
    fn default() -> Self {
        Self {
            delay: Duration::ZERO,
            max_attempts: 5,
            backoff: Backoff::default(),
            dedup_id: None,
        }
    }
}

impl EnqueueOpts {
    /// Default options.
    pub fn new() -> Self {
        Self::default()
    }
    /// Set the initial visibility delay.
    pub fn with_delay(mut self, delay: Duration) -> Self {
        self.delay = delay;
        self
    }
    /// Set the maximum delivery attempts before dead-lettering.
    pub fn with_max_attempts(mut self, max_attempts: u32) -> Self {
        self.max_attempts = max_attempts;
        self
    }
    /// Set the retry backoff strategy.
    pub fn with_backoff(mut self, backoff: Backoff) -> Self {
        self.backoff = backoff;
        self
    }
    /// Set a deduplication id.
    pub fn with_dedup_id(mut self, dedup_id: impl Into<String>) -> Self {
        self.dedup_id = Some(dedup_id.into());
        self
    }
}

/// Options for [`Queue::dequeue`].
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct DequeueOpts {
    /// SQS long-poll `WaitTimeSeconds`, clamped to [`MAX_WAIT`].
    pub wait: Duration,
    /// SQS `VisibilityTimeout` — the lease duration. `0 < t <=` [`MAX_VISIBILITY_TIMEOUT`].
    pub visibility_timeout: Duration,
}

impl Default for DequeueOpts {
    fn default() -> Self {
        Self {
            wait: Duration::from_secs(20),
            visibility_timeout: Duration::from_secs(30),
        }
    }
}

impl DequeueOpts {
    /// Default options.
    pub fn new() -> Self {
        Self::default()
    }
    /// Set the long-poll wait.
    pub fn with_wait(mut self, wait: Duration) -> Self {
        self.wait = wait;
        self
    }
    /// Set the lease/visibility timeout.
    pub fn with_visibility_timeout(mut self, vt: Duration) -> Self {
        self.visibility_timeout = vt;
        self
    }
}

/// Options for [`Queue::nack`].
#[non_exhaustive]
#[derive(Debug, Clone, Default)]
pub struct NackOpts {
    /// `None` retries immediately; `Some(d)` makes the job available no earlier
    /// than `now + d`.
    pub retry_in: Option<Duration>,
}

impl NackOpts {
    /// Retry no earlier than `now + delay`.
    pub fn retry_in(delay: Duration) -> Self {
        Self {
            retry_in: Some(delay),
        }
    }
}

/// A leased unit of work returned by [`Queue::dequeue`].
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct Job {
    /// Stable id (also the natural idempotency key for consumers).
    pub id: JobId,
    /// Source queue.
    pub queue: String,
    /// Opaque payload.
    pub payload: Bytes,
    /// 1 on first delivery, N on the Nth.
    pub attempt: u32,
    /// Deliveries before dead-lettering.
    pub max_attempts: u32,
    /// Lease deadline; refresh with [`Queue::heartbeat`] before it passes.
    pub leased_until: SystemTime,
    /// Per-lease fence token. `ack`/`nack`/`heartbeat` only affect the row while
    /// it still carries this token; a redelivery mints a new one, so a stale
    /// worker's calls become no-ops / `Precondition`.
    pub(crate) lease_token: Uuid,
}

impl Job {
    /// Deserialize the payload from JSON. A decode failure is
    /// [`ForgeError::Invalid`] — the payload is caller data, not a backend error.
    pub fn payload_json<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_slice(&self.payload)
            .map_err(|e| ForgeError::invalid(format!("could not deserialize payload: {e}")))
    }
}

/// A background job queue: at-least-once delivery, visibility-timeout leasing,
/// `maxReceiveCount`-to-DLQ redrive. Mirrors AWS SQS.
///
/// Object-safe; the facade hands out `Arc<dyn Queue>`. Exact semantics live in
/// `docs/contracts/queue.md`.
#[async_trait]
pub trait Queue: Send + Sync {
    /// SQS `SendMessage`. Returns the assigned [`JobId`]. With `opts.dedup_id`,
    /// a hit within the window returns the existing id (success, not an error).
    async fn enqueue(&self, queue: &str, payload: Bytes, opts: EnqueueOpts) -> Result<JobId>;

    /// SQS `ReceiveMessage` (long-poll). Leases at most one due job for
    /// `opts.visibility_timeout`; `Ok(None)` if none arrived within `opts.wait`.
    /// Claiming does not increment attempts.
    async fn dequeue(&self, queue: &str, opts: DequeueOpts) -> Result<Option<Job>>;

    /// SQS `DeleteMessage` (`leased -> done`). Idempotent: acking a job whose
    /// lease already expired and was reclaimed is `Ok(())`, not an error.
    async fn ack(&self, job: &Job) -> Result<()>;

    /// Mark the current delivery failed (`leased -> available`, or to the DLQ if
    /// the incremented attempt count reaches `max_attempts`). The redelivery
    /// increments attempts.
    async fn nack(&self, job: &Job, opts: NackOpts) -> Result<()>;

    /// Extend the lease (beanstalkd `touch`). [`crate::ForgeError::Precondition`]
    /// if the lease was already lost to another worker — stop work on this job.
    async fn heartbeat(&self, job: &Job) -> Result<()>;
}

/// JSON convenience helper over [`Queue`]. Blanket-implemented, so it works on
/// `&dyn Queue` too. Pair it with [`Job::payload_json`] on the consume side.
#[async_trait]
pub trait QueueExt: Queue {
    /// `enqueue` a payload serialized to JSON.
    async fn enqueue_json<T: Serialize + Send + Sync>(
        &self,
        queue: &str,
        value: &T,
        opts: EnqueueOpts,
    ) -> Result<JobId> {
        let bytes = serde_json::to_vec(value)
            .map_err(|e| ForgeError::invalid(format!("could not serialize payload: {e}")))?;
        self.enqueue(queue, Bytes::from(bytes), opts).await
    }
}

impl<T: Queue + ?Sized> QueueExt for T {}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::indexing_slicing, clippy::panic)]
mod tests {
    use super::*;

    #[test]
    fn exponential_backoff_grows_and_caps() {
        let b = Backoff::Exponential {
            base: Duration::from_secs(1),
            cap: Duration::from_secs(300),
        };
        // Fixed seed so the ±25% jitter is deterministic.
        let d1 = b.delay_for_attempt(1, 7).as_millis();
        let d2 = b.delay_for_attempt(2, 7).as_millis();
        let d3 = b.delay_for_attempt(3, 7).as_millis();
        assert!((750..=1250).contains(&d1), "attempt1 ~1s, got {d1}ms");
        assert!((1500..=2500).contains(&d2), "attempt2 ~2s, got {d2}ms");
        assert!((3000..=5000).contains(&d3), "attempt3 ~4s, got {d3}ms");
        let far = b.delay_for_attempt(40, 7).as_millis();
        assert!(far <= 375_000, "capped at 300s + jitter, got {far}ms");
        assert!(far >= 225_000, "capped near 300s, got {far}ms");
    }

    #[test]
    fn high_attempt_does_not_panic_or_overflow() {
        let b = Backoff::Exponential {
            base: Duration::from_secs(1),
            cap: Duration::from_secs(300),
        };
        // u32::MAX once drove a shift overflow in naive impls.
        let _ = b.delay_for_attempt(u32::MAX, 1);
    }

    #[test]
    fn fixed_and_linear_apply_jitter() {
        let fixed = Backoff::Fixed(Duration::from_secs(2));
        let d = fixed.delay_for_attempt(5, 3).as_millis();
        assert!((1500..=2500).contains(&d), "fixed ~2s ±25%, got {d}ms");

        let linear = Backoff::Linear(Duration::from_secs(1));
        let d3 = linear.delay_for_attempt(3, 3).as_millis();
        assert!((2250..=3750).contains(&d3), "linear*3 ~3s ±25%, got {d3}ms");
    }

    #[test]
    fn jitter_varies_with_seed_but_is_deterministic() {
        let b = Backoff::Fixed(Duration::from_secs(10));
        let a = b.delay_for_attempt(1, 1);
        let a_again = b.delay_for_attempt(1, 1);
        let other = b.delay_for_attempt(1, 999);
        assert_eq!(a, a_again, "same seed => same delay");
        assert_ne!(a, other, "different seed => (very likely) different delay");
    }

    #[test]
    fn zero_base_stays_zero() {
        assert_eq!(
            Backoff::Fixed(Duration::ZERO).delay_for_attempt(3, 42),
            Duration::ZERO
        );
    }
}

//! `queue` — lineage: AWS SQS. See `docs/contracts/queue.md`.
//!
//! AT-LEAST-ONCE delivery: a job may be delivered more than once. Consumers
//! MUST be idempotent. Attempts increment on redelivery, never on claim.
//!
//! The contract (the [`Queue`] trait, [`Job`], the option/`Backoff` types) lives in
//! this module, which also wires the Postgres backend plus the managed [`worker`]
//! consumer.

use crate::error::{ForgeError, Result};
use async_trait::async_trait;
use bytes::Bytes;
use serde::de::DeserializeOwned;
use std::fmt;
use std::time::{Duration, SystemTime};
use uuid::Uuid;

/// Largest allowed payload (256 KiB — the SQS `SendMessage` ceiling, enforced
/// so a future SQS backend stays honest). Over => [`crate::error::ForgeError::Limit`].
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

/// Retry backoff strategy: exponential with jitter, capped.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Backoff {
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
        let Backoff::Exponential { base, cap } = self;
        // 2^(n-1), saturating past 63 shifts.
        let factor = 1u64.checked_shl(n - 1).unwrap_or(u64::MAX);
        let base_ms = duration_ms(*base)
            .saturating_mul(factor)
            .min(duration_ms(*cap));
        // Respect a global ceiling so a huge `cap` can't park a job for years
        // (the contract's "same cap rule").
        let base_ms = base_ms.min(duration_ms(MAX_BACKOFF));
        Duration::from_millis(jitter_ms(base_ms, seed))
    }
}

/// Global ceiling on any computed backoff delay (before jitter), matching the
/// queue's 12h visibility ceiling.
const MAX_BACKOFF: Duration = Duration::from_secs(12 * 60 * 60);

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
    /// SQS `MessageDeduplicationId`: dedups enqueues per `(queue, dedup_id)`
    /// within the dedup window. `None` disables dedup.
    pub dedup_id: Option<String>,
}

impl Default for EnqueueOpts {
    fn default() -> Self {
        Self {
            delay: Duration::ZERO,
            max_attempts: 5,
            dedup_id: None,
        }
    }
}

impl EnqueueOpts {
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

/// Approximate message counts for a queue, mirroring the SQS CloudWatch
/// `ApproximateNumberOfMessages*` metrics. All three are point-in-time estimates
/// taken without locking, so a concurrent enqueue/lease may not be reflected yet.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct QueueDepth {
    /// Available for immediate delivery (SQS `ApproximateNumberOfMessagesVisible`).
    /// Counts jobs whose lease has lapsed but not yet been reclaimed, since the next
    /// `dequeue` will hand them out.
    pub visible: u64,
    /// Leased and not past the visibility deadline (SQS `…MessagesNotVisible`).
    pub in_flight: u64,
    /// Enqueued with a delay and not yet due (SQS `…MessagesDelayed`).
    pub delayed: u64,
}

impl QueueDepth {
    /// Construct a depth snapshot. For backend implementors; app code receives this
    /// from [`Queue::depth`].
    pub fn new(visible: u64, in_flight: u64, delayed: u64) -> Self {
        Self {
            visible,
            in_flight,
            delayed,
        }
    }

    /// Total non-terminal messages: `visible + in_flight + delayed` (saturating).
    pub fn total(&self) -> u64 {
        self.visible
            .saturating_add(self.in_flight)
            .saturating_add(self.delayed)
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
    /// Construct a leased job. For backend implementors: a backend mints this from a
    /// claimed row, supplying the per-lease fence `lease_token` that `ack`/`nack`/
    /// `heartbeat` later check via [`Job::lease_token`]. App code never calls this — it
    /// receives `Job`s from [`Queue::dequeue`].
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        id: JobId,
        queue: String,
        payload: Bytes,
        attempt: u32,
        max_attempts: u32,
        leased_until: SystemTime,
        lease_token: Uuid,
    ) -> Self {
        Self {
            id,
            queue,
            payload,
            attempt,
            max_attempts,
            leased_until,
            lease_token,
        }
    }

    /// The per-lease fence token. For backend implementors: `ack`/`nack`/`heartbeat`
    /// must only mutate the row while it still carries this token, so a stale worker's
    /// calls become no-ops / `Precondition`.
    pub fn lease_token(&self) -> Uuid {
        self.lease_token
    }

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

    /// Extend the lease (beanstalkd `touch`). [`crate::error::ForgeError::Precondition`]
    /// if the lease was already lost to another worker — stop work on this job.
    async fn heartbeat(&self, job: &Job) -> Result<()>;

    /// SQS `GetQueueAttributes`: approximate visible / in-flight / delayed counts
    /// for `queue` ([`QueueDepth`]). Non-locking and point-in-time. Pass a
    /// `"<queue>.dlq"` name to gauge a dead-letter backlog without leasing its jobs.
    async fn depth(&self, queue: &str) -> Result<QueueDepth>;
}

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
    fn backoff_respects_the_global_ceiling() {
        // A huge cap at a high attempt would be ~1000h without the global ceiling.
        let b = Backoff::Exponential {
            base: Duration::from_secs(3600),
            cap: Duration::from_secs(1000 * 3600),
        };
        let d = b.delay_for_attempt(999, 7).as_millis();
        let ceiling_ms = (MAX_BACKOFF.as_millis() as f64 * 1.25) as u128;
        assert!(d <= ceiling_ms, "capped at 12h + jitter, got {d}ms");
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
    fn jitter_varies_with_seed_but_is_deterministic() {
        let b = Backoff::Exponential {
            base: Duration::from_secs(10),
            cap: Duration::from_secs(300),
        };
        let a = b.delay_for_attempt(1, 1);
        let a_again = b.delay_for_attempt(1, 1);
        let other = b.delay_for_attempt(1, 999);
        assert_eq!(a, a_again, "same seed => same delay");
        assert_ne!(a, other, "different seed => (very likely) different delay");
    }

    #[test]
    fn zero_base_stays_zero() {
        let b = Backoff::Exponential {
            base: Duration::ZERO,
            cap: Duration::from_secs(300),
        };
        assert_eq!(b.delay_for_attempt(3, 42), Duration::ZERO);
    }
}

pub mod worker;

mod memory;
mod postgres;
pub(crate) use memory::MemQueue;
pub(crate) use postgres::PgQueue;

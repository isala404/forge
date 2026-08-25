use crate::TraceContext;
use crate::error::{ForgeError, Result};
use async_trait::async_trait;
use bytes::Bytes;
use serde::de::DeserializeOwned;
use std::fmt;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};
use std::time::{Duration, SystemTime};
use tokio::sync::Notify;
use uuid::Uuid;

/// Largest allowed payload (256 KiB, the SQS `SendMessage` ceiling, enforced
/// so a future SQS backend stays honest). Over => [`crate::error::ForgeError::Limit`].
pub const MAX_PAYLOAD_BYTES: usize = 256 * 1024;

/// Longest a `dequeue` long-poll may wait (SQS max). Larger is clamped, not rejected.
pub const MAX_WAIT: Duration = Duration::from_secs(20);

/// Largest visibility timeout / lease (SQS max). Out of range => `Invalid`.
pub const MAX_VISIBILITY_TIMEOUT: Duration = Duration::from_secs(12 * 60 * 60);

/// Maximum number of dead letters returned or redriven by one operator call.
pub const MAX_OPERATOR_BATCH: u32 = 100;
/// Maximum jobs accepted by one batch enqueue call.
pub const MAX_ENQUEUE_BATCH: usize = 100;
/// Maximum leases returned by one batch dequeue call.
pub const MAX_DEQUEUE_BATCH: usize = 10;

/// Maximum retained operator-safe failure summary. Worker implementations use a
/// constant classification by default; applications may supply a redacted summary.
pub const MAX_FAILURE_SUMMARY_BYTES: usize = 512;

/// Largest concurrency key stored with a job.
pub const MAX_CONCURRENCY_KEY_BYTES: usize = 256;
/// Largest correlation id stored in a versioned envelope.
pub const MAX_CORRELATION_ID_BYTES: usize = 256;
/// Largest schema id stored in a versioned envelope.
pub const MAX_SCHEMA_BYTES: usize = 256;
/// Largest content type stored in a versioned envelope.
pub const MAX_CONTENT_TYPE_BYTES: usize = 128;
/// Maximum artifact references in one envelope.
pub const MAX_ARTIFACT_REFS: usize = 32;

#[derive(Debug, Clone, Copy)]
pub(crate) struct TerminalRetention {
    pub succeeded: Duration,
    pub dead: Duration,
    pub cancelled: Duration,
}

/// Deliberately small queue priority set. PostgreSQL and memory dequeue higher
/// priorities first and preserve enqueue order within one priority. Continuous
/// higher-priority traffic can starve lower priorities; callers that require a
/// stronger fairness guarantee should use separate queues and worker allocations.
#[derive(
    Debug,
    Clone,
    Copy,
    Default,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum Priority {
    Low,
    #[default]
    Normal,
    High,
}

impl Priority {
    pub(crate) const fn rank(self) -> i16 {
        match self {
            Self::Low => 0,
            Self::Normal => 1,
            Self::High => 2,
        }
    }

    pub(crate) const fn from_rank(value: i16) -> Self {
        match value {
            0 => Self::Low,
            2 => Self::High,
            _ => Self::Normal,
        }
    }
}

/// Public lifecycle state. `queued`, `delayed`, and `retrying` are views over the
/// backend's available state; the distinction is based on due time and attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum JobState {
    Queued,
    Delayed,
    Leased,
    Retrying,
    Succeeded,
    Dead,
    CancelRequested,
    Cancelled,
}

impl JobState {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Dead | Self::Cancelled)
    }
}

/// Payload-free job status for application and operator views.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobStatus {
    pub id: JobId,
    pub queue: String,
    pub state: JobState,
    pub attempt_count: u32,
    pub max_attempts: u32,
    pub priority: Priority,
    pub concurrency_key: Option<String>,
    pub enqueued_at: SystemTime,
    pub available_at: SystemTime,
    pub completed_at: Option<SystemTime>,
}

/// Bounded operator query. The cursor is opaque and scoped to the same queue/filter.
#[derive(Debug, Clone)]
pub struct JobStatusFilter {
    pub queue: Option<String>,
    pub states: Vec<JobState>,
    pub cursor: Option<crate::Cursor>,
    pub limit: u32,
}

impl Default for JobStatusFilter {
    fn default() -> Self {
        Self {
            queue: None,
            states: Vec::new(),
            cursor: None,
            limit: 50,
        }
    }
}

/// One bounded status page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JobStatusPage {
    pub items: Vec<JobStatus>,
    pub next_cursor: Option<crate::Cursor>,
}

/// Cooperative cancellation token carried by every leased job. Forge signals it,
/// but cannot terminate application code; handlers must observe it and return.
#[derive(Clone, Default)]
pub struct JobCancellation {
    cancelled: Arc<AtomicBool>,
    notify: Arc<Notify>,
}

impl fmt::Debug for JobCancellation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JobCancellation")
            .field("cancelled", &self.is_cancelled())
            .finish()
    }
}

impl JobCancellation {
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        if self.is_cancelled() {
            return;
        }
        let notified = self.notify.notified();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }

    pub(crate) fn signal(&self) {
        if !self.cancelled.swap(true, Ordering::AcqRel) {
            self.notify.notify_waiters();
        }
    }
}

/// A blob or external artifact referenced by a queue envelope.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ArtifactRef {
    pub uri: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub content_type: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub version: Option<String>,
}

/// Portable, versioned metadata envelope. Raw queue bytes remain fully supported;
/// this helper is opt-in and is not the backend storage format.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct QueueEnvelope {
    pub version: u16,
    pub schema: String,
    pub content_type: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub correlation_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub trace_context: Option<TraceContext>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub artifacts: Vec<ArtifactRef>,
    pub body: Vec<u8>,
}

impl QueueEnvelope {
    pub const VERSION: u16 = 1;

    pub fn new(
        schema: impl Into<String>,
        content_type: impl Into<String>,
        body: impl Into<Vec<u8>>,
    ) -> Self {
        Self {
            version: Self::VERSION,
            schema: schema.into(),
            content_type: content_type.into(),
            correlation_id: None,
            trace_context: None,
            artifacts: Vec::new(),
            body: body.into(),
        }
    }

    /// Validate metadata and the final encoded size together, then return enqueue-ready bytes.
    pub fn encode(&self) -> Result<Bytes> {
        self.validate()?;
        let bytes = serde_json::to_vec(self)
            .map_err(|e| ForgeError::invalid(format!("could not encode queue envelope: {e}")))?;
        if bytes.len() > MAX_PAYLOAD_BYTES {
            return Err(ForgeError::limit(format!(
                "encoded envelope is {} bytes; max is {MAX_PAYLOAD_BYTES}; store large bodies in blob storage and reference them",
                bytes.len()
            )));
        }
        Ok(Bytes::from(bytes))
    }

    pub fn decode(bytes: &[u8]) -> Result<Self> {
        let value: Self = serde_json::from_slice(bytes)
            .map_err(|e| ForgeError::invalid(format!("could not decode queue envelope: {e}")))?;
        value.validate()?;
        if bytes.len() > MAX_PAYLOAD_BYTES {
            return Err(ForgeError::limit(
                "encoded envelope exceeds the queue payload limit",
            ));
        }
        Ok(value)
    }

    pub fn validate(&self) -> Result<()> {
        if self.version != Self::VERSION {
            return Err(ForgeError::invalid(format!(
                "unsupported queue envelope version {}",
                self.version
            )));
        }
        check_metadata("schema", &self.schema, MAX_SCHEMA_BYTES)?;
        check_metadata("content_type", &self.content_type, MAX_CONTENT_TYPE_BYTES)?;
        if let Some(value) = &self.correlation_id {
            check_metadata("correlation_id", value, MAX_CORRELATION_ID_BYTES)?;
        }
        if let Some(value) = &self.trace_context {
            value.validate()?;
        }
        if self.artifacts.len() > MAX_ARTIFACT_REFS {
            return Err(ForgeError::limit(format!(
                "envelope has {} artifact references; max is {MAX_ARTIFACT_REFS}",
                self.artifacts.len()
            )));
        }
        for artifact in &self.artifacts {
            check_metadata("artifact uri", &artifact.uri, 2048)?;
            if let Some(value) = &artifact.content_type {
                check_metadata("artifact content_type", value, MAX_CONTENT_TYPE_BYTES)?;
            }
            if let Some(value) = &artifact.version {
                check_metadata("artifact version", value, 256)?;
            }
        }
        Ok(())
    }
}

fn check_metadata(name: &str, value: &str, max: usize) -> Result<()> {
    if value.is_empty() {
        return Err(ForgeError::invalid(format!("{name} must not be empty")));
    }
    if value.len() > max {
        return Err(ForgeError::limit(format!(
            "{name} is {} bytes; max is {max}",
            value.len()
        )));
    }
    Ok(())
}

/// Opaque job identifier (a UUID under the hood).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize, serde::Deserialize)]
pub struct JobId(pub Uuid);

impl JobId {
    pub fn new() -> Self {
        Self(Uuid::new_v4())
    }

    pub fn parse(value: &str) -> Result<Self> {
        Uuid::parse_str(value)
            .map(Self)
            .map_err(|_| ForgeError::invalid("job id must be a UUID"))
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
    /// Saturating throughout: no panic at high attempt counts.
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
    /// Caller-selected id for systems layered on top of queue, such as `schedule`.
    /// Normal app enqueues leave this unset and let the backend mint an id. Backends
    /// must treat a repeated same-queue id as idempotent success, including when a
    /// fresh `dedup_id` is also supplied.
    pub job_id: Option<JobId>,
    /// Reserved W3C propagation metadata. It is stored separately from the payload
    /// and baggage has already passed an explicit allow-list.
    pub trace_context: Option<TraceContext>,
    /// Bounded priority. Strict priority is used; sustained higher priority can starve lower.
    pub priority: Priority,
    /// Optional application key used with `DequeueOpts::concurrency_limit_per_key`.
    pub concurrency_key: Option<String>,
}

impl Default for EnqueueOpts {
    fn default() -> Self {
        Self {
            delay: Duration::ZERO,
            max_attempts: 5,
            dedup_id: None,
            job_id: None,
            trace_context: None,
            priority: Priority::Normal,
            concurrency_key: None,
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
    /// Set the job id the backend should assign. Intended for Forge primitives that
    /// need stable correlation across retries; callers usually let queue mint ids.
    pub fn with_job_id(mut self, job_id: JobId) -> Self {
        self.job_id = Some(job_id);
        self
    }

    pub fn with_trace_context(mut self, trace_context: TraceContext) -> Self {
        self.trace_context = Some(trace_context);
        self
    }

    pub fn with_priority(mut self, priority: Priority) -> Self {
        self.priority = priority;
        self
    }

    pub fn with_concurrency_key(mut self, key: impl Into<String>) -> Self {
        self.concurrency_key = Some(key.into());
        self
    }
}

/// One independent item in a bounded batch enqueue.
#[derive(Debug, Clone)]
pub struct BatchEnqueueItem {
    pub payload: Bytes,
    pub opts: EnqueueOpts,
}

impl BatchEnqueueItem {
    pub fn new(payload: impl Into<Bytes>, opts: EnqueueOpts) -> Self {
        Self {
            payload: payload.into(),
            opts,
        }
    }
}

/// Per-item batch result. A failed item does not roll back successful siblings.
#[derive(Debug)]
pub struct BatchEnqueueResult {
    pub job_id: Option<JobId>,
    pub error: Option<ForgeError>,
}

impl BatchEnqueueResult {
    fn from_result(result: Result<JobId>) -> Self {
        match result {
            Ok(job_id) => Self {
                job_id: Some(job_id),
                error: None,
            },
            Err(error) => Self {
                job_id: None,
                error: Some(error),
            },
        }
    }
}

/// Options for [`Queue::dequeue`].
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct DequeueOpts {
    /// SQS long-poll `WaitTimeSeconds`, clamped to [`MAX_WAIT`].
    pub wait: Duration,
    /// SQS `VisibilityTimeout`: the lease duration. `0 < t <=` [`MAX_VISIBILITY_TIMEOUT`].
    pub visibility_timeout: Duration,
    /// Maximum simultaneously leased jobs with the same non-empty concurrency key.
    /// `None` disables key fairness. A job without a key is never constrained.
    pub concurrency_limit_per_key: Option<u32>,
}

impl Default for DequeueOpts {
    fn default() -> Self {
        Self {
            wait: Duration::from_secs(20),
            visibility_timeout: Duration::from_secs(30),
            concurrency_limit_per_key: None,
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

    pub fn with_concurrency_limit_per_key(mut self, limit: u32) -> Self {
        self.concurrency_limit_per_key = Some(limit);
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
    /// Redacted terminal diagnostic. It is truncated to 512 UTF-8 bytes before
    /// persistence and must never contain payloads, credentials, or raw backend errors.
    pub failure_summary: Option<String>,
}

impl NackOpts {
    /// Retry no earlier than `now + delay`.
    pub fn retry_in(delay: Duration) -> Self {
        Self {
            retry_in: Some(delay),
            failure_summary: None,
        }
    }

    /// Attach a bounded, caller-redacted diagnostic for eventual DLQ inspection.
    pub fn with_failure_summary(mut self, summary: impl Into<String>) -> Self {
        self.failure_summary = Some(safe_failure_summary(summary.into()));
        self
    }
}

/// How a redrive treats a prior deduplication reservation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RedriveDedupPolicy {
    /// Drop any reservation pointing at the job so the redrive is independent.
    Clear,
    /// Keep a live reservation pointing at the stable job id.
    Preserve,
}

/// Required redrive choices. A destination is never inferred from the DLQ name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedriveOpts {
    pub destination: String,
    pub dedup_policy: RedriveDedupPolicy,
}

impl RedriveOpts {
    pub fn new(destination: impl Into<String>, dedup_policy: RedriveDedupPolicy) -> Self {
        Self {
            destination: destination.into(),
            dedup_policy,
        }
    }
}

/// One payload-free dead-letter record for operator tooling.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadLetterInfo {
    pub job_id: JobId,
    pub queue: String,
    pub attempt_count: u32,
    pub enqueued_at: SystemTime,
    pub dead_lettered_at: SystemTime,
    pub failure_summary: Option<String>,
}

/// Bounded page of dead letters. `next_cursor` is opaque and valid only for the
/// same logical queue and namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeadLetterPage {
    pub items: Vec<DeadLetterInfo>,
    pub next_cursor: Option<crate::Cursor>,
}

/// Result of a bounded batch redrive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedriveBatchResult {
    pub redriven: u32,
    pub next_cursor: Option<crate::Cursor>,
}

pub(crate) fn safe_failure_summary(mut value: String) -> String {
    if value.len() <= MAX_FAILURE_SUMMARY_BYTES {
        return value;
    }
    let mut end = MAX_FAILURE_SUMMARY_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    value.truncate(end);
    value
}

/// Approximate message counts for a queue, mirroring the SQS CloudWatch
/// `ApproximateNumberOfMessages*` metrics. All three are point-in-time estimates
/// taken without locking, so a concurrent enqueue/lease may not be reflected yet.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, Default)]
pub struct QueueDepth {
    /// Available for immediate delivery (SQS `ApproximateNumberOfMessagesVisible`).
    /// Counts jobs whose lease has lapsed but not yet been reclaimed, since the next
    /// `dequeue` will hand them out.
    pub visible: u64,
    /// Leased and not past the visibility deadline (SQS `…MessagesNotVisible`).
    pub in_flight: u64,
    /// Enqueued with a delay and not yet due (SQS `…MessagesDelayed`).
    pub delayed: u64,
    /// Milliseconds since the oldest immediately visible job was enqueued.
    /// `None` when the queue has no visible jobs.
    pub oldest_visible_age_ms: Option<u64>,
}

// Depth equality intentionally compares counts only. The age is sampled from a
// moving clock and is diagnostic metadata, so including it would make two otherwise
// identical point-in-time queue states compare unequal a millisecond apart.
impl PartialEq for QueueDepth {
    fn eq(&self, other: &Self) -> bool {
        self.visible == other.visible
            && self.in_flight == other.in_flight
            && self.delayed == other.delayed
    }
}

impl Eq for QueueDepth {}

impl QueueDepth {
    /// Construct a depth snapshot. For backend implementors; app code receives this
    /// from [`Queue::depth`].
    pub fn new(visible: u64, in_flight: u64, delayed: u64) -> Self {
        Self {
            visible,
            in_flight,
            delayed,
            oldest_visible_age_ms: None,
        }
    }

    pub fn with_oldest_visible_age_ms(mut self, age: Option<u64>) -> Self {
        self.oldest_visible_age_ms = age;
        self
    }

    /// Total non-terminal messages: `visible + in_flight + delayed` (saturating).
    pub fn total(&self) -> u64 {
        self.visible
            .saturating_add(self.in_flight)
            .saturating_add(self.delayed)
    }
}

/// Read-only queue activity estimates. Monotonic counters and lifetime-average
/// rates avoid scanning retained job rows.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct QueueStats {
    pub enqueued_total: u64,
    pub settled_total: u64,
    pub dead_total: u64,
    pub cancelled_total: u64,
    pub enqueue_rate_per_minute: f64,
    pub settle_rate_per_minute: f64,
    pub oldest_visible_age_ms: Option<u64>,
    pub paused: bool,
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
    /// Reserved cross-process W3C propagation metadata, never mixed into payload bytes.
    pub trace_context: Option<TraceContext>,
    /// Cooperative application-cancellation signal.
    pub cancellation: JobCancellation,
    pub priority: Priority,
    pub concurrency_key: Option<String>,
}

impl Job {
    /// Construct a leased job. For backend implementors: a backend mints this from a
    /// claimed row, supplying the per-lease fence `lease_token` that `ack`/`nack`/
    /// `heartbeat` later check via [`Job::lease_token`]. App code never calls this; it
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
            trace_context: None,
            cancellation: JobCancellation::default(),
            priority: Priority::Normal,
            concurrency_key: None,
        }
    }

    /// Attach stored propagation metadata when a backend materializes the lease.
    pub fn with_trace_context(mut self, trace_context: Option<TraceContext>) -> Self {
        self.trace_context = trace_context;
        self
    }

    pub fn with_scheduling(mut self, priority: Priority, concurrency_key: Option<String>) -> Self {
        self.priority = priority;
        self.concurrency_key = concurrency_key;
        self
    }

    /// The per-lease fence token. For backend implementors: `ack`/`nack`/`heartbeat`
    /// must only mutate the row while it still carries this token, so a stale worker's
    /// calls become no-ops / `Precondition`.
    pub fn lease_token(&self) -> Uuid {
        self.lease_token
    }

    /// Deserialize the payload from JSON. A decode failure is
    /// [`ForgeError::Invalid`]; the payload is caller data, not a backend error.
    pub fn payload_json<T: DeserializeOwned>(&self) -> Result<T> {
        serde_json::from_slice(&self.payload)
            .map_err(|e| ForgeError::invalid(format!("could not deserialize payload: {e}")))
    }
}

/// A background job queue: at-least-once delivery, visibility-timeout leasing,
/// `maxReceiveCount`-to-DLQ redrive. Mirrors AWS SQS.
///
/// Object-safe; the facade hands out `Arc<dyn Queue>`. Exact semantics live in
/// <https://tryforge.dev/primitives/#queue>.
#[async_trait]
pub trait Queue: Send + Sync {
    /// SQS `SendMessage`. Returns the assigned [`JobId`]. With `opts.dedup_id`,
    /// a hit within the window returns the existing id (success, not an error).
    async fn enqueue(&self, queue: &str, payload: Bytes, opts: EnqueueOpts) -> Result<JobId>;

    /// Enqueue at most 100 independent items. Results preserve input order and
    /// deterministic IDs; one invalid item does not roll back its siblings.
    async fn enqueue_batch(
        &self,
        queue: &str,
        items: Vec<BatchEnqueueItem>,
    ) -> Result<Vec<BatchEnqueueResult>> {
        if items.is_empty() || items.len() > MAX_ENQUEUE_BATCH {
            return Err(ForgeError::limit(format!(
                "batch enqueue size must be in 1..={MAX_ENQUEUE_BATCH}"
            )));
        }
        let mut results = Vec::with_capacity(items.len());
        for item in items {
            results.push(BatchEnqueueResult::from_result(
                self.enqueue(queue, item.payload, item.opts).await,
            ));
        }
        Ok(results)
    }

    /// SQS `ReceiveMessage` (long-poll). Leases at most one due job for
    /// `opts.visibility_timeout`; `Ok(None)` if none arrived within `opts.wait`.
    /// Claiming does not increment attempts.
    async fn dequeue(&self, queue: &str, opts: DequeueOpts) -> Result<Option<Job>>;

    /// Lease up to 10 jobs. Only the first receive may long-poll; remaining claims
    /// are immediate, so shutdown and lease settlement stay identical to single dequeue.
    async fn dequeue_batch(
        &self,
        queue: &str,
        max_items: usize,
        opts: DequeueOpts,
    ) -> Result<Vec<Job>> {
        if max_items == 0 || max_items > MAX_DEQUEUE_BATCH {
            return Err(ForgeError::limit(format!(
                "batch dequeue size must be in 1..={MAX_DEQUEUE_BATCH}"
            )));
        }
        let mut jobs = Vec::with_capacity(max_items);
        let mut next = opts;
        for _ in 0..max_items {
            let Some(job) = self.dequeue(queue, next.clone()).await? else {
                break;
            };
            jobs.push(job);
            next.wait = Duration::ZERO;
        }
        Ok(jobs)
    }

    /// SQS `DeleteMessage` (`leased -> done`). Idempotent: acking a job whose
    /// lease already expired and was reclaimed is `Ok(())`, not an error.
    async fn ack(&self, job: &Job) -> Result<()>;

    /// Mark the current delivery failed (`leased -> available`, or to the DLQ if
    /// the incremented attempt count reaches `max_attempts`). The redelivery
    /// increments attempts.
    async fn nack(&self, job: &Job, opts: NackOpts) -> Result<()>;

    /// Extend the lease (beanstalkd `touch`). [`crate::error::ForgeError::Precondition`]
    /// if the lease was already lost to another worker; stop work on this job.
    async fn heartbeat(&self, job: &Job) -> Result<()>;

    /// Low-cost cancellation poll used by managed workers. A true result also signals
    /// `job.cancellation`. It does not terminate the handler.
    async fn cancellation_requested(&self, job: &Job) -> Result<bool> {
        let _ = job;
        Ok(false)
    }

    /// Request cancellation. Queued/delayed/retrying jobs become cancelled atomically;
    /// leased jobs become cancel-requested until their handler cooperates or lease expires.
    async fn cancel(&self, id: JobId) -> Result<Option<JobStatus>> {
        let _ = id;
        Err(ForgeError::not_configured(
            "queue backend does not implement cancellation",
        ))
    }

    /// Finish a cooperative cancellation while the caller still owns the lease fence.
    async fn finish_cancellation(&self, job: &Job) -> Result<()> {
        let _ = job;
        Err(ForgeError::not_configured(
            "queue backend does not implement cancellation",
        ))
    }

    async fn status(&self, id: JobId) -> Result<Option<JobStatus>> {
        let _ = id;
        Err(ForgeError::not_configured(
            "queue backend does not implement status lookup",
        ))
    }

    async fn list_status(&self, filter: JobStatusFilter) -> Result<JobStatusPage> {
        let _ = filter;
        Err(ForgeError::not_configured(
            "queue backend does not implement status listing",
        ))
    }

    /// SQS `GetQueueAttributes`: approximate visible / in-flight / delayed counts
    /// for `queue` ([`QueueDepth`]). Non-locking and point-in-time. Pass a
    /// `"<queue>.dlq"` name to gauge a dead-letter backlog without leasing its jobs.
    async fn depth(&self, queue: &str) -> Result<QueueDepth>;

    /// Stop new leases from a queue. Existing leases continue and queued work is retained.
    async fn pause(&self, queue: &str) -> Result<()> {
        let _ = queue;
        Err(ForgeError::not_configured(
            "queue backend does not implement pause",
        ))
    }

    /// Resume leasing from a paused queue. Idempotent.
    async fn resume(&self, queue: &str) -> Result<()> {
        let _ = queue;
        Err(ForgeError::not_configured(
            "queue backend does not implement resume",
        ))
    }

    async fn is_paused(&self, queue: &str) -> Result<bool> {
        let _ = queue;
        Err(ForgeError::not_configured(
            "queue backend does not implement pause inspection",
        ))
    }

    /// Indexed age plus counter-based throughput estimates; never scans payload rows.
    async fn stats(&self, queue: &str) -> Result<QueueStats> {
        let _ = queue;
        Err(ForgeError::not_configured(
            "queue backend does not implement activity stats",
        ))
    }

    /// Inspect payload-free dead-letter metadata for `queue` (the source queue,
    /// without the reserved `.dlq` suffix).
    async fn dead_letters(
        &self,
        queue: &str,
        cursor: Option<crate::Cursor>,
        limit: u32,
    ) -> Result<DeadLetterPage> {
        let _ = (queue, cursor, limit);
        Err(ForgeError::not_configured(
            "queue backend does not implement dead-letter inspection",
        ))
    }

    /// Move one dead letter to an explicit destination. Returns false when the id
    /// is absent or no longer in a dead-letter state.
    async fn redrive(&self, job_id: JobId, opts: RedriveOpts) -> Result<bool> {
        let _ = (job_id, opts);
        Err(ForgeError::not_configured(
            "queue backend does not implement dead-letter redrive",
        ))
    }

    /// Redrive at most 100 items from one source queue.
    async fn redrive_batch(
        &self,
        queue: &str,
        cursor: Option<crate::Cursor>,
        limit: u32,
        opts: RedriveOpts,
    ) -> Result<RedriveBatchResult> {
        let _ = (queue, cursor, limit, opts);
        Err(ForgeError::not_configured(
            "queue backend does not implement batch redrive",
        ))
    }

    /// Count rows an exact-queue purge would remove. This has no side effects.
    async fn purge_dead_letters_dry_run(&self, queue: &str) -> Result<u64> {
        let _ = queue;
        Err(ForgeError::not_configured(
            "queue backend does not implement dead-letter purge",
        ))
    }

    /// Remove dead letters only when `confirmation` exactly equals `queue`.
    async fn purge_dead_letters(&self, queue: &str, confirmation: &str) -> Result<u64> {
        let _ = (queue, confirmation);
        Err(ForgeError::not_configured(
            "queue backend does not implement dead-letter purge",
        ))
    }
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

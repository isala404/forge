//! In-process `queue` backend. Contract: docs/contracts/queue.md.
//!
//! A single `Mutex`-guarded map of jobs keyed by id, mirroring the `forge_jobs`
//! `available -> leased -> done` state machine the Postgres backend runs, plus a
//! dedup map keyed by `(<namespace>:<queue>, dedup_id)`. The same physical queue
//! name (`crate::util::namespaced`) is used, so namespacing is identical. Leasing,
//! attempt counting, default-backoff redelivery, and `.dlq` redrive match
//! [`super::PgQueue`]; only the storage differs: no SQL, nothing survives a restart.
//!
//! Redelivery timing follows the same rule as Postgres: an explicit `nack` applies
//! [`Backoff::default`] (with per-job jitter), while a lease-expiry reclaim makes the
//! job available immediately, since a lapsed lease means a worker likely crashed and
//! prompt retry beats delay.

use super::{
    Backoff, DequeueOpts, EnqueueOpts, Job, JobId, MAX_PAYLOAD_BYTES, MAX_VISIBILITY_TIMEOUT,
    MAX_WAIT, NackOpts, Queue, QueueDepth,
};
use crate::backend::{BackendLifecycle, Primitive};
use crate::error::{ForgeError, Result};
use async_trait::async_trait;
use bytes::Bytes;
use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};
use uuid::Uuid;

/// Longest a `dedup_id` may be (SQS limit). Over => `Limit`. Mirrors the Postgres backend.
const MAX_DEDUP_ID_LEN: usize = 128;
/// Queue name length cap (matches the schedule name cap). Mirrors the Postgres backend.
const MAX_QUEUE_NAME_BYTES: usize = 256;
/// SQS `DelaySeconds` ceiling (15 min). Out of range => `Invalid`.
const MAX_DELAY: Duration = Duration::from_secs(15 * 60);
/// SQS `maxReceiveCount` ceiling.
const MAX_MAX_ATTEMPTS: u32 = 1000;
/// Poll cadence while long-polling a `dequeue`, matching the Postgres backend's loop.
const POLL_INTERVAL: Duration = Duration::from_millis(200);

/// Lifecycle state of an in-memory job, mirroring the `forge_jobs.status` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Status {
    /// Visible for delivery once `available_at` passes.
    Available,
    /// Leased to a worker until `leased_until`.
    Leased,
    /// Acked. Retained until `maintain` purges it past the retention window.
    Done,
    /// Exhausted while already in a `.dlq`: terminal, never re-homed again.
    Dead,
}

/// One job row. The id is the map key, so it lives outside the struct.
struct JobRow {
    /// Physical (namespaced) queue name; may end in `.dlq` after redrive.
    queue: String,
    payload: Bytes,
    status: Status,
    /// Count of FAILED deliveries; the next delivery is `attempts + 1`.
    attempts: u32,
    max_attempts: u32,
    /// When an `Available` job becomes visible.
    available_at: Instant,
    /// Lease deadline for a `Leased` job.
    leased_until: Option<Instant>,
    /// Per-lease fence token; `ack`/`nack`/`heartbeat` only act while it matches.
    lease_token: Option<Uuid>,
    /// The lease duration, so `heartbeat` can re-extend by the original timeout.
    lease_dur: Option<Duration>,
    /// When a `Done` job was acked, for the retention sweep.
    completed_at: Option<Instant>,
}

impl JobRow {
    fn available(queue: String, payload: Bytes, max_attempts: u32, available_at: Instant) -> Self {
        Self {
            queue,
            payload,
            status: Status::Available,
            attempts: 0,
            max_attempts,
            available_at,
            leased_until: None,
            lease_token: None,
            lease_dur: None,
            completed_at: None,
        }
    }

    fn clear_lease(&mut self) {
        self.lease_token = None;
        self.leased_until = None;
        self.lease_dur = None;
    }
}

/// One dedup slot: the job a `(queue, dedup_id)` resolved to and when the slot lapses.
struct DedupEntry {
    job_id: Uuid,
    expires_at: Instant,
}

/// All mutable state behind one lock, so a single critical section keeps the jobs and
/// their dedup slots consistent without any lock-ordering concern.
struct State {
    jobs: HashMap<Uuid, JobRow>,
    dedup: HashMap<(String, String), DedupEntry>,
}

/// In-process [`Queue`]. Not durable: state lives in this process only.
pub(crate) struct MemQueue {
    state: Mutex<State>,
    dedup_window: Duration,
    retention: Duration,
    /// Namespace prefix on queue names (`<ns>:<queue>`). Empty = no prefix.
    namespace: String,
}

impl MemQueue {
    pub(crate) fn new(dedup_window: Duration, retention: Duration, namespace: String) -> Self {
        Self {
            state: Mutex::new(State {
                jobs: HashMap::new(),
                dedup: HashMap::new(),
            }),
            dedup_window,
            retention,
            namespace,
        }
    }

    /// Take the lock, recovering the guard if a previous holder panicked. Critical
    /// sections are short and synchronous (no `await` held across the lock), so a
    /// poisoned lock never reflects a half-updated invariant worth aborting for.
    fn lock(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Stored queue name for a caller name, applying the namespace prefix.
    fn physical(&self, queue: &str) -> String {
        crate::util::namespaced(&self.namespace, queue)
    }

    /// Strip the namespace prefix from a stored queue name (for the returned `Job`).
    fn logical(&self, stored: &str) -> String {
        if self.namespace.is_empty() {
            stored.to_string()
        } else {
            stored
                .strip_prefix(&self.namespace)
                .and_then(|s| s.strip_prefix(':'))
                .unwrap_or(stored)
                .to_string()
        }
    }

    /// Reclaim expired leases: exhausted non-DLQ jobs re-home to `.dlq` (attempts reset),
    /// exhausted DLQ jobs become terminal `dead`, and the rest return to `available`
    /// immediately with attempts bumped. With `queue: Some`, scoped to that physical queue;
    /// `None` sweeps every queue. Mirrors [`super::PgQueue::reclaim`].
    fn reclaim_locked(jobs: &mut HashMap<Uuid, JobRow>, now: Instant, queue: Option<&str>) {
        for row in jobs.values_mut() {
            if queue.is_some_and(|q| row.queue != q) {
                continue;
            }
            if row.status != Status::Leased || row.leased_until.is_none_or(|d| d > now) {
                continue;
            }
            let new_attempts = row.attempts.saturating_add(1);
            if new_attempts >= row.max_attempts {
                if row.queue.ends_with(".dlq") {
                    row.status = Status::Dead;
                    row.attempts = new_attempts;
                    row.clear_lease();
                    continue;
                } else {
                    row.queue.push_str(".dlq");
                    row.attempts = 0;
                }
            } else {
                row.attempts = new_attempts;
            }
            row.status = Status::Available;
            row.available_at = now;
            row.clear_lease();
        }
    }

    /// Claim and lease the oldest due job in `pq`, or `None` if none are ready.
    fn try_claim_locked(
        &self,
        jobs: &mut HashMap<Uuid, JobRow>,
        pq: &str,
        vt: Duration,
    ) -> Option<Job> {
        let now = Instant::now();
        let id = *jobs
            .iter()
            .filter(|(_, r)| {
                r.queue == pq && r.status == Status::Available && r.available_at <= now
            })
            .min_by_key(|(_, r)| r.available_at)
            .map(|(id, _)| id)?;

        let token = Uuid::new_v4();
        // The Job carries a wall-clock deadline; internal lease timing stays monotonic.
        let leased_until = SystemTime::now() + vt;
        let row = jobs.get_mut(&id)?;
        row.status = Status::Leased;
        row.leased_until = Some(now + vt);
        row.lease_token = Some(token);
        row.lease_dur = Some(vt);
        // attempts counts FAILED deliveries; this delivery is attempts + 1.
        let attempt = row.attempts.saturating_add(1);
        Some(Job::new(
            JobId(id),
            self.logical(&row.queue),
            row.payload.clone(),
            attempt,
            row.max_attempts,
            leased_until,
            token,
        ))
    }

    /// Idempotent maintenance: purge old `done` jobs, reclaim expired leases across all
    /// queues, drop stale dedup slots. Mirrors [`super::PgQueue::maintenance`].
    pub(crate) fn maintain_sweep(&self) {
        let now = Instant::now();
        let retention = self.retention;
        let mut state = self.lock();
        state.jobs.retain(|_, r| {
            !(r.status == Status::Done
                && r.completed_at
                    .is_some_and(|c| now.saturating_duration_since(c) >= retention))
        });
        Self::reclaim_locked(&mut state.jobs, now, None);
        state.dedup.retain(|_, e| e.expires_at > now);
    }
}

#[async_trait]
impl Queue for MemQueue {
    async fn enqueue(&self, queue: &str, payload: Bytes, opts: EnqueueOpts) -> Result<JobId> {
        check_queue_name(queue, false)?;
        check_payload(&payload)?;
        check_enqueue_opts(&opts)?;

        let now = Instant::now();
        let available_at = now + opts.delay;
        let pq = self.physical(queue);
        let mut state = self.lock();

        let requested_id = opts.job_id.map(|id| id.0);
        let id = if let Some(dedup_id) = opts.dedup_id {
            let key = (pq.clone(), dedup_id);
            // A live slot returns its existing job id (success, not an error). An expired
            // slot is overwritten and a fresh job created, matching the Postgres upsert.
            if let Some(entry) = state.dedup.get(&key)
                && entry.expires_at > now
            {
                return Ok(JobId(entry.job_id));
            }
            let id = requested_id.unwrap_or_else(Uuid::new_v4);
            if let Some(existing) = state.jobs.get(&id) {
                if existing.queue == pq {
                    state.dedup.insert(
                        key,
                        DedupEntry {
                            job_id: id,
                            expires_at: now + self.dedup_window,
                        },
                    );
                    return Ok(JobId(id));
                }
                return Err(ForgeError::precondition(
                    "requested job id already exists for another queue",
                ));
            }
            state.dedup.insert(
                key,
                DedupEntry {
                    job_id: id,
                    expires_at: now + self.dedup_window,
                },
            );
            id
        } else {
            requested_id.unwrap_or_else(Uuid::new_v4)
        };

        if let Some(existing) = state.jobs.get(&id) {
            if existing.queue == pq {
                return Ok(JobId(id));
            }
            return Err(ForgeError::precondition(
                "requested job id already exists for another queue",
            ));
        }
        state.jobs.insert(
            id,
            JobRow::available(pq, payload, opts.max_attempts, available_at),
        );
        Ok(JobId(id))
    }

    async fn dequeue(&self, queue: &str, opts: DequeueOpts) -> Result<Option<Job>> {
        check_queue_name(queue, true)?;
        check_visibility(opts.visibility_timeout)?;
        let pq = self.physical(queue);
        let vt = opts.visibility_timeout;
        let wait = opts.wait.min(MAX_WAIT);

        // Reclaim expired leases once up front so crashed work redelivers.
        {
            let mut state = self.lock();
            Self::reclaim_locked(&mut state.jobs, Instant::now(), Some(&pq));
        }

        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let claimed = {
                let mut state = self.lock();
                self.try_claim_locked(&mut state.jobs, &pq, vt)
            };
            if let Some(job) = claimed {
                return Ok(Some(job));
            }
            let now = tokio::time::Instant::now();
            if now >= deadline {
                return Ok(None);
            }
            let sleep = POLL_INTERVAL.min(deadline - now);
            tokio::time::sleep(sleep).await;
        }
    }

    async fn ack(&self, job: &Job) -> Result<()> {
        let mut state = self.lock();
        // Idempotent: a lost/reclaimed lease (token cleared or status changed) is a no-op.
        if let Some(row) = state.jobs.get_mut(&job.id.0)
            && row.status == Status::Leased
            && row.lease_token == Some(job.lease_token())
        {
            row.status = Status::Done;
            row.completed_at = Some(Instant::now());
            row.clear_lease();
        }
        Ok(())
    }

    async fn nack(&self, job: &Job, opts: NackOpts) -> Result<()> {
        if opts.retry_in.is_some_and(|d| d > MAX_VISIBILITY_TIMEOUT) {
            return Err(ForgeError::invalid(format!(
                "retry_in exceeds the maximum of {}s",
                MAX_VISIBILITY_TIMEOUT.as_secs()
            )));
        }
        let id = job.id.0;
        let token = job.lease_token();
        // A job already in a `.dlq` queue must not re-home into an unwatched `.dlq.dlq`;
        // exhaustion there is terminal.
        let in_dlq = job.queue.ends_with(".dlq");
        let now = Instant::now();
        let mut state = self.lock();

        let Some(row) = state.jobs.get_mut(&id) else {
            return Err(ForgeError::NotFound);
        };
        if row.status != Status::Leased || row.lease_token != Some(token) {
            return Err(ForgeError::precondition(
                "lease lost: another worker owns this job",
            ));
        }

        let new_attempts = row.attempts.saturating_add(1);
        if new_attempts >= row.max_attempts {
            if in_dlq {
                // Terminal: park as 'dead' with attempts pinned; nothing re-homes it.
                row.status = Status::Dead;
                row.attempts = new_attempts;
                row.clear_lease();
            } else {
                // Exhausted: re-home to DLQ with attempts reset so DLQ consumers see a clean slate.
                row.queue.push_str(".dlq");
                row.status = Status::Available;
                row.attempts = 0;
                row.available_at = now;
                row.clear_lease();
            }
            return Ok(());
        }

        // Explicit retry timing wins; otherwise the default backoff curve (with per-job jitter).
        let delay = opts.retry_in.unwrap_or_else(|| {
            Backoff::default().delay_for_attempt(new_attempts, seed_from_id(id))
        });
        row.status = Status::Available;
        row.attempts = new_attempts;
        row.available_at = now + delay;
        row.clear_lease();
        Ok(())
    }

    async fn heartbeat(&self, job: &Job) -> Result<()> {
        let id = job.id.0;
        let token = job.lease_token();
        let now = Instant::now();
        let mut state = self.lock();

        let Some(row) = state.jobs.get_mut(&id) else {
            return Err(ForgeError::NotFound);
        };
        if row.status != Status::Leased || row.lease_token != Some(token) {
            return Err(ForgeError::precondition(
                "lease lost: another worker owns this job",
            ));
        }
        if let Some(d) = row.lease_dur {
            row.leased_until = Some(now + d);
        }
        Ok(())
    }

    async fn depth(&self, queue: &str) -> Result<QueueDepth> {
        // Allow `.dlq` names: gauging a dead-letter backlog is a primary use.
        check_queue_name(queue, true)?;
        let pq = self.physical(queue);
        let now = Instant::now();
        let state = self.lock();

        let (mut visible, mut in_flight, mut delayed) = (0u64, 0u64, 0u64);
        for row in state.jobs.values() {
            if row.queue != pq {
                continue;
            }
            match row.status {
                Status::Available if row.available_at <= now => visible += 1,
                Status::Available => delayed += 1,
                // An expired-but-unreclaimed lease counts as visible: the next dequeue hands it out.
                Status::Leased if row.leased_until.is_some_and(|d| d <= now) => visible += 1,
                Status::Leased => in_flight += 1,
                Status::Done | Status::Dead => {}
            }
        }
        Ok(QueueDepth::new(visible, in_flight, delayed))
    }
}

#[async_trait]
impl BackendLifecycle for MemQueue {
    fn name(&self) -> &'static str {
        "memory"
    }
    fn primitive(&self) -> Primitive {
        Primitive::Queue
    }
    fn durable(&self) -> bool {
        false
    }
    fn caveats(&self) -> &'static str {
        "in-process, not durable"
    }
    async fn maintain(&self) -> Result<()> {
        self.maintain_sweep();
        Ok(())
    }
}

/// Derive a stable per-job jitter seed from the job id's low 8 bytes, matching the
/// Postgres backend so the same id decorrelates the same way.
fn seed_from_id(id: Uuid) -> u64 {
    u128::from_le_bytes(*id.as_bytes()) as u64
}

fn check_queue_name(name: &str, allow_dlq: bool) -> Result<()> {
    if name.is_empty() {
        return Err(ForgeError::invalid("queue name must not be empty"));
    }
    if name.len() > MAX_QUEUE_NAME_BYTES {
        return Err(ForgeError::invalid(format!(
            "queue name is {} bytes; max is {MAX_QUEUE_NAME_BYTES}",
            name.len()
        )));
    }
    if !name
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-'))
    {
        return Err(ForgeError::invalid(
            "queue name may only contain [A-Za-z0-9_.-]",
        ));
    }
    if !allow_dlq && name.ends_with(".dlq") {
        return Err(ForgeError::invalid(
            "queue name must not end in '.dlq' (reserved for dead-letter queues)",
        ));
    }
    Ok(())
}

fn check_payload(payload: &[u8]) -> Result<()> {
    if payload.len() > MAX_PAYLOAD_BYTES {
        return Err(ForgeError::limit(format!(
            "payload is {} bytes; max is {MAX_PAYLOAD_BYTES}",
            payload.len()
        )));
    }
    Ok(())
}

fn check_enqueue_opts(opts: &EnqueueOpts) -> Result<()> {
    if opts.delay > MAX_DELAY {
        return Err(ForgeError::invalid(format!(
            "delay exceeds the maximum of {}s",
            MAX_DELAY.as_secs()
        )));
    }
    if opts.max_attempts == 0 || opts.max_attempts > MAX_MAX_ATTEMPTS {
        return Err(ForgeError::invalid(format!(
            "max_attempts must be in 1..={MAX_MAX_ATTEMPTS}"
        )));
    }
    if let Some(d) = &opts.dedup_id
        && d.len() > MAX_DEDUP_ID_LEN
    {
        return Err(ForgeError::limit(format!(
            "dedup_id is {} chars; max is {MAX_DEDUP_ID_LEN}",
            d.len()
        )));
    }
    Ok(())
}

fn check_visibility(vt: Duration) -> Result<()> {
    if vt.is_zero() || vt > MAX_VISIBILITY_TIMEOUT {
        return Err(ForgeError::invalid(format!(
            "visibility_timeout must be in (0, {}s]",
            MAX_VISIBILITY_TIMEOUT.as_secs()
        )));
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn queue() -> MemQueue {
        MemQueue::new(
            Duration::from_secs(300),
            Duration::from_secs(7 * 24 * 60 * 60),
            String::new(),
        )
    }

    /// Long-poll-free dequeue with a roomy lease, so tests never block or expire mid-run.
    fn deq() -> DequeueOpts {
        DequeueOpts::new()
            .with_wait(Duration::ZERO)
            .with_visibility_timeout(Duration::from_secs(60))
    }

    fn payload(s: &str) -> Bytes {
        Bytes::from(s.as_bytes().to_vec())
    }

    #[tokio::test]
    async fn enqueue_dequeue_ack_roundtrips() {
        let q = queue();
        let id = q
            .enqueue("emails", payload("hi"), EnqueueOpts::new())
            .await
            .unwrap();

        let job = q.dequeue("emails", deq()).await.unwrap().expect("a job");
        assert_eq!(job.id, id);
        assert_eq!(job.queue, "emails");
        assert_eq!(job.payload, payload("hi"));
        assert_eq!(job.attempt, 1, "first delivery is attempt 1");

        // Leased now, so a second dequeue finds nothing.
        assert!(q.dequeue("emails", deq()).await.unwrap().is_none());

        q.ack(&job).await.unwrap();
        // Ack is idempotent.
        q.ack(&job).await.unwrap();
        assert_eq!(q.depth("emails").await.unwrap(), QueueDepth::new(0, 0, 0));
    }

    #[tokio::test]
    async fn depth_tracks_visible_in_flight_and_delayed() {
        let q = queue();
        q.enqueue("jobs", payload("now"), EnqueueOpts::new())
            .await
            .unwrap();
        q.enqueue(
            "jobs",
            payload("later"),
            EnqueueOpts::new().with_delay(Duration::from_secs(120)),
        )
        .await
        .unwrap();
        assert_eq!(
            q.depth("jobs").await.unwrap(),
            QueueDepth::new(1, 0, 1),
            "one visible, one delayed"
        );

        let job = q.dequeue("jobs", deq()).await.unwrap().expect("a job");
        assert_eq!(
            q.depth("jobs").await.unwrap(),
            QueueDepth::new(0, 1, 1),
            "the visible one is now in-flight"
        );
        q.ack(&job).await.unwrap();
        assert_eq!(q.depth("jobs").await.unwrap(), QueueDepth::new(0, 0, 1));
    }

    #[tokio::test]
    async fn nack_immediate_redelivers_with_incremented_attempt() {
        let q = queue();
        q.enqueue("work", payload("x"), EnqueueOpts::new())
            .await
            .unwrap();
        let job = q.dequeue("work", deq()).await.unwrap().expect("a job");
        assert_eq!(job.attempt, 1);

        q.nack(&job, NackOpts::retry_in(Duration::ZERO))
            .await
            .unwrap();
        let again = q
            .dequeue("work", deq())
            .await
            .unwrap()
            .expect("redelivered");
        assert_eq!(again.id, job.id);
        assert_eq!(again.attempt, 2, "redelivery increments the attempt");
    }

    #[tokio::test]
    async fn nack_default_delays_with_backoff() {
        let q = queue();
        q.enqueue("work", payload("x"), EnqueueOpts::new())
            .await
            .unwrap();
        let job = q.dequeue("work", deq()).await.unwrap().expect("a job");

        // The default backoff parks the redelivery ~1s out, so it isn't immediately visible.
        q.nack(&job, NackOpts::default()).await.unwrap();
        assert_eq!(
            q.depth("work").await.unwrap(),
            QueueDepth::new(0, 0, 1),
            "default nack parks the job behind the backoff delay"
        );
        assert!(
            q.dequeue("work", deq()).await.unwrap().is_none(),
            "not yet due"
        );
    }

    #[tokio::test]
    async fn exhausted_job_redrives_to_dlq() {
        let q = queue();
        q.enqueue(
            "send",
            payload("x"),
            EnqueueOpts::new().with_max_attempts(1),
        )
        .await
        .unwrap();
        let job = q.dequeue("send", deq()).await.unwrap().expect("a job");
        q.nack(&job, NackOpts::default()).await.unwrap();

        assert_eq!(q.depth("send").await.unwrap(), QueueDepth::new(0, 0, 0));
        assert_eq!(
            q.depth("send.dlq").await.unwrap(),
            QueueDepth::new(1, 0, 0),
            "the exhausted job moved to the dead-letter queue"
        );

        let dead = q
            .dequeue("send.dlq", deq())
            .await
            .unwrap()
            .expect("dlq job");
        assert_eq!(dead.id, job.id);
        assert_eq!(dead.queue, "send.dlq");
        assert_eq!(dead.attempt, 1, "attempts reset on redrive");

        // Exhausting a job already in a `.dlq` is terminal: no `.dlq.dlq`.
        q.nack(&dead, NackOpts::default()).await.unwrap();
        assert_eq!(q.depth("send.dlq").await.unwrap(), QueueDepth::new(0, 0, 0));
        assert_eq!(
            q.depth("send.dlq.dlq").await.unwrap(),
            QueueDepth::new(0, 0, 0)
        );
    }

    #[tokio::test]
    async fn expired_dlq_lease_becomes_dead_not_chained() {
        let q = queue();
        q.enqueue(
            "send",
            payload("x"),
            EnqueueOpts::new().with_max_attempts(1),
        )
        .await
        .unwrap();
        let job = q.dequeue("send", deq()).await.unwrap().expect("a job");
        q.nack(&job, NackOpts::default()).await.unwrap();

        let short = DequeueOpts::new()
            .with_wait(Duration::ZERO)
            .with_visibility_timeout(Duration::from_millis(1));
        let dead = q
            .dequeue("send.dlq", short)
            .await
            .unwrap()
            .expect("dlq job");
        assert_eq!(dead.queue, "send.dlq");

        tokio::time::sleep(Duration::from_millis(5)).await;
        q.maintain_sweep();

        assert_eq!(q.depth("send.dlq").await.unwrap(), QueueDepth::new(0, 0, 0));
        assert_eq!(
            q.depth("send.dlq.dlq").await.unwrap(),
            QueueDepth::new(0, 0, 0),
            "expired DLQ leases must become dead, not chain into a second DLQ"
        );
    }

    #[tokio::test]
    async fn heartbeat_and_fencing() {
        let q = queue();
        q.enqueue("hb", payload("x"), EnqueueOpts::new())
            .await
            .unwrap();
        let job = q.dequeue("hb", deq()).await.unwrap().expect("a job");

        // Live lease: heartbeat succeeds.
        q.heartbeat(&job).await.unwrap();

        // Once acked, the lease is gone: heartbeat is a Precondition failure.
        q.ack(&job).await.unwrap();
        assert!(matches!(
            q.heartbeat(&job).await,
            Err(ForgeError::Precondition(_))
        ));

        // An unknown id is NotFound.
        let ghost = Job::new(
            JobId::new(),
            "hb".to_string(),
            Bytes::new(),
            1,
            5,
            SystemTime::now(),
            Uuid::new_v4(),
        );
        assert!(matches!(
            q.heartbeat(&ghost).await,
            Err(ForgeError::NotFound)
        ));
    }

    #[tokio::test]
    async fn nack_after_release_is_precondition() {
        let q = queue();
        q.enqueue("x", payload("p"), EnqueueOpts::new())
            .await
            .unwrap();
        let job = q.dequeue("x", deq()).await.unwrap().expect("a job");
        // Release the lease, then nack the now-stale handle.
        q.nack(&job, NackOpts::retry_in(Duration::ZERO))
            .await
            .unwrap();
        assert!(matches!(
            q.nack(&job, NackOpts::default()).await,
            Err(ForgeError::Precondition(_))
        ));
    }

    #[tokio::test]
    async fn dedup_within_window_returns_same_job() {
        let q = queue();
        let opts = || EnqueueOpts::new().with_dedup_id("order-42");
        let a = q.enqueue("orders", payload("v1"), opts()).await.unwrap();
        let b = q.enqueue("orders", payload("v2"), opts()).await.unwrap();
        assert_eq!(a, b, "same dedup id within the window collapses to one job");
        assert_eq!(
            q.depth("orders").await.unwrap(),
            QueueDepth::new(1, 0, 0),
            "only one job was actually enqueued"
        );
    }

    #[tokio::test]
    async fn namespaces_isolate_queues() {
        let a = MemQueue::new(
            Duration::from_secs(300),
            Duration::from_secs(3600),
            "app_a".to_string(),
        );
        let bb = MemQueue::new(
            Duration::from_secs(300),
            Duration::from_secs(3600),
            "app_b".to_string(),
        );
        a.enqueue("jobs", payload("from-a"), EnqueueOpts::new())
            .await
            .unwrap();

        assert_eq!(a.depth("jobs").await.unwrap(), QueueDepth::new(1, 0, 0));
        assert_eq!(
            bb.depth("jobs").await.unwrap(),
            QueueDepth::new(0, 0, 0),
            "the other namespace sees nothing"
        );
        assert!(
            bb.dequeue("jobs", deq()).await.unwrap().is_none(),
            "cannot consume across namespaces"
        );

        let job = a.dequeue("jobs", deq()).await.unwrap().expect("a job");
        assert_eq!(job.queue, "jobs", "the caller sees the logical name");
        assert_eq!(job.payload, payload("from-a"));
    }

    #[tokio::test]
    async fn input_validation() {
        let q = queue();
        assert!(matches!(
            q.enqueue("", payload("x"), EnqueueOpts::new()).await,
            Err(ForgeError::Invalid(_))
        ));
        assert!(
            matches!(
                q.enqueue("jobs.dlq", payload("x"), EnqueueOpts::new())
                    .await,
                Err(ForgeError::Invalid(_))
            ),
            "enqueue to a reserved .dlq name is rejected"
        );
        assert!(matches!(
            q.enqueue(
                "jobs",
                payload("x"),
                EnqueueOpts::new().with_max_attempts(0)
            )
            .await,
            Err(ForgeError::Invalid(_))
        ));
        assert!(matches!(
            q.enqueue(
                "jobs",
                Bytes::from(vec![0u8; MAX_PAYLOAD_BYTES + 1]),
                EnqueueOpts::new()
            )
            .await,
            Err(ForgeError::Limit(_))
        ));
        assert!(matches!(
            q.dequeue("jobs", deq().with_visibility_timeout(Duration::ZERO))
                .await,
            Err(ForgeError::Invalid(_))
        ));
        // Dequeue may target a `.dlq` (consuming a dead-letter backlog).
        assert!(q.dequeue("jobs.dlq", deq()).await.unwrap().is_none());
    }

    #[tokio::test]
    async fn maintain_reclaims_expired_leases_across_queues() {
        let q = queue();
        q.enqueue("a", payload("x"), EnqueueOpts::new())
            .await
            .unwrap();
        // Lease with a 1s visibility timeout, then let it lapse.
        let job = q
            .dequeue("a", deq().with_visibility_timeout(Duration::from_secs(1)))
            .await
            .unwrap()
            .expect("a job");
        assert_eq!(q.depth("a").await.unwrap(), QueueDepth::new(0, 1, 0));

        tokio::time::sleep(Duration::from_millis(1100)).await;
        // The lease has lapsed: depth already counts it visible, and maintain reclaims it.
        assert_eq!(q.depth("a").await.unwrap(), QueueDepth::new(1, 0, 0));
        q.maintain_sweep();

        let redelivered = q.dequeue("a", deq()).await.unwrap().expect("reclaimed");
        assert_eq!(redelivered.id, job.id);
        assert_eq!(redelivered.attempt, 2, "crash reclaim bumps the attempt");
    }
}

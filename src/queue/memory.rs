#[cfg(test)]
use super::BatchEnqueueItem;
use super::{
    Backoff, DeadLetterInfo, DeadLetterPage, DequeueOpts, EnqueueOpts, Job, JobId, JobState,
    JobStatus, JobStatusFilter, JobStatusPage, MAX_CONCURRENCY_KEY_BYTES, MAX_OPERATOR_BATCH,
    MAX_PAYLOAD_BYTES, MAX_VISIBILITY_TIMEOUT, MAX_WAIT, NackOpts, Priority, Queue, QueueDepth,
    QueueStats, RedriveBatchResult, RedriveDedupPolicy, RedriveOpts, TerminalRetention,
    safe_failure_summary,
};
use crate::backend::{BackendLifecycle, Primitive};
use crate::clock::Clock;
use crate::error::{ForgeError, Result};
use async_trait::async_trait;
use bytes::Bytes;
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, SystemTime};
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
    /// Cancellation completed before delivery or after cooperative handler exit.
    Cancelled,
}

/// One job row. The id is the map key, so it lives outside the struct.
struct JobRow {
    /// Physical (namespaced) queue name; may end in `.dlq` after redrive.
    queue: String,
    payload: Bytes,
    payload_retained: bool,
    status: Status,
    /// Count of FAILED deliveries; the next delivery is `attempts + 1`.
    attempts: u32,
    max_attempts: u32,
    /// When an `Available` job becomes visible.
    available_at: Duration,
    /// Lease deadline for a `Leased` job.
    leased_until: Option<Duration>,
    /// Per-lease fence token; `ack`/`nack`/`heartbeat` only act while it matches.
    lease_token: Option<Uuid>,
    /// The lease duration, so `heartbeat` can re-extend by the original timeout.
    lease_dur: Option<Duration>,
    /// When a `Done` job was acked, for the retention sweep.
    completed_at: Option<Duration>,
    enqueued_at: SystemTime,
    dead_lettered_at: Option<SystemTime>,
    dead_attempts: u32,
    failure_summary: Option<String>,
    trace_context: Option<crate::TraceContext>,
    sequence: u64,
    priority: Priority,
    concurrency_key: Option<String>,
    cancel_requested: bool,
}

struct AvailableJobRow {
    queue: String,
    payload: Bytes,
    max_attempts: u32,
    available_at: Duration,
    enqueued_at: SystemTime,
    trace_context: Option<crate::TraceContext>,
    sequence: u64,
    priority: Priority,
    concurrency_key: Option<String>,
}

impl JobRow {
    fn available(input: AvailableJobRow) -> Self {
        Self {
            queue: input.queue,
            payload: input.payload,
            payload_retained: true,
            status: Status::Available,
            attempts: 0,
            max_attempts: input.max_attempts,
            available_at: input.available_at,
            leased_until: None,
            lease_token: None,
            lease_dur: None,
            completed_at: None,
            enqueued_at: input.enqueued_at,
            dead_lettered_at: None,
            dead_attempts: 0,
            failure_summary: None,
            trace_context: input.trace_context,
            sequence: input.sequence,
            priority: input.priority,
            concurrency_key: input.concurrency_key,
            cancel_requested: false,
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
    expires_at: Duration,
}

/// All mutable state behind one lock, so a single critical section keeps the jobs and
/// their dedup slots consistent without any lock-ordering concern.
struct State {
    jobs: HashMap<Uuid, JobRow>,
    dedup: HashMap<(String, String), DedupEntry>,
    paused: HashSet<String>,
    counters: HashMap<String, QueueCounter>,
    next_sequence: u64,
}

#[derive(Default)]
struct QueueCounter {
    started_at: Duration,
    enqueued: u64,
    settled: u64,
    dead: u64,
    cancelled: u64,
}

/// In-process [`Queue`]. Not durable: state lives in this process only.
pub(crate) struct MemQueue {
    state: Mutex<State>,
    dedup_window: Duration,
    payload_retention: Duration,
    terminal_retention: TerminalRetention,
    /// Namespace prefix on queue names (`<ns>:<queue>`). Empty = no prefix.
    namespace: String,
    clock: Arc<dyn Clock>,
}

impl MemQueue {
    #[cfg(test)]
    pub(crate) fn new(dedup_window: Duration, retention: Duration, namespace: String) -> Self {
        Self::with_clock(
            dedup_window,
            retention,
            retention,
            namespace,
            Arc::new(crate::clock::SystemClock::new()),
        )
    }

    #[cfg(test)]
    pub(crate) fn with_clock(
        dedup_window: Duration,
        payload_retention: Duration,
        terminal_retention: Duration,
        namespace: String,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self::with_retention(
            dedup_window,
            payload_retention,
            TerminalRetention {
                succeeded: terminal_retention,
                dead: terminal_retention,
                cancelled: terminal_retention,
            },
            namespace,
            clock,
        )
    }

    pub(crate) fn with_retention(
        dedup_window: Duration,
        payload_retention: Duration,
        terminal_retention: TerminalRetention,
        namespace: String,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            state: Mutex::new(State {
                jobs: HashMap::new(),
                dedup: HashMap::new(),
                paused: HashSet::new(),
                counters: HashMap::new(),
                next_sequence: 0,
            }),
            dedup_window,
            payload_retention,
            terminal_retention,
            namespace,
            clock,
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

    fn status_for(&self, id: JobId, row: &JobRow, now: Duration) -> JobStatus {
        let state = match row.status {
            Status::Available if row.attempts > 0 => JobState::Retrying,
            Status::Available if row.available_at > now => JobState::Delayed,
            Status::Available => JobState::Queued,
            Status::Leased if row.cancel_requested => JobState::CancelRequested,
            Status::Leased => JobState::Leased,
            Status::Done => JobState::Succeeded,
            Status::Dead => JobState::Dead,
            Status::Cancelled => JobState::Cancelled,
        };
        let wall_now = self.clock.now();
        let available_at = if row.available_at >= now {
            wall_now + row.available_at.saturating_sub(now)
        } else {
            wall_now
                .checked_sub(now.saturating_sub(row.available_at))
                .unwrap_or(SystemTime::UNIX_EPOCH)
        };
        let completed_at = row.completed_at.map(|at| {
            if at >= now {
                wall_now + at.saturating_sub(now)
            } else {
                wall_now
                    .checked_sub(now.saturating_sub(at))
                    .unwrap_or(SystemTime::UNIX_EPOCH)
            }
        });
        JobStatus {
            id,
            queue: self.logical(&row.queue),
            state,
            attempt_count: row.attempts,
            max_attempts: row.max_attempts,
            priority: row.priority,
            concurrency_key: row.concurrency_key.clone(),
            enqueued_at: row.enqueued_at,
            available_at,
            completed_at,
        }
    }

    /// Reclaim expired leases: exhausted non-DLQ jobs re-home to `.dlq` (attempts reset),
    /// exhausted DLQ jobs become terminal `dead`, and the rest return to `available`
    /// immediately with attempts bumped. With `queue: Some`, scoped to that physical queue;
    /// `None` sweeps every queue. Mirrors [`super::PgQueue::reclaim`].
    fn reclaim_locked(
        jobs: &mut HashMap<Uuid, JobRow>,
        now: Duration,
        now_wall: SystemTime,
        queue: Option<&str>,
    ) -> Vec<Uuid> {
        let mut released = Vec::new();
        for (id, row) in jobs.iter_mut() {
            if queue.is_some_and(|q| row.queue != q) {
                continue;
            }
            if row.status != Status::Leased || row.leased_until.is_none_or(|d| d > now) {
                continue;
            }
            if row.cancel_requested {
                row.status = Status::Cancelled;
                row.completed_at = Some(now);
                row.clear_lease();
                released.push(*id);
                continue;
            }
            let new_attempts = row.attempts.saturating_add(1);
            if new_attempts >= row.max_attempts {
                if row.queue.ends_with(".dlq") {
                    row.status = Status::Dead;
                    row.attempts = new_attempts;
                    row.dead_attempts = row.dead_attempts.saturating_add(new_attempts);
                    row.failure_summary = Some("visibility timeout expired".to_string());
                    row.dead_lettered_at.get_or_insert(now_wall);
                    row.completed_at = Some(now);
                    row.clear_lease();
                    released.push(*id);
                    continue;
                } else {
                    row.queue.push_str(".dlq");
                    row.dead_attempts = new_attempts;
                    row.failure_summary = Some("visibility timeout expired".to_string());
                    row.dead_lettered_at = Some(now_wall);
                    row.attempts = 0;
                    released.push(*id);
                }
            } else {
                row.attempts = new_attempts;
            }
            row.status = Status::Available;
            row.available_at = now;
            row.clear_lease();
        }
        released
    }

    fn release_dedup(state: &mut State, ids: &[Uuid]) {
        state.dedup.retain(|_, entry| !ids.contains(&entry.job_id));
    }

    /// Claim and lease the oldest due job in `pq`, or `None` if none are ready.
    fn try_claim_locked(
        &self,
        jobs: &mut HashMap<Uuid, JobRow>,
        pq: &str,
        vt: Duration,
        concurrency_limit_per_key: Option<u32>,
    ) -> Option<Job> {
        let now = self.clock.elapsed();
        let id = *jobs
            .iter()
            .filter(|(_, r)| {
                if r.queue != pq || r.status != Status::Available || r.available_at > now {
                    return false;
                }
                let Some(limit) = concurrency_limit_per_key else {
                    return true;
                };
                let Some(key) = r.concurrency_key.as_deref() else {
                    return true;
                };
                let leased = jobs
                    .values()
                    .filter(|other| {
                        other.queue == pq
                            && other.status == Status::Leased
                            && !other.cancel_requested
                            && other.concurrency_key.as_deref() == Some(key)
                            && other.leased_until.is_some_and(|until| until > now)
                    })
                    .count();
                leased < limit as usize
            })
            .min_by_key(|(_, r)| (std::cmp::Reverse(r.priority), r.available_at, r.sequence))
            .map(|(id, _)| id)?;

        let token = Uuid::new_v4();
        // The Job carries a wall-clock deadline; internal lease timing stays monotonic.
        let leased_until = self.clock.now() + vt;
        let row = jobs.get_mut(&id)?;
        row.status = Status::Leased;
        row.leased_until = Some(now + vt);
        row.lease_token = Some(token);
        row.lease_dur = Some(vt);
        // attempts counts FAILED deliveries; this delivery is attempts + 1.
        let attempt = row.attempts.saturating_add(1);
        Some(
            Job::new(
                JobId(id),
                self.logical(&row.queue),
                row.payload.clone(),
                attempt,
                row.max_attempts,
                leased_until,
                token,
            )
            .with_trace_context(row.trace_context.clone())
            .with_scheduling(row.priority, row.concurrency_key.clone()),
        )
    }

    /// Idempotent maintenance: purge old `done` jobs, reclaim expired leases across all
    /// queues, drop stale dedup slots. Mirrors [`super::PgQueue::maintenance`].
    pub(crate) fn maintain_sweep(&self) {
        let now = self.clock.elapsed();
        let now_wall = self.clock.now();
        let payload_retention = self.payload_retention;
        let terminal_retention = self.terminal_retention;
        let mut state = self.lock();
        for row in state.jobs.values_mut() {
            if matches!(row.status, Status::Done | Status::Dead | Status::Cancelled)
                && row
                    .completed_at
                    .is_some_and(|at| now.saturating_sub(at) >= payload_retention)
            {
                row.payload = Bytes::new();
                row.payload_retained = false;
            }
        }
        state.jobs.retain(|_, r| {
            let retention = match r.status {
                Status::Done => Some(terminal_retention.succeeded),
                Status::Dead => Some(terminal_retention.dead),
                Status::Cancelled => Some(terminal_retention.cancelled),
                Status::Available | Status::Leased => None,
            };
            !retention.is_some_and(|retention| {
                r.completed_at
                    .is_some_and(|completed| now.saturating_sub(completed) >= retention)
            })
        });
        let released = Self::reclaim_locked(&mut state.jobs, now, now_wall, None);
        Self::release_dedup(&mut state, &released);
        state.dedup.retain(|_, e| e.expires_at > now);
    }
}

#[async_trait]
impl Queue for MemQueue {
    async fn enqueue(&self, queue: &str, payload: Bytes, opts: EnqueueOpts) -> Result<JobId> {
        check_queue_name(queue, false)?;
        check_payload(&payload)?;
        check_enqueue_opts(&opts)?;

        let now = self.clock.elapsed();
        let now_wall = self.clock.now();
        let available_at = now + opts.delay;
        let pq = self.physical(queue);
        let mut state = self.lock();

        let requested_id = opts.job_id.map(|id| id.0);
        let trace_context = opts.trace_context.clone();
        let id = if let Some(dedup_id) = opts.dedup_id {
            let key = (pq.clone(), dedup_id);
            // A live slot returns its existing job id (success, not an error). An expired
            // slot is overwritten and a fresh job created, matching the Postgres upsert.
            if let Some(entry) = state.dedup.get(&key)
                && entry.expires_at > now
            {
                if requested_id.is_some_and(|requested| requested != entry.job_id) {
                    return Err(ForgeError::precondition(
                        "deduplication id is reserved by a different job id",
                    ));
                }
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
        let sequence = state.next_sequence;
        state.next_sequence = state.next_sequence.saturating_add(1);
        state.jobs.insert(
            id,
            JobRow::available(AvailableJobRow {
                queue: pq,
                payload,
                max_attempts: opts.max_attempts,
                available_at,
                enqueued_at: now_wall,
                trace_context,
                sequence,
                priority: opts.priority,
                concurrency_key: opts.concurrency_key,
            }),
        );
        let counter = state
            .counters
            .entry(self.physical(queue))
            .or_insert_with(|| QueueCounter {
                started_at: now,
                ..QueueCounter::default()
            });
        counter.enqueued = counter.enqueued.saturating_add(1);
        Ok(JobId(id))
    }

    async fn dequeue(&self, queue: &str, opts: DequeueOpts) -> Result<Option<Job>> {
        check_queue_name(queue, true)?;
        check_visibility(opts.visibility_timeout)?;
        if opts.concurrency_limit_per_key == Some(0) {
            return Err(ForgeError::invalid(
                "concurrency_limit_per_key must be positive",
            ));
        }
        let pq = self.physical(queue);
        let vt = opts.visibility_timeout;
        let wait = opts.wait.min(MAX_WAIT);

        // Reclaim expired leases once up front so crashed work redelivers.
        {
            let mut state = self.lock();
            if state.paused.contains(&pq) {
                return Ok(None);
            }
            let released = Self::reclaim_locked(
                &mut state.jobs,
                self.clock.elapsed(),
                self.clock.now(),
                Some(&pq),
            );
            Self::release_dedup(&mut state, &released);
        }

        let deadline = tokio::time::Instant::now() + wait;
        loop {
            let claimed = {
                let mut state = self.lock();
                if state.paused.contains(&pq) {
                    return Ok(None);
                }
                self.try_claim_locked(&mut state.jobs, &pq, vt, opts.concurrency_limit_per_key)
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
        let queue = self.physical(&job.queue);
        let mut state = self.lock();
        if let Some(row) = state.jobs.get_mut(&job.id.0)
            && row.status == Status::Leased
            && row.lease_token == Some(job.lease_token())
            && row.queue == queue
            && !row.cancel_requested
        {
            row.status = Status::Done;
            row.completed_at = Some(self.clock.elapsed());
            row.clear_lease();
            let counter = state.counters.entry(queue).or_default();
            counter.settled = counter.settled.saturating_add(1);
            return Ok(());
        }
        if state.jobs.contains_key(&job.id.0) {
            Err(ForgeError::precondition(
                "lease lost: another worker owns this job",
            ))
        } else {
            Err(ForgeError::NotFound)
        }
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
        let queue = self.physical(&job.queue);
        // A job already in a `.dlq` queue must not re-home into an unwatched `.dlq.dlq`;
        // exhaustion there is terminal.
        let in_dlq = job.queue.ends_with(".dlq");
        let now = self.clock.elapsed();
        let now_wall = self.clock.now();
        let mut state = self.lock();

        let Some(row) = state.jobs.get_mut(&id) else {
            return Err(ForgeError::NotFound);
        };
        if row.status != Status::Leased
            || row.lease_token != Some(token)
            || row.queue != queue
            || row.cancel_requested
        {
            return Err(ForgeError::precondition(
                "lease lost: another worker owns this job",
            ));
        }

        let new_attempts = row.attempts.saturating_add(1);
        if new_attempts >= row.max_attempts {
            let summary = opts
                .failure_summary
                .map(safe_failure_summary)
                .unwrap_or_else(|| "handler failed".to_string());
            if in_dlq {
                // Terminal: park as 'dead' with attempts pinned; nothing re-homes it.
                row.status = Status::Dead;
                row.attempts = new_attempts;
                row.dead_attempts = row.dead_attempts.saturating_add(new_attempts);
                row.failure_summary = Some(summary);
                row.dead_lettered_at.get_or_insert(now_wall);
                row.completed_at = Some(now);
                row.clear_lease();
            } else {
                // Exhausted: re-home to DLQ with attempts reset so DLQ consumers see a clean slate.
                row.queue.push_str(".dlq");
                row.status = Status::Available;
                row.dead_attempts = new_attempts;
                row.failure_summary = Some(summary);
                row.dead_lettered_at = Some(now_wall);
                row.attempts = 0;
                row.available_at = now;
                row.clear_lease();
            }
            let counter = state.counters.entry(queue).or_default();
            counter.settled = counter.settled.saturating_add(1);
            counter.dead = counter.dead.saturating_add(1);
            state.dedup.retain(|_, entry| entry.job_id != id);
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
        let queue = self.physical(&job.queue);
        let now = self.clock.elapsed();
        let mut state = self.lock();

        let Some(row) = state.jobs.get_mut(&id) else {
            return Err(ForgeError::NotFound);
        };
        if row.status != Status::Leased
            || row.lease_token != Some(token)
            || row.queue != queue
            || row.cancel_requested
        {
            return Err(ForgeError::precondition(
                "lease lost: another worker owns this job",
            ));
        }
        if let Some(d) = row.lease_dur {
            row.leased_until = Some(now + d);
        }
        Ok(())
    }

    async fn cancellation_requested(&self, job: &Job) -> Result<bool> {
        let state = self.lock();
        let Some(row) = state.jobs.get(&job.id.0) else {
            return Err(ForgeError::NotFound);
        };
        let requested = row.cancel_requested || row.status == Status::Cancelled;
        if requested {
            job.cancellation.signal();
        }
        Ok(requested)
    }

    async fn cancel(&self, id: JobId) -> Result<Option<JobStatus>> {
        let now = self.clock.elapsed();
        let mut state = self.lock();
        let Some(row) = state.jobs.get_mut(&id.0) else {
            return Ok(None);
        };
        let mut settled_queue = None;
        match row.status {
            Status::Available => {
                row.status = Status::Cancelled;
                row.cancel_requested = false;
                row.completed_at = Some(now);
                row.clear_lease();
                settled_queue = Some(row.queue.clone());
            }
            Status::Leased => row.cancel_requested = true,
            Status::Done | Status::Dead | Status::Cancelled => {}
        }
        let out = self.status_for(id, row, now);
        if let Some(queue) = settled_queue {
            let counter = state.counters.entry(queue).or_default();
            counter.settled = counter.settled.saturating_add(1);
            counter.cancelled = counter.cancelled.saturating_add(1);
        }
        state.dedup.retain(|_, entry| entry.job_id != id.0);
        Ok(Some(out))
    }

    async fn finish_cancellation(&self, job: &Job) -> Result<()> {
        let mut state = self.lock();
        let Some(row) = state.jobs.get_mut(&job.id.0) else {
            return Err(ForgeError::NotFound);
        };
        if row.status != Status::Leased
            || row.lease_token != Some(job.lease_token())
            || !row.cancel_requested
        {
            return Err(ForgeError::precondition("cancellation fence was lost"));
        }
        row.status = Status::Cancelled;
        row.completed_at = Some(self.clock.elapsed());
        let queue = row.queue.clone();
        row.clear_lease();
        let counter = state.counters.entry(queue).or_default();
        counter.settled = counter.settled.saturating_add(1);
        counter.cancelled = counter.cancelled.saturating_add(1);
        Ok(())
    }

    async fn status(&self, id: JobId) -> Result<Option<JobStatus>> {
        let state = self.lock();
        Ok(state
            .jobs
            .get(&id.0)
            .map(|row| self.status_for(id, row, self.clock.elapsed())))
    }

    async fn list_status(&self, filter: JobStatusFilter) -> Result<JobStatusPage> {
        check_operator_limit(filter.limit)?;
        if let Some(queue) = &filter.queue {
            check_queue_name(queue, true)?;
        }
        let after = filter
            .cursor
            .as_ref()
            .and_then(|value| Uuid::parse_str(value.token()).ok());
        if filter.cursor.is_some() && after.is_none() {
            return Err(ForgeError::invalid("invalid job-status cursor"));
        }
        let now = self.clock.elapsed();
        let state = self.lock();
        let mut items = state
            .jobs
            .iter()
            .filter(|(_, row)| {
                filter
                    .queue
                    .as_ref()
                    .is_none_or(|q| row.queue == self.physical(q))
            })
            .map(|(id, row)| self.status_for(JobId(*id), row, now))
            .filter(|item| filter.states.is_empty() || filter.states.contains(&item.state))
            .collect::<Vec<_>>();
        items.sort_by_key(|item| (item.enqueued_at, item.id.0));
        if let Some(after) = after {
            if let Some(index) = items.iter().position(|item| item.id.0 == after) {
                items.drain(..=index);
            } else {
                return Err(ForgeError::invalid(
                    "job-status cursor is not in this result set",
                ));
            }
        }
        let more = items.len() > filter.limit as usize;
        items.truncate(filter.limit as usize);
        let next_cursor = more.then(|| {
            crate::Cursor::from_token(
                items
                    .last()
                    .map(|item| item.id.to_string())
                    .unwrap_or_default(),
            )
        });
        Ok(JobStatusPage { items, next_cursor })
    }

    async fn depth(&self, queue: &str) -> Result<QueueDepth> {
        // Allow `.dlq` names: gauging a dead-letter backlog is a primary use.
        check_queue_name(queue, true)?;
        let pq = self.physical(queue);
        let now = self.clock.elapsed();
        let state = self.lock();

        let (mut visible, mut in_flight, mut delayed) = (0u64, 0u64, 0u64);
        let mut oldest: Option<SystemTime> = None;
        for row in state.jobs.values() {
            if row.queue != pq {
                continue;
            }
            match row.status {
                Status::Available if row.available_at <= now => {
                    visible += 1;
                    oldest =
                        Some(oldest.map_or(row.enqueued_at, |value| value.min(row.enqueued_at)));
                }
                Status::Available => delayed += 1,
                // An expired-but-unreclaimed lease counts as visible: the next dequeue hands it out.
                Status::Leased if row.leased_until.is_some_and(|d| d <= now) => {
                    visible += 1;
                    oldest =
                        Some(oldest.map_or(row.enqueued_at, |value| value.min(row.enqueued_at)));
                }
                Status::Leased => in_flight += 1,
                Status::Done | Status::Dead | Status::Cancelled => {}
            }
        }
        let age = oldest
            .and_then(|at| self.clock.now().duration_since(at).ok())
            .map(|d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
        Ok(QueueDepth::new(visible, in_flight, delayed).with_oldest_visible_age_ms(age))
    }

    async fn pause(&self, queue: &str) -> Result<()> {
        check_queue_name(queue, true)?;
        self.lock().paused.insert(self.physical(queue));
        Ok(())
    }

    async fn resume(&self, queue: &str) -> Result<()> {
        check_queue_name(queue, true)?;
        self.lock().paused.remove(&self.physical(queue));
        Ok(())
    }

    async fn is_paused(&self, queue: &str) -> Result<bool> {
        check_queue_name(queue, true)?;
        Ok(self.lock().paused.contains(&self.physical(queue)))
    }

    async fn stats(&self, queue: &str) -> Result<QueueStats> {
        check_queue_name(queue, true)?;
        let pq = self.physical(queue);
        let now = self.clock.elapsed();
        let wall_now = self.clock.now();
        let state = self.lock();
        let oldest = state
            .jobs
            .values()
            .filter(|row| {
                row.queue == pq
                    && ((row.status == Status::Available && row.available_at <= now)
                        || (row.status == Status::Leased
                            && row.leased_until.is_some_and(|until| until <= now)))
            })
            .map(|row| row.enqueued_at)
            .min()
            .and_then(|at| wall_now.duration_since(at).ok())
            .map(|age| u64::try_from(age.as_millis()).unwrap_or(u64::MAX));
        let Some(counter) = state.counters.get(&pq) else {
            return Ok(QueueStats {
                oldest_visible_age_ms: oldest,
                paused: state.paused.contains(&pq),
                ..QueueStats::default()
            });
        };
        let minutes = now
            .saturating_sub(counter.started_at)
            .as_secs_f64()
            .max(1.0)
            / 60.0;
        Ok(QueueStats {
            enqueued_total: counter.enqueued,
            settled_total: counter.settled,
            dead_total: counter.dead,
            cancelled_total: counter.cancelled,
            enqueue_rate_per_minute: counter.enqueued as f64 / minutes,
            settle_rate_per_minute: counter.settled as f64 / minutes,
            oldest_visible_age_ms: oldest,
            paused: state.paused.contains(&pq),
        })
    }

    async fn dead_letters(
        &self,
        queue: &str,
        cursor: Option<crate::Cursor>,
        limit: u32,
    ) -> Result<DeadLetterPage> {
        check_queue_name(queue, false)?;
        check_operator_limit(limit)?;
        let pq = self.physical(&format!("{queue}.dlq"));
        let after = cursor.and_then(|value| Uuid::parse_str(value.token()).ok());
        let state = self.lock();
        let mut rows: Vec<_> = state
            .jobs
            .iter()
            .filter(|(_, row)| {
                row.queue == pq && matches!(row.status, Status::Available | Status::Dead)
            })
            .filter(|(id, _)| after.is_none_or(|after| **id > after))
            .collect();
        rows.sort_by_key(|(id, _)| **id);
        let more = rows.len() > limit as usize;
        rows.truncate(limit as usize);
        let items = rows
            .iter()
            .map(|(id, row)| DeadLetterInfo {
                job_id: JobId(**id),
                queue: queue.to_string(),
                attempt_count: row.dead_attempts.max(row.attempts),
                enqueued_at: row.enqueued_at,
                dead_lettered_at: row.dead_lettered_at.unwrap_or(row.enqueued_at),
                failure_summary: row.failure_summary.clone(),
            })
            .collect::<Vec<_>>();
        let next_cursor = more.then(|| {
            crate::Cursor::from_token(
                rows.last()
                    .map(|(id, _)| id.to_string())
                    .unwrap_or_default(),
            )
        });
        Ok(DeadLetterPage { items, next_cursor })
    }

    async fn redrive(&self, job_id: JobId, opts: RedriveOpts) -> Result<bool> {
        check_queue_name(&opts.destination, false)?;
        let destination = self.physical(&opts.destination);
        let mut state = self.lock();
        let Some(row) = state.jobs.get_mut(&job_id.0) else {
            return Ok(false);
        };
        let owned = if self.namespace.is_empty() {
            !row.queue.contains(':')
        } else {
            row.queue.starts_with(&format!("{}:", self.namespace))
        };
        if !owned
            || !row.queue.ends_with(".dlq")
            || !matches!(row.status, Status::Available | Status::Dead)
        {
            return Ok(false);
        }
        if !row.payload_retained {
            return Err(ForgeError::precondition(
                "dead-letter payload retention elapsed; the job cannot be redriven",
            ));
        }
        row.queue = destination;
        row.status = Status::Available;
        row.attempts = 0;
        row.available_at = self.clock.elapsed();
        row.completed_at = None;
        row.dead_attempts = 0;
        row.dead_lettered_at = None;
        row.failure_summary = None;
        row.cancel_requested = false;
        row.clear_lease();
        if opts.dedup_policy == RedriveDedupPolicy::Clear {
            state.dedup.retain(|_, entry| entry.job_id != job_id.0);
        }
        Ok(true)
    }

    async fn redrive_batch(
        &self,
        queue: &str,
        cursor: Option<crate::Cursor>,
        limit: u32,
        opts: RedriveOpts,
    ) -> Result<RedriveBatchResult> {
        let page = self.dead_letters(queue, cursor, limit).await?;
        let mut redriven = 0;
        for item in &page.items {
            redriven += u32::from(self.redrive(item.job_id, opts.clone()).await?);
        }
        Ok(RedriveBatchResult {
            redriven,
            next_cursor: page.next_cursor,
        })
    }

    async fn purge_dead_letters_dry_run(&self, queue: &str) -> Result<u64> {
        check_queue_name(queue, false)?;
        let pq = self.physical(&format!("{queue}.dlq"));
        let state = self.lock();
        Ok(state
            .jobs
            .values()
            .filter(|row| row.queue == pq && matches!(row.status, Status::Available | Status::Dead))
            .count() as u64)
    }

    async fn purge_dead_letters(&self, queue: &str, confirmation: &str) -> Result<u64> {
        check_queue_name(queue, false)?;
        if confirmation != queue {
            return Err(ForgeError::precondition(
                "purge confirmation must exactly match the source queue",
            ));
        }
        let pq = self.physical(&format!("{queue}.dlq"));
        let mut state = self.lock();
        let ids = state
            .jobs
            .iter()
            .filter(|(_, row)| {
                row.queue == pq && matches!(row.status, Status::Available | Status::Dead)
            })
            .map(|(id, _)| *id)
            .collect::<Vec<_>>();
        for id in &ids {
            state.jobs.remove(id);
        }
        Self::release_dedup(&mut state, &ids);
        Ok(ids.len() as u64)
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
    if let Some(key) = &opts.concurrency_key {
        if key.is_empty() {
            return Err(ForgeError::invalid("concurrency_key must not be empty"));
        }
        if key.len() > MAX_CONCURRENCY_KEY_BYTES {
            return Err(ForgeError::limit(format!(
                "concurrency_key is {} bytes; max is {MAX_CONCURRENCY_KEY_BYTES}",
                key.len()
            )));
        }
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

fn check_operator_limit(limit: u32) -> Result<()> {
    if limit == 0 || limit > MAX_OPERATOR_BATCH {
        return Err(ForgeError::invalid(format!(
            "limit must be in 1..={MAX_OPERATOR_BATCH}"
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
        assert!(matches!(
            q.ack(&job).await,
            Err(ForgeError::Precondition(_))
        ));
        assert_eq!(q.depth("emails").await.unwrap(), QueueDepth::new(0, 0, 0));
    }

    #[tokio::test]
    async fn batch_pause_resume_and_stats_compose() {
        let q = queue();
        let deterministic = JobId::parse("11111111-1111-4111-8111-111111111111").unwrap();
        let results = q
            .enqueue_batch(
                "batch",
                vec![
                    BatchEnqueueItem::new(
                        payload("one"),
                        EnqueueOpts::new().with_job_id(deterministic),
                    ),
                    BatchEnqueueItem::new(payload("two"), EnqueueOpts::new()),
                    BatchEnqueueItem::new(
                        Bytes::from(vec![0; MAX_PAYLOAD_BYTES + 1]),
                        EnqueueOpts::new(),
                    ),
                ],
            )
            .await
            .unwrap();
        assert_eq!(results.len(), 3);
        let [deterministic_result, generated_result, invalid_result] = results.as_slice() else {
            unreachable!("batch length was asserted above");
        };
        assert_eq!(deterministic_result.job_id, Some(deterministic));
        assert!(generated_result.job_id.is_some());
        assert!(matches!(invalid_result.error, Some(ForgeError::Limit(_))));

        q.pause("batch").await.unwrap();
        assert!(q.is_paused("batch").await.unwrap());
        assert!(q.dequeue("batch", deq()).await.unwrap().is_none());
        q.resume("batch").await.unwrap();
        let jobs = q.dequeue_batch("batch", 10, deq()).await.unwrap();
        assert_eq!(jobs.len(), 2);
        for job in &jobs {
            q.ack(job).await.unwrap();
        }
        let stats = q.stats("batch").await.unwrap();
        assert_eq!(stats.enqueued_total, 2);
        assert_eq!(stats.settled_total, 2);
        assert!(!stats.paused);
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
    async fn expired_dead_letter_payload_cannot_be_redriven() {
        let clock = Arc::new(crate::clock::ManualClock::new(SystemTime::UNIX_EPOCH));
        let q = MemQueue::with_clock(
            Duration::from_secs(300),
            Duration::from_secs(1),
            Duration::from_secs(2),
            String::new(),
            clock.clone(),
        );
        let id = q
            .enqueue(
                "send",
                payload("secret"),
                EnqueueOpts::new().with_max_attempts(1),
            )
            .await
            .unwrap();
        let job = q.dequeue("send", deq()).await.unwrap().expect("a job");
        q.nack(&job, NackOpts::default()).await.unwrap();
        let dead = q
            .dequeue("send.dlq", deq())
            .await
            .unwrap()
            .expect("a dead letter");
        q.nack(&dead, NackOpts::default()).await.unwrap();

        clock.advance(Duration::from_secs(1));
        q.maintain_sweep();

        assert!(q.status(id).await.unwrap().is_some());
        assert!(matches!(
            q.redrive(
                id,
                RedriveOpts::new("retry-send", RedriveDedupPolicy::Clear),
            )
            .await,
            Err(ForgeError::Precondition(_))
        ));
    }

    #[tokio::test]
    async fn terminal_states_have_independent_retention() {
        let clock = Arc::new(crate::clock::ManualClock::new(SystemTime::UNIX_EPOCH));
        let q = MemQueue::with_retention(
            Duration::from_secs(300),
            Duration::from_secs(10),
            TerminalRetention {
                succeeded: Duration::from_secs(1),
                cancelled: Duration::from_secs(2),
                dead: Duration::from_secs(3),
            },
            String::new(),
            clock.clone(),
        );
        let succeeded = q
            .enqueue("retention", payload("done"), EnqueueOpts::new())
            .await
            .unwrap();
        let job = q.dequeue("retention", deq()).await.unwrap().unwrap();
        q.ack(&job).await.unwrap();
        let cancelled = q
            .enqueue("retention", payload("cancel"), EnqueueOpts::new())
            .await
            .unwrap();
        q.cancel(cancelled).await.unwrap();
        let dead = q
            .enqueue(
                "retention",
                payload("dead"),
                EnqueueOpts::new().with_max_attempts(1),
            )
            .await
            .unwrap();
        let job = q.dequeue("retention", deq()).await.unwrap().unwrap();
        q.nack(&job, NackOpts::default()).await.unwrap();
        let job = q.dequeue("retention.dlq", deq()).await.unwrap().unwrap();
        q.nack(&job, NackOpts::default()).await.unwrap();

        clock.advance(Duration::from_secs(1));
        q.maintain_sweep();
        assert!(q.status(succeeded).await.unwrap().is_none());
        assert!(q.status(cancelled).await.unwrap().is_some());
        assert!(q.status(dead).await.unwrap().is_some());
        clock.advance(Duration::from_secs(1));
        q.maintain_sweep();
        assert!(q.status(cancelled).await.unwrap().is_none());
        assert!(q.status(dead).await.unwrap().is_some());
        clock.advance(Duration::from_secs(1));
        q.maintain_sweep();
        assert!(q.status(dead).await.unwrap().is_none());
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
    async fn deterministic_id_is_primary_over_deduplication() {
        let q = queue();
        let first = JobId::new();
        let other = JobId::new();
        let id = q
            .enqueue(
                "orders",
                payload("v1"),
                EnqueueOpts::new()
                    .with_job_id(first)
                    .with_dedup_id("order-42"),
            )
            .await
            .unwrap();
        assert_eq!(id, first);
        assert_eq!(
            q.enqueue(
                "orders",
                payload("ignored"),
                EnqueueOpts::new()
                    .with_job_id(first)
                    .with_dedup_id("order-42"),
            )
            .await
            .unwrap(),
            first
        );
        assert!(matches!(
            q.enqueue(
                "orders",
                payload("conflict"),
                EnqueueOpts::new()
                    .with_job_id(other)
                    .with_dedup_id("order-42"),
            )
            .await,
            Err(ForgeError::Precondition(_))
        ));
    }

    #[tokio::test]
    async fn terminal_failure_releases_dedup_and_operator_actions_are_safe() {
        let q = queue();
        let first = q
            .enqueue(
                "send",
                payload("first"),
                EnqueueOpts::new()
                    .with_max_attempts(1)
                    .with_dedup_id("content-v1"),
            )
            .await
            .unwrap();
        let job = q.dequeue("send", deq()).await.unwrap().expect("job");
        q.nack(
            &job,
            NackOpts::default().with_failure_summary("safe summary"),
        )
        .await
        .unwrap();

        let second = q
            .enqueue(
                "send",
                payload("second"),
                EnqueueOpts::new().with_dedup_id("content-v1"),
            )
            .await
            .unwrap();
        assert_ne!(
            first, second,
            "terminal jobs no longer reserve dedup content"
        );

        let page = q.dead_letters("send", None, 10).await.unwrap();
        assert_eq!(page.items.len(), 1);
        let item = page.items.first().unwrap();
        assert_eq!(item.job_id, first);
        assert_eq!(item.attempt_count, 1);
        assert_eq!(item.failure_summary.as_deref(), Some("safe summary"));

        assert!(
            q.redrive(
                first,
                RedriveOpts::new("retry-send", RedriveDedupPolicy::Clear),
            )
            .await
            .unwrap()
        );
        let redriven = q
            .dequeue("retry-send", deq())
            .await
            .unwrap()
            .expect("redriven");
        assert_eq!(redriven.id, first);
        q.nack(&redriven, NackOpts::default()).await.unwrap();

        assert_eq!(q.purge_dead_letters_dry_run("retry-send").await.unwrap(), 1);
        assert!(matches!(
            q.purge_dead_letters("retry-send", "wrong").await,
            Err(ForgeError::Precondition(_))
        ));
        assert_eq!(
            q.purge_dead_letters("retry-send", "retry-send")
                .await
                .unwrap(),
            1
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

    #[tokio::test]
    async fn cancellation_is_cooperative_and_fenced() {
        let q = queue();
        let queued = q
            .enqueue("jobs", payload("queued"), EnqueueOpts::new())
            .await
            .unwrap();
        let status = q.cancel(queued).await.unwrap().unwrap();
        assert_eq!(status.state, JobState::Cancelled);
        assert!(q.dequeue("jobs", deq()).await.unwrap().is_none());

        let leased_id = q
            .enqueue("jobs", payload("leased"), EnqueueOpts::new())
            .await
            .unwrap();
        let leased = q.dequeue("jobs", deq()).await.unwrap().unwrap();
        assert_eq!(leased.id, leased_id);
        assert_eq!(
            q.cancel(leased_id).await.unwrap().unwrap().state,
            JobState::CancelRequested
        );
        assert!(q.cancellation_requested(&leased).await.unwrap());
        assert!(leased.cancellation.is_cancelled());
        assert!(matches!(
            q.ack(&leased).await,
            Err(ForgeError::Precondition(_))
        ));
        q.finish_cancellation(&leased).await.unwrap();
        assert_eq!(
            q.status(leased_id).await.unwrap().unwrap().state,
            JobState::Cancelled
        );
    }

    #[tokio::test]
    async fn priority_fifo_and_key_concurrency_are_bounded() {
        let q = queue();
        let low = q
            .enqueue(
                "jobs",
                payload("low"),
                EnqueueOpts::new().with_priority(Priority::Low),
            )
            .await
            .unwrap();
        let first_high = q
            .enqueue(
                "jobs",
                payload("high-1"),
                EnqueueOpts::new()
                    .with_priority(Priority::High)
                    .with_concurrency_key("tenant-a"),
            )
            .await
            .unwrap();
        let second_high = q
            .enqueue(
                "jobs",
                payload("high-2"),
                EnqueueOpts::new()
                    .with_priority(Priority::High)
                    .with_concurrency_key("tenant-a"),
            )
            .await
            .unwrap();
        let normal = q
            .enqueue(
                "jobs",
                payload("normal"),
                EnqueueOpts::new().with_concurrency_key("tenant-b"),
            )
            .await
            .unwrap();
        let fair = deq().with_concurrency_limit_per_key(1);
        let first = q.dequeue("jobs", fair.clone()).await.unwrap().unwrap();
        assert_eq!(first.id, first_high);
        let second = q.dequeue("jobs", fair.clone()).await.unwrap().unwrap();
        assert_eq!(
            second.id, normal,
            "a saturated key cannot consume the next slot"
        );
        q.ack(&first).await.unwrap();
        q.ack(&second).await.unwrap();
        assert_eq!(
            q.dequeue("jobs", fair.clone()).await.unwrap().unwrap().id,
            second_high
        );
        assert_eq!(
            q.status(low).await.unwrap().unwrap().priority,
            Priority::Low
        );
    }
}

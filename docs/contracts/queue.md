# queue — lineage: AWS SQS

Background jobs.

## Lineage

Verbs and semantics mirror **AWS SQS** (`SendMessage`, `ReceiveMessage` long-poll, `DeleteMessage`,
`ChangeMessageVisibility`, redrive policy / dead-letter queue, `MessageDeduplicationId`), with
**beanstalkd**'s `touch` borrowed for lease extension. Method names are Rust-idiomatic
(`enqueue`/`dequeue`/`ack`/`nack`/`heartbeat`); behavior matches the lineage. The default backend is
Postgres; the contract is the lowest common denominator that Postgres **and** SQS can both honor, so an
SQS-backed implementation stays a drop-in later.

> **AT-LEAST-ONCE delivery. Your consumer MUST be idempotent.** A job can be delivered more than once —
> after a lease expiry, a worker crash, a heartbeat that lost a race, or a network blip between `ack` and
> commit. This is not an edge case; design for it. Forge does **not** offer exactly-once and does not
> approximate it. (Restated under *Delivery / consistency guarantees* — it is the one thing that breaks apps.)

## Trait (Rust sketch — directional; this doc wins on conflict)

```rust
#[async_trait]
pub trait Queue: Send + Sync {
    /// SQS SendMessage. Returns the assigned JobId. Idempotent within the
    /// dedup window when `opts.dedup_id` is set (see Semantics).
    async fn enqueue(&self, queue: &str, payload: Bytes, opts: EnqueueOpts)
        -> Result<JobId, ForgeError>;

    /// SQS ReceiveMessage (long-poll). Leases at most one job for
    /// `visibility_timeout`. `None` if none became available within `wait`.
    async fn dequeue(&self, queue: &str, opts: DequeueOpts)
        -> Result<Option<Job>, ForgeError>;

    /// SQS DeleteMessage. Permanently removes the job. Success is idempotent:
    /// acking an already-acked or expired-then-reclaimed lease is not an error.
    async fn ack(&self, job: &Job) -> Result<(), ForgeError>;

    /// SQS ChangeMessageVisibility(0) for immediate retry, or to `opts.retry_in`.
    /// Counts as a failed delivery: the redelivery increments attempts and may dead-letter.
    async fn nack(&self, job: &Job, opts: NackOpts) -> Result<(), ForgeError>;

    /// SQS ChangeMessageVisibility(extend) / beanstalkd touch. Pushes the lease
    /// deadline out by one `visibility_timeout` from now. Precondition error if
    /// the lease is already lost.
    async fn heartbeat(&self, job: &Job) -> Result<(), ForgeError>;

    /// SQS GetQueueAttributes: approximate message counts for `queue`. Non-locking,
    /// point-in-time. Accepts a `"<queue>.dlq"` name for dead-letter backlogs.
    async fn depth(&self, queue: &str) -> Result<QueueDepth, ForgeError>;

    /// Managed consume loop: bounded concurrency, auto-heartbeat, graceful
    /// shutdown, panic => nack. See Worker.
    fn worker(&self, queue: &str) -> WorkerBuilder;
}

/// SQS ApproximateNumberOfMessages{,NotVisible,Delayed}. Estimates, not snapshots.
#[non_exhaustive]
pub struct QueueDepth {
    /// Available for immediate delivery, incl. lapsed-but-unreclaimed leases.
    pub visible: u64,
    /// Leased and not past the visibility deadline.
    pub in_flight: u64,
    /// Enqueued with a delay and not yet due.
    pub delayed: u64,
}

#[non_exhaustive]
pub struct EnqueueOpts {
    /// SQS DelaySeconds. Job invisible until now + delay. Default 0.
    pub delay: Duration,
    /// SQS redrive maxReceiveCount. Deliveries before dead-letter. Default 5.
    pub max_attempts: u32,
    /// Retry backoff for redeliveries. Default exponential + jitter.
    pub backoff: Backoff,
    /// SQS MessageDeduplicationId. Dedups enqueues per-queue within the
    /// dedup window (default 5 min). `None` = no dedup.
    pub dedup_id: Option<String>,
}

#[non_exhaustive]
pub struct DequeueOpts {
    /// SQS long-poll WaitTimeSeconds. Clamped to <= 20s. Default 20s.
    pub wait: Duration,
    /// SQS VisibilityTimeout = lease duration. 0 < t <= 12h. Default 30s.
    pub visibility_timeout: Duration,
}

#[non_exhaustive]
pub struct NackOpts {
    /// None => immediate retry (ChangeMessageVisibility(0)).
    /// Some(d) => retry no earlier than now + d.
    pub retry_in: Option<Duration>,
}

/// Exponential by default: base 1s, factor 2^(attempt-1), capped, ±25% jitter,
/// ms precision. (Port of forge-core calculate_backoff; see Semantics.)
#[non_exhaustive]
pub enum Backoff { Fixed(Duration), Linear(Duration), Exponential { base: Duration, cap: Duration } }

#[non_exhaustive]
pub struct Job {
    pub id: JobId,
    pub queue: String,
    pub payload: Bytes,
    /// 1 on first delivery; N on the Nth delivery. See "do not pre-increment".
    pub attempt: u32,
    pub max_attempts: u32,
    /// Lease deadline; refresh with `heartbeat` before this passes.
    pub leased_until: SystemTime,
    /* opaque lease fence (worker_id, attempts) — not public surface */
}
```

## Semantics

**State machine:** `available --dequeue--> leased --ack--> done`. From `leased`, an `ack` failure
(`nack`, or the lease lapsing without `ack`/`heartbeat`) returns the job to `available` for **redelivery**,
incrementing `attempts`; when the failed delivery's count reaches `max_attempts` the job is **re-homed**
to the `"<queue>.dlq"` dead-letter queue as a fresh `available` job (attempts reset) — *except* a job that
is already in a `*.dlq` queue, which instead becomes terminal **`dead`** (attempts pinned) so exhaustion
there never chains into `.dlq.dlq`. `done` and `dead` are terminal; a `.dlq` queue is itself an ordinary,
consumable queue (dequeue/depth accept `.dlq` names; `enqueue` rejects them).

| Op | Behavior |
|----|----------|
| `enqueue` | Inserts one job in state **available**, visible at `now + delay`. Returns its `JobId`. With `dedup_id`: if a job with the same `(queue, dedup_id)` was enqueued within the dedup window, no new job is inserted and the **existing** `JobId` is returned — this is success, not an error (SQS FIFO precedent). Dedup is scoped **per queue**: the same `dedup_id` in two queues is two jobs. |
| `dequeue` | Long-poll up to `opts.wait` (<= 20s). Atomically claims one available, due job via `FOR UPDATE SKIP LOCKED`, moves it **available -> leased**, sets `leased_until = now + visibility_timeout`, stamps the worker fence, and returns it. `Ok(None)` if nothing became available before `wait` elapsed. **Claiming does not increment `attempts`** — a delivery *is* an attempt; `attempt` is read off the row (see Deviations). |
| `ack` | **leased -> done.** Removes the job from the working set. Idempotent: acking a job whose lease already expired and was reclaimed by another worker is **not** an error (returns `Ok(())`); the reclaiming worker's later `ack` wins. This is the at-least-once seam — `ack` does **not** mean "no one else ran this." Idempotent consumers make that safe. |
| `nack` | Marks the current delivery failed. `retry_in = None` -> **leased -> available** immediately (`ChangeMessageVisibility(0)`). `retry_in = Some(d)` -> available at `now + d`. The redelivery **increments `attempts`**; if the incremented count reaches `max_attempts`, the job goes to the dead-letter queue instead of available (see Visibility / leasing / retry / DLQ). |
| `heartbeat` | Extends the lease to `now + visibility_timeout`. Fenced by `(worker_id, attempts)`: if the lease was already lost (expired and reclaimed), returns `Precondition` — stop work, another worker owns it now. beanstalkd `touch` semantics. |
| `depth` | Returns approximate `{visible, in_flight, delayed}` counts for the queue in one query, excluding terminal (`done`) jobs. **No locking, no leasing** — unlike polling the queue, it never makes a job invisible or bumps `attempts`, so it is the correct way to gauge a DLQ backlog (`depth("orders.dlq")`). Counts are estimates: a concurrent enqueue/lease may not be reflected. A lapsed-but-unreclaimed lease counts as `visible` (the next `dequeue` will hand it out). |
| `worker` | Returns a `WorkerBuilder`. The built worker runs a managed loop: dequeues up to `concurrency` jobs, runs the handler, **auto-heartbeats** at ~`visibility_timeout / 3` while the handler runs, `ack`s on `Ok`, `nack`s on `Err`, and **`nack`s on panic** (caught at the task boundary, never crashes the loop). On shutdown it stops dequeuing and waits (bounded by a grace period) for in-flight handlers, heartbeating them until they finish or the grace expires. |

### Backoff (port of `forge-core::RetryConfig::calculate_backoff`)

Default redelivery delay for attempt *n* (1-based): `base * 2^(n-1)`, capped at `cap`, then **±25%
jitter**, at **millisecond** precision. `base = 1s`, `cap = 300s` by default. Jitter desynchronizes a
fleet retrying after a shared upstream outage so they don't re-thunder the recovering dependency. Overflow
is saturating (no panic at high attempt counts). `Fixed`/`Linear` follow the same jitter + cap rule.

## Delivery / consistency guarantees

- **At-least-once.** Every enqueued job is delivered **one or more** times until acked or dead-lettered.
  Duplicates are normal: lease expiry under a slow handler, a worker dying between handler success and
  `ack`, an `ack` that commits after the lease already lapsed. **Consumers MUST be idempotent** — key
  side effects on `job.id` (or a business key), and treat re-delivery as expected, not exceptional.
- **No exactly-once.** Not offered, not approximated. Do not build on the absence of duplicates.
- **No silent loss.** A job leaves the working set only via `ack` (success) or by exhausting attempts into
  the DLQ. It is never dropped. A crashed worker's lease expires and the job is redelivered.
- **`dedup_id` de-duplicates *enqueues*, not *deliveries*.** It collapses repeated sends within the window;
  it does nothing about a single job being delivered twice. Idempotent consumers remain mandatory.

## Ordering

**No ordering guarantee.** Jobs are not delivered in enqueue order. Roughly-FIFO emerges from
`ORDER BY scheduled_at` under light load, but it is **best-effort and never promised** — `SKIP LOCKED`,
concurrent workers, delays, and redeliveries all reorder freely. Do not encode ordering assumptions.
Strict FIFO is a non-goal.

## Visibility / leasing / retry / DLQ

- **Lease = visibility timeout.** `dequeue` leases a job for `visibility_timeout` (default 30s, range
  `0 < t <= 12h`). While leased the job is **invisible** to other workers. `heartbeat` extends the
  deadline; nothing else touches it.
- **Lease expiry => redelivery.** If `leased_until` passes with no `ack` or `heartbeat`, the job returns to
  **available** and is redelivered. **This is where `attempts` increments** — the redelivery is the next
  attempt. The expired worker's stale `ack`/`heartbeat` then fails its `(worker_id, attempts)` fence and is
  a no-op / `Precondition`.
- **Attempts increment on redelivery, never on claim.** First delivery is attempt 1 (`attempts` column = 0
  at rest, surfaced as `attempt = 1`). The increment happens on redelivery — explicit `nack` or
  lease-expiry reclaim — not on the initial claim. A job is dead-lettered once a delivery fails and the
  count reaches `max_attempts` (default 5 = SQS `maxReceiveCount`).
- **Dead-letter queue.** Exhausted jobs move to `"<queue>.dlq"` (SQS redrive policy). They are **never
  silently dropped**. The DLQ is an ordinary queue (consumable / inspectable). Automatic redrive *back* to
  the source queue is a non-goal for v1.
- **Backoff between retries.** Governed by `EnqueueOpts.backoff` (default exponential + jitter, above).

## Limits

| Limit | Value | Source / rationale |
|-------|-------|--------------------|
| Payload size | **<= 256 KiB** | SQS `SendMessage` max. Enforced loudly so a future SQS backend stays honest. Over => `Limit`. |
| `dequeue.wait` | **<= 20s** | SQS long-poll max. Larger values are clamped to 20s (not an error). Default 20s. |
| `visibility_timeout` | **0 < t <= 12h** | SQS visibility max. Out of range => `Invalid`. Default 30s. |
| `dedup_id` length | <= 128 chars | SQS dedup id limit. Over => `Limit`. |
| Dedup window | default 5 min, configurable | SQS FIFO precedent. Scoped per `(queue, dedup_id)`. |
| `max_attempts` | 1 ..= 1000 | SQS `maxReceiveCount` ceiling. Default 5. |
| `delay` | 0 ..= 15 min | SQS `DelaySeconds` max. Out of range => `Invalid`. |
| Queue name | non-empty; ASCII `[A-Za-z0-9_.-]`; cannot end in `.dlq` (reserved) | Keeps name -> DLQ name mapping unambiguous. |

Backoff/lease arithmetic is millisecond precision.

## Error mapping

| Condition | Variant | Retryable |
|-----------|---------|-----------|
| Backend (Postgres) unreachable / pool timeout / connection dropped | `Unavailable` | yes |
| `nack`/`heartbeat` on an unknown `JobId` | `NotFound` | no |
| `heartbeat`/`ack`/`nack` after the lease was lost (fence mismatch — another worker reclaimed it) | `Precondition` | no — re-fetch; stop work on this job |
| Payload > 256 KiB; `dedup_id` too long | `Limit` | no |
| `visibility_timeout`, `delay`, `max_attempts` out of range; empty or reserved (`.dlq`) queue name | `Invalid` | no — caller bug |
| Vendor/SDK surfaced error (non-Postgres backend) | `Backend` (carries retryability flag) | per flag |
| Misconfiguration (bad DSN, missing migration, malformed queue config) at `Forge::init()` | `Config` | no — init only |

Notes: there is no queue registry — queues are implicit in the rows — so `dequeue` on a name with no due
jobs returns `Ok(None)`, never `NotFound`. A `dedup_id` hit is **not** an error — `enqueue` returns the
existing `JobId` with `Ok`. An already-acked or already-expired `ack` is **not** an error either (returns
`Ok(())`, idempotent — never `NotFound` or `Precondition`); only `nack`/`heartbeat`, which need a live fenced
row, yield `Precondition` (fence lost) or `NotFound` (no such job). `Config` is init-time only: a misconfigured queue
fails inside `Forge::init()`, never lazily on first `enqueue`. Error messages never contain payload bytes
or `dedup_id` values.

## Deviations from lineage

- **No pre-increment of attempts on claim.** SQS increments `ApproximateReceiveCount` on every receive.
  Forge counts a delivery as an attempt but **increments only on redelivery**, not on the initial claim.
  Rationale: the old runtime bumped `attempts` at claim time and then needed a `release_claim` "undo dance"
  whenever a worker claimed but couldn't run (e.g. concurrency permit exhausted), otherwise live work
  silently burned attempts toward the DLQ. Counting the delivery itself and incrementing on redelivery
  removes the undo path. `attempt` on first delivery is therefore 1 with `attempts` at rest = 0.
- **Per-queue dedup scope.** SQS dedup is already per FIFO queue; the old Postgres index keyed dedup on the
  id alone, so the same id across two queues collided. The uniqueness index **bakes the queue name in** —
  `(queue, dedup_id)` — so identical ids in different queues are independent jobs.
- **Bounded long-poll, clamped not rejected.** `wait > 20s` is clamped to 20s (SQS would reject); chosen so
  an over-eager caller degrades gracefully instead of erroring.
- **DLQ name is derived, not configured.** SQS attaches an arbitrary redrive target ARN. Forge fixes it to
  `"<queue>.dlq"` for a one-obvious-way mapping; redrive-back is post-v1.
- **Lease fence is `(worker_id, attempts)`, exposed only as behavior.** SQS uses opaque receipt handles.
  Forge's fence delivers the same "stale handle can't mutate" guarantee on Postgres without a receipt type
  on the public surface.

## Observability

One span per operation, emitted automatically. Span name `forge.queue.<op>`
(`enqueue` / `dequeue` / `ack` / `nack` / `heartbeat`), plus `forge.queue.worker.run` for a worker tick.

Fields (never payload bytes, never `dedup_id` values):

| Field | On |
|-------|----|
| `queue` | all |
| `job.id` | dequeue / ack / nack / heartbeat |
| `job.attempt`, `job.max_attempts` | dequeue / nack |
| `outcome` = `ack` \| `nack` \| `dead_letter` \| `lease_expired` | worker tick / nack |
| `visibility_timeout_ms`, `wait_ms` | dequeue |
| `retry_in_ms` | nack |
| `dedup_hit` (bool) | enqueue |
| `payload_bytes` (length only) | enqueue |
| `error.variant` | any failure |

A dead-letter transition emits a distinct event (`outcome = dead_letter`) so exhaustion is alertable.

## Non-goals

Deliberately **not** provided (some post-v1):

- **Exactly-once delivery.** At-least-once only; consumers are idempotent. No de-duplication of deliveries.
- **Strict FIFO / total ordering.** Best-effort only, never promised.
- **Priorities.** No priority lane in v1; post-v1.
- **DLQ redrive-back.** No automatic move of dead-lettered jobs back to the source queue; post-v1. (The
  DLQ is consumable manually as an ordinary queue.)
- **Fan-out / pub-sub.** One job, one successful consumer. No topic broadcast; post-v1.
- **Cross-queue transactions.** No atomic enqueue across multiple queues. (Atomic enqueue *within the
  caller's own DB transaction* on the Postgres backend is an implementation affordance, not a contract.)
- **Cancellation, job introspection API, scheduled/cron jobs.** Out of scope; scheduling is the `schedule`
  primitive.

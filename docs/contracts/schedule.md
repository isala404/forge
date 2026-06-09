# schedule — lineage: cron (5-field) + Unix `at` + Kubernetes CronJob

Time-triggered enqueues. A thin layer over `queue`.

## Lineage

Recurring schedules use **5-field cron** syntax (`min hour dom mon dow`), evaluated in
**UTC** — the **Kubernetes CronJob** precedent. One-shot schedules mirror **Unix `at`**:
a single fire at an absolute instant. Missed-tick handling follows CronJob's
`startingDeadlineSeconds` — a tick missed while the system was down fires once on
recovery if it is still within the deadline, else it is skipped and logged.

Crucially, schedule **does not deliver jobs**. Every tick *enqueues* into the `queue`
primitive. All of `queue`'s machinery — leasing, retry/backoff, the dead-letter queue,
at-least-once delivery — is **inherited**, not re-implemented.

> **A scheduled job is delivered AT-LEAST-ONCE.** schedule guarantees *at most one
> enqueue per tick* across all replicas, but the enqueued job then rides `queue`, which
> is at-least-once. So a single tick can still produce more than one *delivery* to your
> consumer (lease expiry, worker crash, redelivery). **Your consumer MUST be
> idempotent.** This is the queue contract showing through, not a schedule bug.

## Trait (Rust sketch — directional; this doc wins on conflict)

```rust
#[async_trait]
pub trait Schedule: Send + Sync {
    /// Register or replace a recurring schedule. Upsert by `name`: re-registering
    /// the same name replaces `expr`, `queue`, and `payload` atomically. `expr` is
    /// validated as 5-field cron at registration; invalid => `Invalid`.
    async fn cron(&self, name: &str, expr: &str, queue: &str, payload: Bytes)
        -> Result<(), ForgeError>;

    /// Unix `at`: schedule a single enqueue at absolute `when` (a `SystemTime`).
    /// Returns the `queue` JobId the eventual enqueue will carry — see Semantics.
    async fn at(&self, when: SystemTime, queue: &str, payload: Bytes)
        -> Result<JobId, ForgeError>;

    /// Remove a schedule by name. Returns `true` if one was removed, `false` if no
    /// schedule had that name. Not an error to cancel nothing.
    async fn cancel(&self, name: &str) -> Result<bool, ForgeError>;

    /// All registered schedules — recurring crons and pending one-shots (the `kind`
    /// field distinguishes them). A one-shot disappears once it has fired.
    async fn list(&self) -> Result<Vec<ScheduleInfo>, ForgeError>;
}

pub enum ScheduleKind { Cron(String), At } // Cron carries the 5-field expression

#[non_exhaustive]
pub struct ScheduleInfo {
    pub name: String,
    pub kind: ScheduleKind,            // Cron(expr) | At (a pending one-shot)
    pub queue: String,                 // target queue for the enqueue
    pub next_run: SystemTime,          // next time this fires
    pub last_run: Option<SystemTime>,  // last tick that fired; None if never
}
```

`name` identifies a recurring schedule and is the upsert key. `payload` is opaque bytes,
handed to `queue` verbatim. `JobId` is the `queue` primitive's id type.

## Semantics

| op | behavior |
|----|----------|
| `cron` | Upsert by `name`. Inserts the schedule, or replaces an existing one's `expr`/`queue`/`payload` (one atomic write — re-registering the same name does **not** create a second schedule). `expr` is parsed and validated as 5-field cron at this call; a bad expression is rejected here, never silently at tick time. `next_run` is recomputed from the new `expr`. Returns `Ok(())`. |
| `at` | Schedules exactly one enqueue at `when`. A `when` already in the past fires on the next tick if within the missed-tick grace (below), else is skipped + logged — the same policy as a missed cron tick. Returns the `JobId` the eventual enqueue will carry, so the caller can correlate / inspect / `ack` it via `queue` once it lands. The job becomes visible in `queue` only when the tick fires, not at `at` call time (see resolution). |
| `cancel` | Removes the recurring schedule named `name`. Returns `true` if one existed and was removed, `false` if none did. Cancelling an unknown name is **success** (`Ok(false)`), not `NotFound`. A tick already enqueued before `cancel` is **not** recalled — it lives in `queue` now and runs to completion there. `cancel` does **not** target one-shots created by `at`. |
| `list` | Returns every registered recurring schedule with its `next_run`/`last_run`. One-shot `at` jobs are excluded — once fired they are ordinary `queue` jobs, and before firing they are not "schedules" in the cron sense. Empty vec if none. |

A scheduled enqueue is indistinguishable, once landed, from any other `queue` job: same
payload, same retry/backoff/DLQ rules. schedule's entire job is *deciding when to call
`queue.enqueue`*, exactly once per tick.

## Delivery / consistency guarantees

- **Exactly one ENQUEUE per tick, fleet-wide.** Across every replica running the ticker
  (`forge.run_scheduler()`), a given tick produces **one** `queue.enqueue`. Each due row
  is claimed with `FOR UPDATE SKIP LOCKED` and, in the **same transaction**, the job is
  inserted into `queue` and the row's `next_run` advanced (or the one-shot deleted) — so a
  concurrent replica sees the row already claimed/advanced and never re-fires it. There is
  no leader election: every replica may run the ticker safely. Because the claim and the
  enqueue commit together, a crash between them loses nothing.
- **At-least-once DELIVERY, inherited from `queue`.** One enqueue per tick is **not**
  one delivery per tick. The enqueued job is delivered at-least-once by `queue`.
  Consumers MUST be idempotent (restated from Lineage — it is the thing that breaks apps).
- **No silent loss within the deadline.** A tick that should fire while any replica's
  ticker is running fires. A tick missed because *all* tickers were down fires once on
  recovery if it is still within the missed-tick deadline (below), else it is skipped and
  logged — never fired twice.

## Ordering

No ordering guarantee on the *delivery* side — that is `queue`'s domain (`queue` is
explicitly not FIFO). On the *enqueue* side, ticks for a single schedule are enqueued in
chronological tick order, but once in `queue` they reorder freely against each other and
against unrelated jobs. Two schedules whose ticks coincide enqueue in an unspecified order.
Do not encode ordering assumptions.

## Tick resolution / missed-tick policy

- **Minimum resolution: 1 minute.** Cron's smallest field is the minute; the ticker
  evaluates schedules on a 1-minute boundary. There is no sub-minute scheduling.
- **UTC only.** All `expr` and all `at`/`when` instants are evaluated in UTC. No local
  time, no DST, no `TZ=` prefix (post-v1; see Non-goals).
- **Tick instant.** A tick fires when wall-clock UTC crosses the cron boundary the
  expression names. Tick timestamps are stored at **TIMESTAMPTZ, seconds precision**;
  `last_run`/`next_run` are seconds-precise.
- **Missed-tick (CronJob `startingDeadlineSeconds`).** If a scheduled tick's instant
  passed while no ticker was running to fire it, on recovery it fires **once** iff it is
  **less than 1 hour late**; if it is ≥ 1h late it is **skipped and logged**.
- **At most one missed tick fires — never backfilled.** Only the **single most-recent**
  missed tick within the deadline is fired. If many ticks were missed during a long
  outage, the older ones are dropped (logged), not replayed. schedule does **not** catch
  up a backlog. This is deliberate — see Deviations and Non-goals.
- **No overlap policy.** schedule fires every due tick on time; it does not check whether
  a prior tick's job is still being consumed. Overlap is the consumer's problem in v1
  (k8s `concurrencyPolicy` is post-v1).

## Limits

| thing | limit | over-limit error |
|-------|-------|------------------|
| `name` | non-empty, ≤ 256 bytes, UTF-8 | `Invalid` (empty), `Limit` (over) |
| `expr` | valid 5-field cron; UTC; min resolution 1 min | `Invalid` |
| `queue` | a valid `queue` name (per `queue` contract) | `Invalid` |
| `payload` | ≤ 256 KiB (the `queue` payload cap) | `Limit` |
| `at` / `when` | strictly in the future (UTC); ≤ ~100-year ceiling | `Invalid` (past), `Limit` (over ceiling) |
| missed-tick lateness | fires only if < 1h late | skipped + logged (not an error) |
| tick resolution | ≥ 60s | sub-minute `expr` rejected as `Invalid` |

Payload and queue-name limits are pass-throughs to `queue` — schedule validates them up
front so a bad payload fails at `cron`/`at` time, not silently at tick time. The
~100-year ceiling on `at.when` matches the `kv` TTL ceiling rationale: an absolute time a
century out is effectively always a bug, and a fixed ceiling keeps backends in agreement.

## Error mapping

| condition | variant | retryable |
|-----------|---------|-----------|
| `cancel` on an unknown name | returns `false`, not an error | — |
| `list` with no schedules | returns empty vec, not an error | — |
| malformed/sub-minute cron `expr`; empty `name`; invalid target queue name | `Invalid` | no — caller bug |
| `name` over 256 B; `payload` over 256 KiB | `Limit` | no |
| transient backend outage (pool timeout, dropped conn, `08xxx`/`57014`) | `Unavailable` | yes |
| failure to enqueue at tick time (queue backend unavailable) | retried by the ticker; not surfaced to a caller | — (internal retry) |
| other vendor/SDK error | `Backend` (carries retryability flag) | per flag |
| misconfiguration (bad DSN, missing migration) at `Forge::init()` | `Config` | no — init only |

`NotFound` and `Precondition` are **never** produced by this surface — `cancel` reports
absence as `Ok(false)`, matching the kv/queue "absence on a read path is not an error" rule,
and there is no lease/fence to lose (the ticker's row claim is internal). Errors at tick time
(queue temporarily unavailable) are the ticker's to retry on its next pass, not a `cron`/`at`
caller's to handle. Error messages never contain `payload` bytes.

## Deviations from lineage

- **5-field cron, UTC only.** Many cron implementations support a 6th seconds field and/or
  a timezone (Quartz, some Go libs, k8s `CronJob.spec.timeZone`). Forge fixes 5 fields and
  UTC so the Postgres ticker and any future backend agree on one unambiguous evaluation.
  Timezone-aware cron is post-v1.
- **Missed tick fires AT MOST once — no backfill.** k8s CronJob with
  `startingDeadlineSeconds` will also start at most one missed run, but some cron tools
  replay every missed interval. Forge fires only the single most-recent missed tick within
  the 1h deadline and drops (logs) the rest. Catching up a backlog of identical jobs is
  almost always wrong and amplifies a recovering system's load.
- **schedule owns no delivery semantics.** Unlike a self-contained cron daemon that runs
  the job itself, Forge's schedule only calls `queue.enqueue`. Retry, backoff, DLQ, and
  at-least-once all belong to `queue`. schedule's sole contract is *one enqueue per tick,
  fleet-wide*.
- **`at` returns the future `JobId`.** Unix `at` returns a job number for `atq`/`atrm`;
  Forge returns the `queue` `JobId` the tick will assign, so the one-shot is correlatable
  and inspectable through `queue` (not through schedule) once it lands.
- **`cron` is upsert, not create-only.** Many schedulers error on a duplicate name. Forge
  treats `cron(name, ...)` as register-or-replace so re-deploying an app with the same
  schedule definition is idempotent rather than a conflict.
- **Per-row tick claim, no leader.** A typical multi-replica cron either needs an external
  coordinator or risks N duplicate fires. Forge instead claims each due row with `FOR UPDATE
  SKIP LOCKED` and advances/enqueues it in one transaction, so every replica can run the
  ticker and a tick still fires exactly once — no leader election, no extra infrastructure.
- **`at` takes a `SystemTime`, not a `chrono` type.** The rest of Forge's public surface
  uses `std::time`, so `at`/`ScheduleInfo` do too; UTC cron math happens internally.

## Observability

Span `forge.schedule.<op>` (`forge.schedule.cron`, `forge.schedule.at`,
`forge.schedule.cancel`, `forge.schedule.list`), plus `forge.schedule.tick` for each
ticker evaluation that fires (or skips) a schedule. Fields:

| field | notes |
|-------|-------|
| `schedule.op` | operation name |
| `schedule.name` | schedule name — a low-cardinality, non-secret identifier (emitted like `queue`) |
| `schedule.queue` | target queue name |
| `schedule.fired` | `tick`: number of schedules enqueued this pass |
| `schedule.count` | `list`: number of schedules returned |
| `schedule.outcome` | `ok` / error variant |

A missed tick past the 1h grace is logged at WARN (with the schedule name and lateness) so a
dropped run is alertable. `payload` contents are **never** emitted — only sizes, counts, and
the (non-secret) schedule and queue names.

## Non-goals

Deliberately **not** provided (some post-v1):

- **Timezone-aware / DST-aware cron.** UTC only in v1. `TZ` prefixes and
  `CronJob.spec.timeZone`-style scheduling are post-v1.
- **Sub-minute (per-second) resolution.** 1-minute minimum, matching cron's smallest field.
- **Overlap / concurrency policy.** No `concurrencyPolicy` equivalent (Allow/Forbid/Replace)
  in v1 — a tick fires regardless of whether the prior tick's job is still in flight. Overlap
  is the consumer's concern; post-v1.
- **Backfill of missed ticks.** Only the single most-recent missed tick fires (within 1h);
  a backlog is never replayed.
- **Exactly-once delivery.** Inherited at-least-once from `queue`; consumers are idempotent.
  schedule guarantees one *enqueue* per tick, not one *delivery*.
- **Recalling an already-enqueued tick.** `cancel` stops future ticks; a job already handed
  to `queue` runs there. Cancel that via `queue` if needed.
- **A general timer / reminder API, calendar recurrence (RRULE), or jittered schedules.**
  Out of scope; this is cron + `at`, nothing more.

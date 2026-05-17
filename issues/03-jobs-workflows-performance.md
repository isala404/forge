# Job Queue, Workflow Executor, Cron, Daemon — Performance & Scalability Audit

Audit scope: `crates/forge-runtime/src/jobs/`, `workflow/`, `cron/`, `daemon/`, plus the system migrations in `crates/forge-runtime/migrations/system/` and workflow context code in `crates/forge-core/src/workflow/context.rs`.

Severity scale: **P0** (data loss / correctness / cannot scale past a single small node), **P1** (causes meaningful resource waste, latency spikes, or starvation at modest scale), **P2** (matters for production polish).

---

## 1. Durable-sleep wakeup has no NOTIFY path; precision is bounded by `poll_interval` and the partial index excludes sleeping rows

- **Where:** `crates/forge-runtime/src/workflow/scheduler.rs:165-196` (poll), `crates/forge-runtime/migrations/system/v001_initial.sql:166-168` (index), `v005_workflow_status.sql:17-19` (status split), `crates/forge-core/src/workflow/context.rs:801-815` (`set_wake_at`).
- **Severity:** P0.
- **Concern:** The CLAUDE.md and code comments claim durable sleeps have a sub-50ms wakeup. There is no trigger that fires `pg_notify('forge_workflow_wakeup', ...)` on `UPDATE forge_workflow_runs SET wake_at = ...` or on `wake_at <= NOW()`. The only NOTIFY producers on that channel are (a) inserts into `forge_workflow_events` and (b) `cancel_requested_at` flipping non-null. A workflow that called `ctx.sleep(Duration::from_secs(30))` will wake up *only* when the next `interval.tick()` of `WorkflowSchedulerConfig::poll_interval` (default **1 s**) elapses, then claims via `wake_at <= NOW()`. Worse, the partial index `idx_forge_workflow_runs_wake` filters `WHERE status = 'waiting' AND wake_at IS NOT NULL`, but `set_wake_at` sets `status = 'sleeping'` (v005 split). The poll query at scheduler.rs:170 (`status = 'sleeping' AND wake_at <= NOW()`) therefore does a seq scan — fine at 10k rows, catastrophic at 10M sleeping workflow runs (a perfectly normal long-tail SaaS scenario: "renew subscription in 30 days").
- **Fix sketch:**
  1. Replace `idx_forge_workflow_runs_wake` with two indexes: `ON forge_workflow_runs(wake_at) WHERE status = 'sleeping'` and `ON forge_workflow_runs(event_timeout_at) WHERE status = 'waiting'`.
  2. Either accept the polling latency floor and document it (raise `poll_interval` to 5s with an explicit note), or implement a true wakeup table: insert `(workflow_run_id, wake_at)` into `forge_workflow_wakeups` on sleep, have a single dedicated tokio task that does `SELECT min(wake_at) FROM ...` and sleeps until then (woken by NOTIFY on insert with an earlier `wake_at`), then claims and dispatches resume jobs. That is the "jobs/wakeup table" alluded to in the audit prompt — it doesn't exist yet.

## 2. Workflow scheduler poll query lacks `FOR UPDATE SKIP LOCKED`; multi-node deployments double-dispatch resume jobs

- **Where:** `crates/forge-runtime/src/workflow/scheduler.rs:165-181` (SELECT), `334-392` (`claim_and_resume`).
- **Severity:** P1.
- **Concern:** Every running gateway node executes `WorkflowScheduler::run`. It's not leader-gated for the wakeup path — only `cleanup_consumed_events` is. Each node SELECTs candidate rows, then races on `UPDATE ... WHERE status IN ('sleeping','waiting')` inside `claim_and_resume`. The losers harmlessly do nothing, but at N nodes you do N×batch_size SELECTs + N-1 wasted UPDATE round-trips per tick, multiplied by every wake_at expiry. With 100 ready workflows and 10 nodes this is 1000 SELECT round-trips per second of wall-clock, all racing the same rows.
- **Fix sketch:** Either gate `process_ready_workflows` behind `is_leader()` (matches cron's model), or change the SELECT to `FOR UPDATE SKIP LOCKED` and partition the workload by hash. Leader-only is simpler and matches the cron pattern.

## 3. Worker NOTIFY listener has no reconnect; a listener-conn drop silently degrades the cluster to poll-only

- **Where:** `crates/forge-runtime/src/jobs/worker.rs:153-180`.
- **Severity:** P1.
- **Concern:** The listener task connects once via `PgListener::connect_with`; if the connection dies (network blip, PG failover, idle timeout), `listener.recv()` returns an error, the `select!` arm fires but does nothing useful, and on the next iteration the same dead listener is polled. Workers fall back to the `poll_interval` default (5 s), so dispatch latency silently jumps 100× and operators have no signal. Same pattern in `workflow/scheduler.rs:90-101` and worth checking.
- **Fix sketch:** Wrap the listener in a reconnect loop with exponential backoff; on reconnect, immediately `wakeup_trigger.notify_one()` so any jobs enqueued during the gap are processed. Increment a counter metric on each reconnect so operators see flapping.

## 4. Polling overhead: workers/schedulers/cron all run forever even when leadership and queue are empty; defaults are not tunable from `forge.toml`

- **Where:** `worker.rs:50` (`poll_interval: 5s`), `workflow/scheduler.rs:30` (`poll_interval: 1s`), `cron/scheduler.rs:133` (`poll_interval: 1s`), `daemon/runner.rs:30-36` (`health_check: 30s`, `heartbeat: 10s`).
- **Severity:** P1.
- **Concern:** With 10 nodes each running the workflow scheduler, that's 10 SELECTs/sec against `forge_workflow_runs` even when zero workflows exist. Cron is the same. None of the intervals appear to read from `[worker]`/`[scheduler]` config — they're hard-coded defaults in `Default` impls. Empty-queue cost dominates small-deployment baselines.
- **Fix sketch:** Surface every interval in `forge.toml` ([worker.poll_interval](), [workflow.poll_interval](), [cron.poll_interval]()). Add an adaptive backoff: when N consecutive polls find no work, double the interval up to a cap (e.g. 30 s). NOTIFY pre-empts the back-off, so dispatch latency stays low for an active queue while idle deployments cost ~nothing.

## 5. Stale-reclaim decrements `attempts` to `attempts - 1` instead of resetting to the pre-claim value; recovers correctly only if claim's increment was exactly 1

- **Where:** `crates/forge-runtime/src/jobs/queue.rs:692-722`.
- **Severity:** P1 — correctness, but the math currently happens to work.
- **Concern:** `claim()` does `attempts = attempts + 1`. `release_stale` does `attempts = attempts - 1` on reset. That's algebraically correct *today*, but it couples two queries that don't reference each other and breaks silently if someone changes the claim increment (e.g. to support multi-attempt claim batching). Also: a stale-reclaim races concurrent `start()` — the fencing in `start()` checks `attempts = $3`, which is the value *the original worker observed at claim time*. After `release_stale` decrements, a stale original worker's `start()` query no longer matches (good) but a re-claim by a new worker increments attempts again so it does match (good). The reasoning depends on those two `±1`s exactly cancelling. Brittle.
- **Fix sketch:** Drop the decrement. Treat `attempts` as a monotonic claim counter; if you want a "retries-actually-attempted" metric, track it as a separate column updated only when execution reaches `start()`. The `(worker_id, attempts)` fence in `start()` keeps working because each reclaim still increments.

## 6. Cron catch-up storms after long downtime: per-cron limit caps storm size, but cluster-wide there's no global rate-limit and catch-up jobs are dispatched on every tick

- **Where:** `crates/forge-runtime/src/cron/scheduler.rs:283-292` (catch-up called every tick), `392-458` (`handle_catch_up`).
- **Severity:** P1.
- **Concern:** With 50 crons that have `catch_up = true` and a 6-hour outage on a `* * * * * *` cron, `catch_up_limit` (per-cron) is the only ceiling. On startup, `tick()` runs `handle_catch_up` for *every* cron with `catch_up=true`, every poll iteration. Until the limit is exhausted, each tick re-queries `forge_cron_runs` for the last completed run and re-tries `between_in_tz` (potentially expensive for high-frequency crons over wide windows). And `handle_catch_up` is invoked unconditionally even when there are zero missed runs — that's `O(crons)` SELECTs per second forever.
- **Fix sketch:**
  1. Run catch-up once on leader-takeover, not on every tick. Track per-cron "caught up to" timestamp in memory.
  2. Add a global `cron.catch_up_jobs_per_tick` budget so a 100-cron deployment doesn't insert 10k jobs in one second after recovering from downtime.
  3. Make `between_in_tz` calls bounded — pass `LIMIT catch_up_limit` into the iterator, don't materialize a full `Vec<DateTime>` of missed slots first.

## 7. Daemons holding advisory locks have no failover heartbeat; leader death takes ~30s+ to release (PG-side) and there's no health observability

- **Where:** `crates/forge-runtime/src/daemon/runner.rs:333-356` (`try_become_leader` loop), `pg::LeaderElection`.
- **Severity:** P1.
- **Concern:** A daemon's leadership is bound to a dedicated PG connection holding `pg_try_advisory_lock`. When the leader node hard-fails (panic, kernel OOM, network partition), the lock releases only when PG detects the dead TCP session — `tcp_keepalives_*` defaults can mean 30s–2h. During that window, follower nodes loop with `tokio::time::sleep(Duration::from_secs(5))` and **never time out** waiting. No alert, no metric exposed for "this daemon hasn't been alive in N seconds". The `forge_daemons` table has `last_heartbeat` but I see no code path that updates it from inside the daemon loop — the column is set only on start/stop and on status transitions.
- **Fix sketch:**
  1. Spawn a heartbeat task inside the daemon loop that bumps `last_heartbeat` every `heartbeat_interval` while leader. Followers can detect a stale leader by `last_heartbeat < NOW() - 3*heartbeat_interval` and proactively try to acquire (PG-level advisory lock still gates the actual transition — safe).
  2. Expose `daemon_last_heartbeat_seconds` as a Prometheus metric.
  3. Tune PG `tcp_keepalives_idle = 30` on the leader-election connection.

## 8. Workflow step persistence write amplification: `record_step_start` then `record_step_complete` = 2 round-trips per step, plus the wake/event UPDATEs share the same connection pool as the workflow handler

- **Where:** `crates/forge-core/src/workflow/context.rs:365-454` (two UPSERTs per step), plus `workflow/executor.rs:678-705` (`save_step` UPSERT), and `set_wake_at`/`set_waiting_for_event` each do an UPDATE.
- **Severity:** P1.
- **Concern:** A workflow with 20 steps does 40+ writes against `forge_workflow_steps` plus extra UPDATEs to `forge_workflow_runs` on every suspension. At 100 concurrent workflows progressing 1 step/sec, that's 4000+ writes/sec on a single PG primary, all blocking on the *same* pool that gateway RPC handlers use. There's no batching, no `RETURNING`-piggybacked metadata, and the step start INSERT is its own round-trip even though `record_step_complete` immediately overwrites the row in most fast steps.
- **Fix sketch:**
  1. For fast steps (sub-100ms heuristic), elide `record_step_start` and write only on completion. The `is_step_started` guard is for resume — set it from `step_states` in memory, not the DB.
  2. Batch multiple step completions into one INSERT inside a workflow tick when they happen close together (rare but possible inside `parallel_steps`).
  3. Workload-isolate workflow writes from gateway reads. The CLAUDE.md says "workload isolation belongs at the worker level" but the single pool means a write storm on workflow steps stalls RPC queries. Either give workers a dedicated pool subset (semaphore on a child pool) or document the cap.

## 9. `validate_resume` failure marks the run `failed` (terminal); operator has no "blocked" state to unblock by re-deploying the right version

- **Where:** `crates/forge-runtime/src/workflow/executor.rs:297-309`, `v005_workflow_status.sql:7-15` (collapsed `BlockedMissingVersion` / `BlockedSignatureMismatch` into `failed`).
- **Severity:** P1.
- **Concern:** CLAUDE.md still claims `WorkflowStatus::BlockedSignatureMismatch` exists and `/_api/ready` reports unhealthy on blocked runs. v005 collapsed those into terminal `failed`. That means a routine deployment that bumps a workflow signature (and forgets to mark the old version `deprecated` with a long-tail of in-flight runs) marks every in-flight run as permanently failed — no recovery path. The `WorkflowStatus` enum and the `is_terminal()` check both treat this as game-over. This is a footgun against the framework's own "durable workflows" pitch.
- **Fix sketch:** Restore a non-terminal `Blocked` status (single variant with a `blocking_reason` text column). `complete_workflow`/`fail_workflow` reject transitions from `Blocked`. The scheduler skips Blocked rows. A `forge workflow unblock <run_id>` CLI/operator API transitions them to `pending` once the matching version is deployed. Also fix `/_api/ready` to actually query Blocked counts (today it has nothing to query against).

## 10. Compensation handlers are lost across restart and `cancel()` silently records "manual remediation required" — operationally surprising

- **Where:** `crates/forge-runtime/src/workflow/executor.rs:326-344`, `forge-core/src/workflow/context.rs:617-622`.
- **Severity:** P1 — known limitation in the comment but the failure mode is non-obvious.
- **Concern:** `compensation_state: Arc<RwLock<HashMap<Uuid, CompensationState>>>` lives in memory on the node that ran the workflow. A workflow that completes 3 of 5 steps, then suspends on `wait_for_event` for 10 days, then receives a cancel — will hit the "handlers lost" branch on any restart. The whole saga story is undermined. The doc comment names this honestly but the API doesn't surface "this workflow's compensation will be uncompensable across restart" to users at design time.
- **Fix sketch:** Either:
  - Require compensation handlers to be expressed as named jobs/workflows (referenced by `#[job]` name, not closures), so they survive restart by registry lookup. Workflow row persists `compensation_steps: [{step, handler_job_name, payload}]`.
  - Or change the API to make compensation unsupported across suspension points and fail compile-time when a step with `.compensate(...)` is followed by `ctx.sleep`/`ctx.wait_for_event`.
  - Either way, document that closure-based compensation is single-process only.

## 11. Worker dispatch loop awaits semaphore permits inline; one stuck permit acquisition blocks the entire claim loop and the NOTIFY drain

- **Where:** `crates/forge-runtime/src/jobs/worker.rs:225-308`.
- **Severity:** P2.
- **Concern:** Inside the `for job in jobs` loop, `system_semaphore.clone().acquire_owned().await` is awaited *before* `tokio::spawn`. If `system_reserved` is 4 and 4 long-running `$workflow_resume` jobs are already in flight, the 5th claimed job hangs the loop. Subsequent claimed user jobs sit unprocessed in memory (already claimed in DB, so they're invisible to other workers) until the system permit is freed. Meanwhile new NOTIFY arrivals are coalesced by `Notify` and lost. Worst case: a slow workflow saga starves user job throughput on the same worker.
- **Fix sketch:** `try_acquire_owned()` first; if it fails for system semaphore, *return the job to the queue* by issuing an UPDATE back to `pending` (with a small backoff to avoid claim/release thrash), and continue draining user jobs. Or split the claim into two queries: one for system jobs sized to `system_reserved`, one for user jobs sized to `max_concurrent`.

## 12. `start()` fence reset side-effect: failed `start()` due to RowNotFound does not return the permit-equivalent to other workers, but does swallow the cancel-reason

- **Where:** `crates/forge-runtime/src/jobs/executor.rs:64-79`.
- **Severity:** P2.
- **Concern:** When `start()` returns RowNotFound (lost claim race), the executor returns `Cancelled { reason: ... }`. Fine. But the spawned task in `worker.rs:251` still consumed a semaphore permit for the whole duration of the lost-race detection, which is one DB round-trip. Cheap, but at scale (50 workers seeing high stale-reclaim rate due to overload) this multiplies the cost of the bad state. Also the `cancellation_reason` is built from `job.cancel_reason`, which is stale in-memory data — by the time the race finalizes, the actual cancel reason may have been set by another worker. Minor logging fidelity.
- **Fix sketch:** Detect the lost-claim case *before* spawning by re-reading the row after claim (extra round-trip, probably not worth it) or accept the minor cost and add a metric `worker_lost_claim_total` so operators can tune `stale_threshold` against observed loss rate.

## 13. Cron tick window of `2 × poll_interval` will double-dispatch on clock skew between cron leader and PG

- **Where:** `crates/forge-runtime/src/cron/scheduler.rs:227-263`.
- **Severity:** P2 — saved by the UNIQUE constraint, but causes spurious DB load.
- **Concern:** Each tick: `window_start = now - 2*poll_interval; between_in_tz(window_start, now)`. With `poll_interval = 1s`, every tick re-considers the same 2 seconds of scheduled times. The UNIQUE `(cron_name, scheduled_time)` constraint correctly rejects the duplicate, but each rejected attempt is still one INSERT … ON CONFLICT round-trip per cron per tick — even on a perfectly-running cluster. On a 50-cron deployment that's 50 wasted INSERT-on-conflict cycles every second.
- **Fix sketch:** Track `last_processed_scheduled_time` per cron in memory; window is `(last_processed, now)` rather than always 2s look-back. Fall back to the wider window only on first tick after acquiring leadership.

## 14. `ChangeListener`/`PgListener` payload truncated to 8000 bytes — workflow event NOTIFY uses `event_name:correlation_id` strings that can exceed it for long correlation IDs

- **Where:** `crates/forge-runtime/src/workflow/event_store.rs:43-49`.
- **Severity:** P2.
- **Concern:** PostgreSQL NOTIFY payload limit is 8000 bytes. Most correlation IDs are UUIDs (36 chars), so this is academically safe today. But the scheduler doesn't actually parse the payload — it just runs `process_ready_workflows()` on any NOTIFY. So the payload is wasted bandwidth (every event sender pays for a string format and a `pg_notify` round-trip that nothing reads). Sending an empty payload would be just as effective for the wakeup signal.
- **Fix sketch:** Either consume the payload (target specific run IDs to avoid scanning) or send `pg_notify('forge_workflow_wakeup', '')` to reduce bandwidth. The former is the right answer at scale.

## 15. Worker DB pool vs `max_concurrent` interaction is unbounded and undocumented

- **Where:** `crates/forge-runtime/src/jobs/worker.rs:50-58` (`max_concurrent: 8`, `system_reserved: 4`) and the shared single pool described in CLAUDE.md.
- **Severity:** P2.
- **Concern:** A single worker can hold up to `max_concurrent + system_reserved` = 12 *outer* in-flight job executions, each of which may acquire multiple DB connections (for `ctx.db`, for step persistence, for heartbeats). With 4 worker nodes that's 48 outer execution slots competing with HTTP request handlers for a shared pool. The framework provides no automated check that `pool.max_connections >= sum(workers × concurrency) + RPC_concurrency`. Pool exhaustion will manifest as random Database errors in jobs and 500s on RPC.
- **Fix sketch:** At `Forge::build()`, sum expected connection demand (workers, schedulers, daemons, RPC concurrency from `gateway.max_concurrent`) and either error out or warn if it exceeds `database.max_connections`. Document the formula. Bonus: give the heartbeat task a dedicated `PoolOptions::min_connections` reservation so heartbeats never lose to userland work.

---

## Top 3 fixes before GA

1. **Implement a real durable-sleep wakeup path (Issue #1) and fix the partial index status mismatch.** Today's `ctx.sleep(30 days)` works correctly but at scale loses both the precision claim and the index that makes the query cheap. This is the load-bearing promise of the workflow system — it must not be a polling lie.
2. **Make the workflow scheduler leader-gated (Issue #2) and add reconnect to all PgListener tasks (Issue #3).** Without these, multi-node deployments do N× the work of single-node deployments on the hot path, and any PG connection flap silently degrades job dispatch latency by 100× with no signal.
3. **Restore a non-terminal `Blocked` workflow status (Issue #9) and document/fix compensation across restart (Issue #10).** Both turn routine deployments into data-loss events for in-flight workflows. A framework that markets durable workflows cannot ship at 1.0 if `cargo deploy` silently fails every long-running saga that was suspended across the deploy boundary.

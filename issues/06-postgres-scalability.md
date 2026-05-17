# Postgres-as-Everything: Scale Audit

Forge runs every coordination primitive through a single Postgres 18 instance:
gateway reads/writes, jobs (SKIP LOCKED), workflows (durable state), cron,
signals (analytics), reactivity (LISTEN/NOTIFY + change log), leader election
(advisory locks), rate limiting, KV. This audit walks each subsystem and asks
where it hits a wall first, what regime the wall lives in, and what the
Postgres-only mitigation looks like.

Numbers below are characteristic, not benchmarked — they identify the regime
where each bottleneck bites, not a guaranteed cliff.

---

## 1. The `forge_notify_change` trigger is the central scaling chokepoint

**File:** `crates/forge-runtime/migrations/system/v002_change_log.sql:19-73`,
applied to every reactive table by
`crates/forge-runtime/migrations/system/v001_initial.sql:365-385`
(`forge_enable_reactivity`).

**Severity:** Critical.

**Regime:** Bites at ~1–5k row writes/sec aggregate across reactive tables.

Every INSERT/UPDATE/DELETE on a reactive table fires a per-row PL/pgSQL
trigger that:

1. Calls `to_jsonb(OLD)` and `to_jsonb(NEW)` (full-row JSON materialization,
   even for tiny column changes).
2. Diffs the two JSONBs key by key (`jsonb_each` → `IS DISTINCT FROM` per
   column).
3. INSERTs into `forge_change_log` (BIGSERIAL — sequence contention, plus a
   WAL record per change, plus the `idx_forge_change_log_created` index).
4. Calls `pg_notify` (acquires the cluster-wide NOTIFY queue lock).

For an UPDATE that touches one column on a 30-column table this is roughly
30× write amplification in row body + 1 WAL record for the trigger row + 1
NOTIFY queue insert + 1 index update. The trigger is `FOR EACH ROW`, so bulk
DML doesn't amortize.

Worse: the trigger runs synchronously inside the user's transaction. A
`UPDATE forge_jobs SET last_heartbeat = NOW()` (called every few seconds per
running job) trips the full trigger pipeline because `forge_jobs` is on the
reactivity list (`v001_initial.sql:433`). At 1000 concurrent jobs that is
1000 NOTIFY queue inserts + 1000 change_log rows + 1000 index updates *per
heartbeat round* with zero user-visible value (no one subscribes to
heartbeat changes).

**Mitigation (Postgres-only):**
- Add a column allowlist per table when calling `forge_enable_reactivity` so
  heartbeat / progress / `last_*` columns don't fire the trigger. Implement
  as `WHEN (NEW.* IS DISTINCT FROM OLD.* on tracked cols)` clause on the
  trigger.
- Switch the JSON diff to a STATEMENT-level trigger with a per-table list of
  watched columns, or generate a per-table trigger function that only checks
  the columns reactivity actually cares about.
- Move `forge_change_log` to UNLOGGED + replicate via logical decoding if
  durability matters less than WAL pressure. Or: emit only the seq and
  table_name into the log, fetch the row body on demand.
- Stop putting `forge_jobs` on the reactivity firehose — jobs already have a
  dedicated `forge_jobs_available` channel (v003) for worker wakeup; the
  reactor subscribing to job-table changes is a separate concern that should
  go through a coarser-grained notification.

---

## 2. NOTIFY queue contention and the 8 KiB payload cliff

**File:** `crates/forge-runtime/migrations/system/v002_change_log.sql:61-65`
(explicit 7900-byte trim of the column list before publish),
`crates/forge-runtime/src/pg/notify.rs:48` (`MAX_PAYLOAD_BYTES = 7 * 1024`).

**Severity:** High.

**Regime:** Bites two ways. (a) Throughput: PG's NOTIFY queue is a
single-writer global structure protected by an LWLock; sustained throughput
caps somewhere around 10–30k notifies/sec for the entire cluster regardless
of pool size. (b) Payload: the trigger silently drops the column list when
the payload would exceed 7900 bytes, forcing the listener into "invalidate
everything for the table" (`listener.rs:212` / column-filter fallback in the
reactor).

This means a wide table (many columns, long names) loses column-level
invalidation entirely and falls back to table-level — every subscription on
that table re-executes on every write. That is a silent reactor performance
cliff with no metric exposing it.

`pg_notification_queue_usage()` is referenced in `pool.rs:357` as a PG-18
feature but I don't see it actually polled anywhere. The system is flying
blind on queue pressure until publishes start failing with
`NOTIFY queue is full`.

**Mitigation:**
- Add a metric backed by `pg_notification_queue_usage()` and a healthcheck
  that flips `/ready` to unhealthy at >80%.
- For high-fanout tables, push the payload to `forge_change_log` and emit
  only `seq` over NOTIFY (the listener already replays from the log). That
  removes the column-list cliff and shrinks every payload to ~30 bytes.
- Cap reactivity-eligible columns at trigger creation: if a table has >40
  columns, force change_log-only mode.

---

## 3. WAL pressure: signals partition + change log + workflow steps are all hot

**Files:**
- `crates/forge-runtime/src/signals/collector.rs:225-282` (32-column UNNEST
  into partitioned `forge_signals_events`).
- `crates/forge-runtime/migrations/system/v002_change_log.sql:5-12`
  (`forge_change_log` BIGSERIAL + index).
- `crates/forge-runtime/src/workflow/executor.rs:678-705` (per-step
  INSERT/UPDATE on `forge_workflow_steps`).

**Severity:** High.

**Regime:** Bites at ~10k events/sec sustained for signals alone, sooner if
workflows are step-heavy.

Signals is the largest write source. Every RPC call produces a row (auto-
captured in `FunctionExecutor`). The table has 7 inherited indexes
including a GIN on `properties` (`v001_initial.sql:728`). GIN inserts are
expensive — each new row hits the pending list / fastupdate path, then
periodically gets folded back, which causes VACUUM/autovacuum spikes. At
10k events/sec the pending list churn alone can saturate one core's worth
of autovacuum work.

The change log compounds this: every write to a reactive table writes a
change_log row, which itself has an index. So one user UPDATE produces (a)
the user row WAL, (b) the change_log row WAL, (c) the change_log index WAL,
(d) the NOTIFY queue entry. Three index/heap writes per user write.

Workflow steps amplify too: a 20-step workflow produces 20 INSERT/UPDATEs
on `forge_workflow_steps` plus 20 UPDATEs on `forge_workflow_runs` (status,
saved_state, current_step) — and `forge_workflow_runs` is on the reactivity
list (`v001_initial.sql:434`), so each of those 40 writes fires the
trigger.

**Mitigation:**
- Drop the GIN on `forge_signals_events.properties` by default; gate it
  behind config. Most deployments don't query JSON properties; those that
  do can add it back.
- Reduce per-step writes: one UPSERT per step, not insert+update. Skip
  `current_step` updates on `forge_workflow_runs` — derive it from the
  latest step row at read time.
- Move `forge_workflow_steps` off the reactivity firehose unless someone
  explicitly subscribes to step-level events. Currently
  `v001_initial.sql:435` enables it unconditionally.
- Consider an UNLOGGED staging table for signals events, batch-INSERT into
  the partitioned table every minute. Costs at-most-N-seconds durability;
  signals are not financial.

---

## 4. Single pool, every workload sharing it, gateway last in line

**File:** `crates/forge-runtime/src/pg/pool.rs:9-86` (doctrine comment),
`crates/forge-core/src/config/database.rs:103` (default `pool_size = 50`).

**Severity:** High.

**Regime:** Bites under any sustained job/workflow load combined with
moderate gateway traffic.

The single-pool decision is documented and conscious. The risk it names
("a burst of slow background work can drain the budget that gateway
requests need") is real and unmitigated: there is no semaphore at the
gateway acquire site. The worker semaphore caps workers, the reactor
semaphore caps re-executions, but gateway requests just compete for
whatever's left. If reactor + workers + persistent listeners hold (10 + 64
+ 6 ≈ 80) and `pool_size = 50`, the gateway gets *zero* connections during
a reactor flush, but pool_timeout is 30s — every gateway request waits up
to 30s, then 503s.

The documented sizing formula lands at ~130 connections for default knobs
(`pool.rs:54-62`), but the default is 50. So the default config is already
oversubscribed. Doctrine says "raise pool_size and/or lower worker
concurrency" — but raising pool_size into the hundreds runs into PG's own
connection cost (~10 MB/conn + process overhead), pushing past where a
single PG instance is happy. PG's practical sweet spot is ~200–400
connections per node; beyond that, throughput goes *down*.

**Mitigation:**
- Bump the default pool_size to match the documented formula (≥130) and
  warn loudly at startup if pool_size < worker.max_concurrent +
  realtime.max_concurrent + 16.
- Add a gateway-side semaphore (sized < pool_size) so background work
  can't starve user requests. The doctrine comment dismisses "tagged
  semaphores" until a workload proves it; that workload is "any production
  cluster with a busy reactor."
- Refuse to start (or warn aggressively) if `pool_size` × node count
  exceeds 70% of `max_connections` on the PG side. The connection budget
  is cluster-wide; a 10-node cluster with pool_size=130 needs 1300 PG
  conns, which is past the comfortable single-instance ceiling.

---

## 5. Persistent LISTEN connections grow O(nodes × channels)

**Files:** `crates/forge-runtime/src/pg/pool.rs:42-46` (documents 2–3
persistent connections per node), `crates/forge-runtime/src/jobs/worker.rs:160-180`
(one `forge_jobs_available` listener per worker), `realtime/listener.rs:172-182`
(one `forge_changes` listener per node), `workflow/scheduler.rs:91-93`
(`forge_workflow_wakeup`).

**Severity:** Medium.

**Regime:** Bites at ~50+ nodes or when running many workers per node.

Each listener holds a connection for the process lifetime. A node with one
worker holds ~3 persistent listeners. Add leader-elected daemons (one PG
connection per leader role this node owns, doctrine documents this). At 50
nodes × ~4 persistent conns ≈ 200 connections just sitting on LISTENs,
before any work happens. Each LISTEN'd channel also costs O(listeners)
delivery work in the NOTIFY backend.

The worker listener is the worst offender: `worker.rs:159-180` spawns a
NEW PgListener per worker instance. If a node runs N parallel
worker processes (e.g., to use multiple cores around tokio's single-runtime
limit) the listener count multiplies.

**Mitigation:**
- One process-wide listener that fans out to all in-process workers via a
  tokio broadcast channel, not one listener per worker. The current shape
  is a leak waiting for a high-worker-count deployment.
- Document and enforce a hard ceiling: `nodes × (3 + leader_roles_owned)`
  must stay under `max_connections / 4`.

---

## 6. Advisory lock contention is benign — but the lease-row writes are not

**File:** `crates/forge-runtime/src/pg/leader.rs:117-160`.

**Severity:** Low-medium.

**Regime:** Bites in steady state with many cluster nodes.

`pg_try_advisory_lock` itself is fast and uses a dedicated hash table.
The risk lives elsewhere: every leader refresh writes a row to
`forge_leaders` (`leader.rs:142-157`, runs every `check_interval = 5s` by
default). With one leader per role and ~6 leader roles in the framework
(cron, signals refresh, workflow scheduler, etc.) that's 6 writes/5s =
1.2 writes/sec — fine. But each write fires the reactivity machinery if
the table is on it (`forge_leaders` isn't, by inspection — good). And
standbys also probe via `pg_locks` (`leader.rs:18-22` validate interval =
1s).

A 50-node cluster running 4-role leader contention does ~50 × 4 / 5s =
40 `pg_try_advisory_lock` calls/sec + 40 `pg_locks` reads/sec. `pg_locks`
is unbounded in cost — it scans all lock manager partitions and is known
to be slow under contention.

**Mitigation:**
- Keep `forge_leaders` UNLOGGED (already is — `v001_initial.sql:32`).
- Cache `pg_locks` probe results locally for at least one `check_interval`.
- Don't run validate-lock every second; coalesce with the refresh tick.

---

## 7. Rate limiter does one round-trip per request, atomic on a hot bucket key

**File:** `crates/forge-runtime/src/rate_limit/limiter.rs:38-58`.

**Severity:** High.

**Regime:** Bites the moment a global or per-tenant bucket gets hot.

The `StrictRateLimiter` does an INSERT ... ON CONFLICT DO UPDATE on
`forge_rate_limits` *per request*. The table is UNLOGGED (good), but
`ON CONFLICT DO UPDATE` on a single hot key serializes every request
through one row lock. A `RateLimitKey::Global` bucket gets every request
in the cluster funneled through one row's RowExclusiveLock — that's a
hard ceiling around a few thousand req/sec depending on row width and
contention.

Same problem for "per-tenant" limits on a busy tenant: their bucket row
becomes the contention point.

**Mitigation:**
- Sharded bucket: write to one of K (say 16) shards per logical key
  picked by `random()`, read by summing. Trades exactness (over-allow by
  up to one shard's worth) for throughput.
- Tiered approach: the `HybridRateLimiter` referenced as the DDoS option
  is mentioned but I'd promote it to default. PG-only fallback runs in
  the local DashMap first, only checks PG every N requests or when local
  budget runs low. Doctrine allows this — PG stays authoritative, local
  is just an admission gate.

---

## 8. Replica round-robin without read-your-writes guard

**File:** `crates/forge-runtime/src/pg/pool.rs:257-277` (`read_pool`),
`realtime/reactor.rs:88-99` (acknowledges the lag tradeoff).

**Severity:** High when replicas are enabled and read-from-replica is on.

**Regime:** Bites on any write-then-immediately-read flow, which is
common after mutations.

The reactor comment names the problem: "a NOTIFY may arrive before the
replica has the committed data." Replication lag is the issue for reactor
re-execution, but it's also an issue for normal query handlers. The
mutation context flushes the outbox after commit and returns to the
client; the client then issues a query that hits a replica that hasn't
caught up. Result: client sees stale data immediately after writing it.

Forge has no causal-consistency token (no LSN tracking, no
`pg_wait_for_replay_lsn`), no per-session sticky routing, no
write-followed-by-read flag.

**Mitigation:**
- Read the commit LSN after every mutation (`pg_current_wal_lsn()`),
  stash on the auth context or session, route subsequent reads to a
  replica only if `pg_last_wal_replay_lsn() >= stashed_lsn` else fall
  through to primary.
- Or: in the mutation context, mark the session as "writes-pinned-to-
  primary for next N seconds" — coarse but cheap.
- Either way: don't ship `read_from_replica = true` as a recommended
  default until causality is solved.

---

## 9. Workflow runs table: high-frequency UPDATE target, on reactivity

**File:** `crates/forge-runtime/src/workflow/executor.rs:489-509`
(`persist_saved_state`), `:612-674` (status transitions), reactivity
enabled at `v001_initial.sql:434`.

**Severity:** Medium-high.

**Regime:** Bites at ~1000+ concurrent active workflows.

Every step boundary writes to `forge_workflow_runs` (status, current_step,
saved_state, sometimes compensation_state). `saved_state` is JSONB and
can grow arbitrarily — workflow authors are encouraged to use it as
durable variable storage. Two compounding issues:

1. **Bloat:** Each UPDATE of a JSONB column creates a new heap tuple
   (Postgres MVCC). A workflow that updates `saved_state` 50 times during
   its run leaves 50 dead tuples on the same row. With 10k workflows
   running concurrently, that's 500k dead tuples on one table awaiting
   autovacuum.
2. **Reactivity amplification:** Every UPDATE fires the
   `forge_notify_change` trigger, which writes another change_log row
   and another NOTIFY. None of the reactor's subscribers usually care
   about `saved_state` changes mid-run.

**Mitigation:**
- Move `saved_state` and `compensation_state` to a separate
  `forge_workflow_state` table (one row per run) that is NOT on the
  reactivity firehose. The runs table keeps the fields anyone subscribes
  to (status, output, completed_at).
- Add an explicit autovacuum tuning hint in the migration:
  `ALTER TABLE forge_workflow_runs SET (autovacuum_vacuum_scale_factor =
  0.01, autovacuum_vacuum_insert_scale_factor = 0.01);`
- Or: track the version counter and use HOT-update-friendly fixed-width
  fields where possible.

---

## 10. `forge_jobs` is a hot read+write table; SKIP LOCKED has its own ceiling

**File:** `crates/forge-runtime/src/jobs/queue.rs:245-293` (claim query),
indexes at `v001_initial.sql:74-93`.

**Severity:** Medium.

**Regime:** Bites at ~5k jobs/sec dispatched per Postgres instance.

The claim CTE does a `FOR UPDATE SKIP LOCKED` over a partial index
filtered by `status = 'pending'`. The index handles the happy path well.
Two issues:

1. The query joins on the `forge_paused_queues` NOT EXISTS subselect on
   every claim (`queue.rs:265-268`). That's a small table but it's
   touched on every batch claim by every worker.
2. The table accumulates terminal rows (completed/failed/dead_letter/
   cancelled) until the `cleanup_expired` cron deletes them
   (`v001_initial.sql:517-529`, default retention 7 days). With 5k
   jobs/sec, that's ~3 billion rows at any time before cleanup —
   completely fine for storage but the partial index on
   `status = 'pending'` only helps the claim path. Range scans for "show
   me my jobs" go through the much larger
   `idx_forge_jobs_owner_subject` against a 3-billion-row heap.

Also: `forge_jobs` is on the reactivity firehose, so every claim, start,
complete, heartbeat fires the change trigger — see Bottleneck 1.

**Mitigation:**
- Move terminal jobs to `forge_jobs_history` on completion; keep the
  hot table small. The "retention" knob then becomes a knob on the
  history table where it doesn't impact claim latency.
- Add `idx_forge_jobs_owner_status` so per-owner queries don't scan the
  history.
- Reduce heartbeat write frequency — current default isn't shown but
  every heartbeat is a full UPDATE + trigger. Heartbeats could be
  coalesced or moved to a UNLOGGED side table.

---

## 11. `forge_signals_users` UPSERT path is unbounded contention on identify()

**File:** `crates/forge-runtime/migrations/system/v001_initial.sql:793-814`
(table); call site in signals collector / endpoints not shown explicitly
but `identify()` is documented in CLAUDE.md.

**Severity:** Medium.

**Regime:** Bites for any single user generating bursty identify() calls
(SPA reloads, multi-tab sessions, mobile reconnects).

`forge_signals_users` has counters (`total_sessions`, `total_events`) and
JSONB `traits`. Every identify() call presumably increments these. A user
reloading a page 10× in 30 seconds (typical during dev / debugging /
flaky network) hits the same row 10× under `ON CONFLICT DO UPDATE`,
serializing through one row lock. Worse, JSONB merge of `traits` is a
read-modify-write on a possibly-large blob.

**Mitigation:**
- Counters belong in `forge_kv_counters` (already exists for this
  purpose) keyed by user_id; only flush aggregated values to
  `forge_signals_users` periodically.
- `traits` should be set-only on first identify, then explicitly merged
  by a job, not blindly merged on every call.

---

## 12. Materialized views are refreshed CONCURRENTLY every 5min — at scale they don't finish

**File:** `crates/forge-runtime/migrations/system/v001_initial.sql:819-928`
(three matviews), `crates/forge-runtime/src/signals/views.rs:11-13`
(refresh call).

**Severity:** Medium.

**Regime:** Bites once `forge_signals_events` crosses ~100M rows.

`forge_signals_daily_stats` scans all events in the last 90 days,
`forge_signals_retention` does a self-cohort join over all users × all
events, `forge_signals_function_stats` aggregates 30 days of rpc_call
events with PERCENTILE_CONT. All three are refreshed CONCURRENTLY every
5 minutes on the leader (`refresh_views`). CONCURRENTLY requires a
unique index and effectively replays the whole query.

At 10k events/sec for 30 days that's ~26 billion events to PERCENTILE_CONT
over — single-query, no parallelism guarantees, holding a snapshot for
the duration. Lock-wise it's safe (concurrent refresh), but it spins one
core continuously and prevents the table from being vacuumed effectively
because the snapshot is open.

**Mitigation:**
- Replace `forge_signals_daily_stats` with an incremental rollup: a
  per-hour summary table written by a job, then daily/weekly views on
  top of that. Common pattern, scales linearly.
- Tier the refresh: function_stats every 5min, daily_stats every hour,
  retention every 6 hours.
- Use `pg_cron`-style or a worker-driven rollup that processes only the
  delta since last run.

---

## 13. Partition management is "today + next month" — no buffer for traffic spikes

**File:** `crates/forge-runtime/src/signals/partition.rs:14-34`,
DDL at `v001_initial.sql:679-703`.

**Severity:** Medium.

**Regime:** Bites at the first month boundary when the cron is unhealthy.

The runtime maintains only the current month's and next month's
partition. If the partition cron fails (leader election broken, daemon
crashed, cluster down at month rollover) inserts fall through to
`forge_signals_events_default` — the default partition. The default
partition is fine as a safety net but: (a) once it has data, you can't
attach a new partition for that range without first detaching the
default; (b) the default has the same indexes as the parent, so it
accepts writes happily and accumulates silently.

There's no alert, no metric, no health check on partition coverage. The
first sign of trouble is when an operator finally tries to add the
missing partition and the ATTACH PARTITION fails.

**Mitigation:**
- Maintain `current + 3` partitions, not `current + 1`. Cheap insurance.
- Health-check that `forge_signals_events_default` is empty; flip
  `/ready` to degraded if it isn't.
- Surface partition coverage in an admin endpoint.

---

## 14. `forge_change_log` retention is 1 hour — fragile for slow consumers

**File:** `crates/forge-runtime/migrations/system/v002_change_log.sql:77-87`,
listener resync logic at `realtime/listener.rs:151-162`.

**Severity:** Medium.

**Regime:** Bites whenever a node is offline for >1 hour or a listener
falls behind.

The change log gives at-least-once recovery for the LISTEN/NOTIFY feed
within a 1-hour window. Outside that, listeners fall back to "full
resync of all active subscriptions" (`listener.rs:96-98`,
`needs_resync`). At 10k subscriptions per node, a full resync is 10k
query re-executions. That's a thundering herd on the primary every time
a node restarts after a long outage or a network partition.

**Mitigation:**
- Tier the retention with the write rate: keep at least N=1e6 rows
  regardless of age, plus the 1-hour floor. A burst of writes shouldn't
  evict the recent past.
- Cap the resync rate: a full-resync limits to K queries/sec to avoid
  spiking the primary. The reactor's `max_concurrent_reexecutions = 64`
  partially handles this, but spread it across more than the immediate
  flush window.
- Persist the listener's `last_seq` across restarts (currently in-memory
  only at `listener.rs:47`). On node restart, you re-replay from
  `max_seq`, not from where you left off — so you miss everything
  emitted while down.

---

## 15. Observability gaps make all of the above invisible

Cross-cutting issue. Several scale signals exist in PG but aren't surfaced:

- `pg_notification_queue_usage()` is referenced (`pool.rs:357`) but not
  polled or exported.
- `pg_stat_activity` waits per workload aren't broken out — the
  application_name is set per-service (`pool.rs:177`) but no metric
  exposes "gateway connections waiting" vs "worker connections in use."
- Dead tuple counts on hot tables (`forge_jobs`, `forge_workflow_runs`,
  `forge_signals_events`) aren't on the dashboard.
- Replication lag isn't checked against an expected SLO; the replica
  health check (`pool.rs:283-318`) only proves the replica is up, not
  caught up.

Without these, the framework hits the walls above without warning. The
first signal will be elevated p99 latency, which won't say *which*
subsystem starved.

**Mitigation:**
- Export the four PG views (`pg_stat_activity`, `pg_stat_user_tables`,
  `pg_stat_replication`, `pg_notification_queue_usage`) as Prometheus
  metrics from the daemon side.
- Add `/admin/diag/pg` endpoint that returns a one-shot snapshot of all
  four for support tickets.

---

## Top 3 scale fixes before GA

1. **Make the change-trigger column-selective and stop firing it on
   `forge_jobs` / `forge_workflow_runs` / `forge_workflow_steps`.**
   The full-row JSONB diff on every write to every reactive table is the
   single highest cost in the system and most of the writes it fires on
   (heartbeats, saved_state, current_step) have no subscribers. Fix:
   reactivity declares the watched column set per table; trigger short-
   circuits when none of those columns changed. Pulls 80% of the WAL,
   NOTIFY-queue, and change_log pressure out of the steady-state path.

2. **Add a gateway-side connection semaphore and raise the default
   `pool_size`.** Default `pool_size = 50` is already below the
   documented sizing formula. Background work has caps, gateway work
   doesn't, so under load the gateway is the first to starve — the
   opposite of what users expect. Two-line fix: default to 130, add a
   semaphore sized `pool_size - (worker_max + reactor_max + 16)` at the
   gateway acquire site. Doctrine objects to per-workload pools; this is
   per-workload *admission*, which is the throttle the doctrine
   document already endorses for workers and the reactor.

3. **Solve causal consistency for the replica read path, or stop
   shipping replicas as a scale answer.** `read_from_replica` is
   user-visible but the read-after-write story is unspecified — a
   correctness landmine the framework will get blamed for. Cheapest fix:
   on every mutation commit, capture `pg_current_wal_lsn()`, attach to
   the session, route the next ~30s of reads from that session to
   primary OR to a replica that has caught up to that LSN
   (`pg_last_wal_replay_lsn()`). PG-only, no external state, fits the
   doctrine. Without this, replicas can't be turned on safely.

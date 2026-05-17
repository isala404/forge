# Cluster Coordination Audit

Scope: `crates/forge-runtime/src/cluster/` plus how `LeaderElection` is consumed by cron, daemons, and workflows. PostgreSQL session advisory locks via `pg_try_advisory_lock` are the only mutual-exclusion primitive; heartbeats and lease rows are advisory metadata. The framework is single-Postgres, so PG itself is the consensus authority — most concerns reduce to *what happens between the moment the leader loses its lock and the moment its in-flight work observes that loss*.

---

## 1. Cron-tick split brain inside `lock_validate_interval`
**File**: `crates/forge-runtime/src/cron/scheduler.rs:190`, `crates/forge-runtime/src/pg/leader.rs:411`
**Severity**: High
**Failure mode**: `CronRunner::is_leader()` reads the cached `AtomicBool` written by `LeaderElection`. `validate_lock_held` only runs every 1s (default `lock_validate_interval`). If PG terminates the leader's backend at T, the new candidate's `try_become_leader` succeeds on the very next `check_interval` tick (5s default — but it can be sooner if the standby polls just after PG sees the lock free). Meanwhile the old leader's `is_leader()` still returns true until its own validate timer fires up to 1s later. Both nodes will execute `tick()` and enter `try_claim_and_enqueue` in that overlap. The `(cron_name, scheduled_time)` unique constraint saves correctness for the *new* slot, but the stale leader can also fire `handle_catch_up`, which scans completed history and tries to claim arbitrary back-dated slots — easy duplicate enqueue.
**Fix**: Require `is_leader()` callers to re-validate the lock at the start of each tick (cheap `pg_try_advisory_lock_shared` probe on the held conn, or call `validate_lock_held().await?` first thing in `tick()`), and short-circuit on failure. Belt-and-braces: make the `(cron_name, scheduled_time)` claim also assert the caller's node holds the lease row in the same statement.

## 2. Daemon leader-elected loop never re-checks leadership after acquiring it
**File**: `crates/forge-runtime/src/daemon/runner.rs:329-356, 408`
**Severity**: High
**Failure mode**: `run_daemon_loop` calls `try_become_leader` once before invoking the daemon handler. The handler can run indefinitely. If the lock is lost mid-run (PG kills the backend, network reset, sqlx silently swaps the connection — `test_before_acquire` only fires on pool checkout), the daemon keeps running while another node also acquires leadership and starts its own copy. This is the canonical split-brain scenario for `leader_elected = true` daemons (e.g. a singleton sweeper, a queue drainer, an external-system reconciler).
**Fix**: Spawn `LeaderElection::run()` as a sibling task and pass a `CancellationToken` or `watch::Receiver<bool>` derived from `is_leader` into `DaemonContext`. When leadership drops, cancel the handler future via `tokio::select!`. Today `DaemonContext` has a shutdown signal but no leadership signal — they are not the same thing.

## 3. Lock-owning connection has no pool-side keepalive or lifetime guard
**File**: `crates/forge-runtime/src/pg/leader.rs:122-126, 161`
**Severity**: High
**Failure mode**: The leader stashes a `PoolConnection<Postgres>` for the lifetime of leadership. sqlx tears down idle backend connections under `idle_timeout`/`max_lifetime`; the configured pool (`pg/pool.rs:242`) does not set either — but firewalls, PgBouncer in front of PG, or PG-side `idle_in_transaction_session_timeout` / `tcp_keepalives` settings still kill long-idle backends. If the TCP side dies, sqlx will detect it the next time we issue a query — meaning the **next** validate tick — but during that window we hold leadership in memory and other nodes assume the lock is free. Worse: if the connection dies between two writes (e.g. inside `refresh_lease`), sqlx may transparently reconnect on the next query against the same `PoolConnection` handle (sqlx does *not* in fact reconnect a checked-out conn, but operators commonly mis-set max_lifetime expecting it to). Either way, the framework has no proactive keepalive on the held conn.
**Fix**: Either (a) run a `SELECT 1` on the held connection every `min(check_interval/2, 5s)` to keep the backend alive and detect TCP loss within one cycle, or (b) set `tcp_keepalives_idle/interval/count` explicitly on the dedicated connect_options when sqlx hands them off — and pin `max_lifetime(Duration::MAX)` on a *separate* leader-only pool sized 1.

## 4. `release_leadership` on the pool, not the held connection, after partial loss
**File**: `crates/forge-runtime/src/pg/leader.rs:331-341`
**Severity**: Medium
**Failure mode**: When `lock_connection` is `None` (because validate already dropped it), the code falls through to `DELETE FROM forge_leaders WHERE role=$1 AND node_id=$2` *on the pool*. A different node may have already taken leadership and overwritten the row via `ON CONFLICT DO UPDATE` — the WHERE node_id guard does save us from clobbering the new leader's row, but only by accident: there is no integrity check that this node was ever the leader. Combined with #3, a node that briefly thought it was leader can issue a stray DELETE attempt under race.
**Fix**: Track a `was_ever_leader` flag and refuse the DELETE if validate already cleared `is_leader` (it's already a no-op in that case via WHERE). Document the invariant. Or: gate the DELETE behind a `RETURNING node_id` check so unexpected races are logged loudly.

## 5. Heartbeat thread and lock-validate are completely decoupled
**File**: `crates/forge-runtime/src/cluster/heartbeat.rs:197-211`, `crates/forge-runtime/src/pg/leader.rs:175`
**Severity**: Medium
**Failure mode**: A leader can keep heartbeating on the pool (so it remains `active` in `forge_nodes` and is never marked `dead`) while having silently lost its advisory lock. From an operator's dashboard the node looks healthy *and* the new leader row exists — two "leaders" by metadata, with one of them serving stale work for up to `lock_validate_interval`. The `mark_dead_nodes` threshold is even longer (max of `dead_threshold` or 3× current adaptive interval — adaptive interval can climb to 60s, making the dead threshold up to 3 minutes).
**Fix**: When `validate_lock_held` discovers a lost lock, push a status update through the registry to set the node to `draining` or `degraded` so operators see the discrepancy. Cross-reference: `cluster/metrics::set_is_leader(false)` only updates Prometheus, not the DB.

## 6. Heartbeat update uses the shared pool, not a dedicated conn — pool exhaustion fakes node death
**File**: `crates/forge-runtime/src/cluster/heartbeat.rs:198-209`
**Severity**: Medium
**Failure mode**: When the gateway/worker is saturated and all pool conns are checked out for >`acquire_timeout` (default 30s) the heartbeat UPDATE times out. Other nodes' `mark_dead_nodes` then flips this still-running node to `dead`. Workflow/job assignment continues, but operators see a flapping cluster and the affected node's leader advisory lock *remains held* (different connection, different lifetime). Net effect: a healthy leader is marked dead while still being leader.
**Fix**: Hold a dedicated 1-conn pool for cluster maintenance (heartbeat + leader election), independent of the request pool. This is already the canonical pattern for the lock conn — extend it to heartbeats.

## 7. Cron stale reclaim duplicates jobs without cancelling the original
**File**: `crates/forge-runtime/src/cron/scheduler.rs:330-358`
**Severity**: High
**Failure mode**: `try_claim_and_enqueue` does `ON CONFLICT (cron_name, scheduled_time) DO UPDATE … WHERE forge_cron_runs.status='running' AND started_at < NOW() - 15min`. When it fires, it overwrites the run's `id` and `node_id` and enqueues a *new* `forge_jobs` row. The original cron job is **not cancelled**: it stays in `forge_jobs`, can still be claimed by a worker on the original node (or a fresh worker after restart), and runs to completion alongside the reclaim. Cron handlers that are not idempotent (sending emails, charging cards, posting webhooks) will fire twice. The 15-minute threshold sounds safe, but a leader that lost its lock at T but is still running the handler will look "stale" exactly because the lock was lost — duplicate fire is the *common* case, not the corner case.
**Fix**: On reclaim, mark the previous job row as `cancelled` (or set a `superseded_at` column) and have the worker check that flag before invoking the handler. Or store the cron run's `claim_id` on the job and verify the job's claim_id still matches the live run at handler entry.

## 8. Daemon FNV lock_id is 64-bit but folded into i64 with wrapping_mul — collision risk between daemons
**File**: `crates/forge-core/src/cluster/roles.rs:88-95`
**Severity**: Medium
**Failure mode**: The "FNV-1a" hash uses `wrapping_mul(1099511628211)` (the 64-bit FNV prime) on an `i64` seeded at `0x464F_5247_4000`. Two distinct daemon names can collide and share an advisory lock. Two daemons that should run on different leaders will then contend on the same lock, making one starve the other forever. Worse, when both happen to belong to *different* applications joined to the same PG (forbidden today but easy to do in dev), they cross-elect. Birthday-bound says collision is statistically rare under a few thousand daemons, but with stable names the failure is deterministic per project, not random — bad luck means "broken forever".
**Fix**: Detect collisions at startup: compute the lock_id for every registered leader role and abort with a clear error if two non-equal `LeaderRole`s map to the same `lock_id`. Bonus: use the full 64-bit FNV (returning `i64::from_ne_bytes(u64::from(hash).to_ne_bytes())`) and skip the seed-base mixing, or persist daemon name → assigned-id in a `forge_daemon_locks` table with `SERIAL`.

## 9. Workflow scheduler runs on every node; cleanup is leader-gated but claim is not
**File**: `crates/forge-runtime/src/workflow/scheduler.rs:69-75, 113-148`
**Severity**: Low (correctness), Medium (cost)
**Failure mode**: `is_leader()` returns `true` when no `leader_election` is configured (line 74). Even with election configured, the actual `process_ready_workflows` path is *not* leader-gated; only `cleanup_consumed_events` is. The claim is safe (UPDATE with WHERE status, single-tx enqueue) but every node in the cluster scans `forge_workflow_runs` every `poll_interval` (default 1s) and contends on the same rows. At 10 nodes this is 10× the read load and 10× the LISTEN reconnect storm. Not a correctness bug, but pre-1.0 it should be settled deliberately.
**Fix**: Either commit to "every node polls, claims race on PG" (document it, and ensure the SELECT uses an index that doesn't lock-spin), or move the SELECT under `is_leader()` and rely on NOTIFY for the immediate-wake path. Today the design is unstated.

## 10. Workflow signature mismatch during rolling deploy strands all in-flight runs
**File**: `crates/forge-runtime/src/workflow/registry.rs:152-176`, `crates/forge-runtime/src/workflow/scheduler.rs:334`
**Severity**: High
**Failure mode**: During a rolling deploy, half the cluster runs `workflow X v1 sig=A` and half runs `workflow X v1 sig=B` (someone added a step without bumping the version — a bug, but a common one). The scheduler does not consult `LeaderElection` when claiming a row, so v1-sig=A nodes pull the run, call `validate_resume`, get `SignatureMismatch`, and mark the row blocked. A node with v1-sig=B will *never* try to resume it because it's already blocked. Result: a single misdeploy bricks every concurrent run of that workflow for the duration of the rollout, requiring operator action even though sig=B was the canonical version.
**Fix**: On `SignatureMismatch`, log loudly but **do not transition to a terminal `BlockedSignatureMismatch` state** — return the row to its prior `sleeping`/`waiting` state and let it be re-picked by another node. Only mark blocked after N consecutive mismatches across all live nodes (e.g. mark blocked when no `forge_nodes` row advertises a matching signature). Cross-reference: the workflow macro is supposed to *force* a version bump on signature change at compile time — verify that contract holds end-to-end (audit suggestion separate from this fix).

## 11. Graceful shutdown does not wait for leader-held work to drain before releasing the lock
**File**: `crates/forge-runtime/src/cluster/shutdown.rs:88-131`
**Severity**: High
**Failure mode**: `GracefulShutdown::shutdown()` sequence: set draining → wait for in-flight RPCs → release leadership → deregister. The in-flight counter only tracks RPC handlers (via `InFlightGuard` — used only at the gateway). It does **not** track:
- Cron handlers that have been dispatched as jobs and are currently executing
- Workflow steps in progress
- Daemon work-in-progress (if `leader_elected`)
- Job-worker tasks
When SIGTERM arrives, the lock is released immediately after RPC drain, but the daemon's `run_daemon_loop` is still inside the user handler. The next node grabs the lock; for ~poll_interval seconds two nodes run the daemon. The graceful shutdown actively creates the split-brain it should prevent.
**Fix**: Either (a) require `LeaderElection::release_leadership()` be called *after* every leader-elected subsystem signals "cleanly stopped", not at a hardcoded position in the shutdown ladder, or (b) push leader-elected work onto the same in-flight counter so step 2 actually waits for it.

## 12. Late shutdown subscribers miss the broadcast
**File**: `crates/forge-runtime/src/cluster/shutdown.rs:293-310` (test documents the behaviour)
**Severity**: Medium
**Failure mode**: `broadcast::channel(1)` does not replay history. A handler that calls `shutdown.subscribe()` *after* `shutdown_tx.send(())` fires never observes shutdown and runs to completion ignoring the drain timeout. The test at line 293 confirms this is a *known* property, not a bug. But subscribers are created at startup, not per-request — except daemon contexts and some background spawns subscribe lazily. If a daemon panics-and-restarts during shutdown, the restarted daemon subscribes after the broadcast and never sees it.
**Fix**: Use `tokio::sync::watch::channel(false)` (replays current value) instead of `broadcast`. Or pair the broadcast with the `shutdown_requested: AtomicBool` so late subscribers can poll it once on attach.

## 13. No version-skew gate — old nodes can serve traffic after a newer node has migrated schema
**File**: `crates/forge-runtime/src/cluster/registry.rs:27-57`
**Severity**: High
**Failure mode**: `forge_nodes` records `version` but nothing in the cluster code reads it. During a rolling deploy from v0.5.0 → v0.6.0 where v0.6.0 runs a `forge migrate` that adds a new column referenced by a query, v0.5.0 nodes will start failing those queries (SQL compile-time check passed *for that binary's view of the schema*, but the new schema is incompatible). Symmetrically, an old node still holding the scheduler leader lock will keep firing crons against the new schema. No node compares its `version` against the active leader's `version` and refuses leadership if behind.
**Fix**: Record a `forge_schema_version` table updated by migrations; have nodes compare on startup and on every leader acquire. A node whose `version` is older than the max active version in `forge_nodes` should not attempt leadership and should refuse to serve mutations. Pre-1.0 acceptable to be strict: "all live nodes must report identical `version` or the cluster is in a downgrade-blocked state".

## 14. Time-skew is implicit everywhere
**File**: `crates/forge-runtime/src/pg/leader.rs:139-141, 260-261, 351-364`, `crates/forge-runtime/src/cron/scheduler.rs:227`
**Severity**: Medium
**Failure mode**: `lease_until` uses `Utc::now()` on the *Rust process*, then compares with `NOW()` on PG in `check_leader_health` (line 361 uses Rust's `Utc::now()` again — at least consistent on the reader, but the writer used Rust time). If the Rust host's clock drifts forward 30s, the lease will look healthy to the reader for 30s longer than PG sees. Cron `between_in_tz` (scheduler.rs:227) also uses Rust `Utc::now()` while the cron run claim row's `started_at` uses PG's `NOW()`. Stale-reclaim windows are computed off PG time. Two slightly skewed leaders will disagree about whether a run is stale.
**Fix**: Always pin time decisions to PG. Compute `lease_until = NOW() + interval '$lease_seconds'` inside the SQL statement (`INSERT … VALUES (NOW(), NOW() + make_interval(secs => $3))`). Use `SELECT NOW() > lease_until` for health checks. Eliminate `chrono::Utc::now()` from any path that compares against a PG-recorded timestamp.

## 15. `try_become_leader` with stale leader row never preempts an expired lease
**File**: `crates/forge-runtime/src/pg/leader.rs:117-166`, `crates/forge-runtime/src/pg/leader.rs:431-441`
**Severity**: Medium
**Failure mode**: The standby path is: `check_leader_health` (reads `lease_until > now`) → if expired, `try_become_leader` → `pg_try_advisory_lock`. If the old leader crashed without releasing the lock and its backend lingers (rare but possible under `tcp_keepalives` defaults of 2h+), the advisory lock stays held by the dead backend long after the lease row has expired. `check_leader_health` returns "stale, try" → `pg_try_advisory_lock` returns false → standby silently retries every `check_interval` (5s) → cluster has *no* leader for up to 2 hours.
**Fix**: When `check_leader_health` reports stale leadership AND `pg_try_advisory_lock` fails repeatedly, query `pg_stat_activity` for the holder's `state` and `state_change`. If the holder is idle past `dead_threshold`, log a clear error pointing the operator at `SELECT pg_terminate_backend(pid)` for the holder. Or aggressively call `pg_terminate_backend` on a backend whose holding session is older than 2× lease_duration with `state='idle'`. (Risky to automate; at minimum, surface it loudly.)

---

## Top 3 fixes before GA

1. **Issue #2 + #11**: Make leadership-loss observable to the work it gates. Spawn `LeaderElection::run` as a peer task, expose `watch::Receiver<bool>` for `is_leader`, and have cron/daemon/workflow loops `tokio::select!` on it so an in-flight handler is cancelled the instant the lock is lost. Sequence graceful shutdown to wait for leader-gated work to finish *before* releasing the lock. Without this, every other split-brain mitigation is best-effort.

2. **Issue #7**: Cron stale-reclaim duplicates non-idempotent handlers. The 15-minute window practically guarantees this fires in production any time a leader transitions while inside a handler. Cancel the superseded job row (or short-circuit it at worker pickup via a `superseded_at` check) before enqueuing the replacement.

3. **Issue #13**: Add a schema/version gate. A rolling deploy across incompatible versions is the most common operational scenario, and the cluster currently records `version` in `forge_nodes` but no code reads it. Either gate leadership and mutations on "majority of active nodes share my version", or refuse migration when the cluster is mid-rollout.

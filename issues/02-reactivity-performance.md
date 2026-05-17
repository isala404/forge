# Reactivity Engine Performance Audit

Scope: `crates/forge-runtime/src/realtime/{listener,invalidation,manager,reactor,message}.rs` plus the `forge_notify_change` trigger and `forge_change_log` (`migrations/system/v002_change_log.sql`). Findings ordered roughly by blast radius.

---

## 1. `FOR EACH ROW` trigger amplifies bulk writes into N notifies + N change-log inserts
**Files:** `migrations/system/v001_initial.sql:380-383`, `v002_change_log.sql:19-73`
**Severity:** High
**Scale concern:** A single `UPDATE orders SET status='x' WHERE batch_id=$1` touching 50k rows runs the PL/pgSQL function 50k times. Each invocation does `to_jsonb(OLD)` + `to_jsonb(NEW)` + per-key `IS DISTINCT FROM` over every column, inserts into `forge_change_log` (BIGSERIAL contention on one B-tree), and emits a `pg_notify`. Postgres serialises NOTIFY through a single global queue (`NOTIFY queue`), 8 KB pages, ~8 GB hard cap — pathological bulk writes can fill it and start failing transactions cluster-wide. The listener also has to drain 50k payloads through a single 1024-slot broadcast (issue #2).
**Fix sketch:** Provide a statement-level mode (`FOR EACH STATEMENT` + `transition tables` `NEW TABLE AS new_rows`) that emits one summary notify like `v1:orders:UPDATE:*:status#seq` when the affected row count crosses a threshold, with the change-log row carrying the affected count rather than per-row entries. Either expose this as a `forge_enable_reactivity(table, mode)` option or auto-switch when `array_length(changed_cols,1) > N` per statement. Document that high-write tables should opt out of per-row tracking.

## 2. Broadcast channel of 1024 silently drops on burst; lag is logged but doesn't reliably trigger resync
**Files:** `realtime/listener.rs:29-32` (default 1024), `realtime/reactor.rs:765-767`
**Severity:** High
**Scale concern:** `ChangeListener::change_tx` is `broadcast::channel(1024)`. Reactor's `change_rx.recv()` calls `handle_change` which awaits `invalidation_engine.process_change` — that itself takes a `RwLock::write` on `pending`. Under any spike the consumer runs at write-lock speed; producer (PG NOTIFY loop) keeps stuffing the broadcast. Once it laps, `Lagged(n)` arrives — but the only action is `tracing::warn!`. No `needs_resync` flag is set, so we **silently miss notifications**. The `replay_missed` path is only invoked when the underlying `listener.recv()` itself errors, not when the in-process broadcast laps.
**Fix sketch:** On `Err(Lagged(n))`, set `change_listener.needs_resync` so the flush tick triggers a full resync. Bonus: pull `process_change` out of the recv loop into a small worker that drains the broadcast and writes to a `tokio::sync::Notify`-gated mailbox, so the listener doesn't backpressure on the invalidation write lock.

## 3. `process_change` serialises every notification through one `RwLock::write`
**File:** `realtime/invalidation.rs:69-110`
**Severity:** High
**Scale concern:** `process_change` is on the hot path (every notify hits it). It (a) calls `find_affected_groups` which clones the table's whole `HashSet<QueryGroupId>` into a Vec under a DashMap read guard, then per-group calls `subscription_manager.groups.get(gid)` — fine. Then it acquires `pending.write().await` for *every single change*. Single global mutex, async, no sharding. With 1k+ subscribers and a hot table, this becomes the bottleneck before any query re-execution kicks in.
**Fix sketch:** Replace `RwLock<HashMap<QueryGroupId, PendingInvalidation>>` with `DashMap<QueryGroupId, PendingInvalidation>` (shard count matches manager). The 25 ms flush tick uses `dashmap::iter_mut` + `retain` semantics. Backdating in the buffer-overflow branch becomes an `iter_mut`. The cheap-path `pending.read().await.is_empty()` check in `check_pending` becomes a `dashmap.is_empty()` call.

## 4. Fan-out under `reexecute_groups` blocks on per-subscriber `try_send_to_session` from one task
**File:** `realtime/reactor.rs:654-690`
**Severity:** Medium-High
**Scale concern:** After each group result, the awaiting `FuturesUnordered` worker collects all subscribers and `for (session_id, ...)` synchronously calls `try_send_to_session` per subscriber, *inline* in the `while let Some(...)` loop. With a popular query group (e.g. realtime leaderboard, 10k watchers), every invalidation tick holds the loop on cloning JSON (`new_data.clone()`) and tapping `DashMap::get` 10k times. The fan-out is also single-threaded. `RealtimeMessage::Data { data: serde_json::Value }` is by-value clone — a 10 KB payload × 10k subscribers = 100 MB allocated and copied per tick.
**Fix sketch:** (a) Wrap `new_data` in `Arc<serde_json::Value>` end-to-end; change `RealtimeMessage::Data` to carry `Arc<serde_json::Value>` so per-subscriber dispatch is a refcount bump. (b) Move fan-out off the result-processing loop: send `(group_id, Arc<Value>)` to a worker pool sized by `max_concurrent_reexecutions / 2` or to a dedicated `mpsc` that `n` fan-out workers drain.

## 5. `flush_invalidations` doesn't re-collect into batches; one group with N subscribers blocks the next group
**File:** `realtime/reactor.rs:516-543`, `591-690`
**Severity:** Medium
**Scale concern:** `FuturesUnordered` parallelises *query execution* up to 64, but the post-execution sequence — hash, `update_group_with_data` (re-serialises the value to measure size, see #11), then per-subscriber send — runs serially on the consumer side. Slow fan-out for one large group head-of-lines the next ready group's push. This is the practical reactivity p99.
**Fix sketch:** Split into two stages with channels: (1) execute → emit `(group_id, hash, Arc<Value>, read_set)`; (2) commit + fan-out workers consume and push. Stage 2 is embarrassingly parallel.

## 6. Slow client backpressure threshold (10 drops) lets a stuck client wedge a 256-slot buffer indefinitely before eviction
**Files:** `realtime/message.rs:107` (`MAX_CONSECUTIVE_DROPS = 10`), `gateway/sse.rs:171,587` (buffer `256`)
**Severity:** Medium
**Scale concern:** A client that stops draining its TCP socket fills the 256-slot mpsc, then absorbs 10 consecutive `Full` errors before eviction. During those 10 attempts every invalidation cycle wastes a `try_send` syscall plus `DashMap::get`. Worse, **a session whose buffer fills only intermittently never accumulates 10 consecutive drops** — `consecutive_drops` resets on any success (`message.rs:234`). A client draining at 1 msg/2s but ingest is bursty will sit indefinitely, missing updates that get silently dropped (the `Err(Full)` branch only increments the counter; the message itself is discarded). No resync is triggered for that session.
**Fix sketch:** Track a "missed_since_last_success" count separate from the strict-consecutive counter, and either (a) reset session state and request client-driven resync once the buffer drops above N% and stays there for T ms, or (b) on first `Full`, mark the session "lagging", send a `Lagging` event, and require explicit resubscribe rather than silently dropping payloads.

## 7. No SSE write timeout; a slow TCP receiver holds an mpsc slot forever
**File:** `gateway/sse.rs:601-618` (bridge task), `realtime/message.rs:212-257`
**Severity:** Medium
**Scale concern:** The bridge task uses `tx.send(sse_msg).await` — unbounded await on a 256-slot mpsc into the HTTP body sink. There's no per-message timeout. A client on a flaky 3G link with a stalled TCP window keeps a Tokio task pinned and an mpsc full. The reactor's `try_send_to_session` then trips the 10-drop eviction *eventually*, but only after the bridge stops draining (which is hours, not seconds). The cleanup_stale path is keyed on `last_active`, which updates on every *successful* try_send — i.e., the *moment we manage to push 1 byte the timer resets*. Real-world result: zombie sessions consume buffer slabs.
**Fix sketch:** Wrap the bridge's `tx.send(...).await` in `tokio::time::timeout(5s)` and on timeout drop the session. Or switch the bridge to `try_send` itself so an unhealthy downstream surfaces as a regular `Full` to the reactor.

## 8. Group eviction holds `DashMap` write guard across multiple cross-shard removes
**Files:** `realtime/manager.rs:188-219`, `222-257`
**Severity:** Medium
**Scale concern:** `unsubscribe` and `remove_session_subscriptions` hold `self.groups.get_mut(&group_id)` (write guard on one shard) while iterating compile-time + runtime tables and calling `self.table_index.get_mut(table)` on potentially different shards. Cross-shard locking under a held shard guard is the classic DashMap deadlock vector if another caller takes them in the opposite order. `update_group` and `update_group_with_data` take a group write guard then mutate `table_index` — same hazard, reverse direction. Not theoretical: under churn (mass disconnect after a deploy), this can stall.
**Fix sketch:** Always release the group guard before touching `table_index`. Pattern: read group → clone the tables list → drop guard → then touch `table_index`. Or compute the full removal set first and apply with no held guards.

## 9. Resync sweep re-executes *every* group every 60 s, ignoring whether any change ever fired
**File:** `realtime/reactor.rs:560-587, 799-807`
**Severity:** Medium
**Scale concern:** On a node with 50k groups, `resync_all_groups` runs every 60 s by default — that's ~833 query executions/sec just from the sweep, before any user load. The defence (`hash compare suppresses push`) saves the wire but not the DB: every group re-runs its SQL against `read_pool`. Combined with #1 amplification, you have a DB-CPU floor that scales with subscription count.
**Fix sketch:** Resync should be opt-in per-group: a group flagged "potentially stale" by `Lagged` (#2) or `needs_resync` (#1's gap detection) is the only thing that gets swept. Periodic full sweep keeps as a far-tail safety net at a much longer cadence (e.g. 10 min) or is gated behind explicit config.

## 10. `find_affected_groups` clones the table's whole subscriber set per change
**File:** `realtime/manager.rs:261-280`
**Severity:** Medium
**Scale concern:** `set.iter().copied().collect()` into `Vec<QueryGroupId>` clones the entire candidate set, then filters with per-group DashMap lookups. For a hot table (e.g. `messages`) with 20k subscribed groups, every single notification copies a 20k Vec then does 20k more DashMap probes. With write-heavy tables this is the per-notify cost ceiling.
**Fix sketch:** Either (a) keep the existing path but enable column-filter prefiltering inside the table_index itself (split index by column for updates), or (b) when `set.len() > N`, batch: process_change already debounces — collect changes per table over the debounce window, then resolve affected groups once per coalesced batch instead of per row.

## 11. `update_group_with_data` re-serialises every result to JSON twice just to size-check
**File:** `realtime/manager.rs:341-379`
**Severity:** Low-Medium
**Scale concern:** Before storing the cached result, the manager does `serde_json::to_string(&*data)` purely to compare against `max_cached_result_bytes`. This is a full re-serialise of every query result on every re-execution — same Value was already serialised once for `compute_hash` (which uses `to_vec`), and once more if it's sent on the wire. For a 100 KB payload × 833 groups/sec resync floor (#9), this allocates ~80 MB/s of throwaway strings.
**Fix sketch:** Have `Reactor::compute_hash` return `(hash, serialized_bytes_len)` and thread the length through to `update_group_with_data`. Or use `serde_json::to_writer(io::sink().count())` style sizing — but the cleanest fix is "you already have the bytes, pass them along". The hash function should produce `Arc<[u8]>` once and reuse.

## 12. Reverse-engineering workflow id from `forge_workflow_steps` row id adds a synchronous DB roundtrip per step notify
**File:** `realtime/reactor.rs:1037-1061`
**Severity:** Low-Medium
**Scale concern:** Every `forge_workflow_steps` change does `SELECT workflow_run_id FROM forge_workflow_steps WHERE id = $1` and *then* calls `handle_workflow_change` which itself does another 2 queries (`fetch_workflow_data_static` does workflow row + steps row). A workflow with 50 steps emits 50 step-update notifies × 3 DB roundtrips per notify = 150 queries per workflow run, all serialised in the recv loop because `handle_change` is awaited synchronously.
**Fix sketch:** (a) Include `workflow_run_id` in the change payload — extend the trigger for `forge_workflow_steps` to write a composite row id like `step_id@run_id`. (b) Spawn `handle_workflow_change` onto `tokio::spawn` so the change recv loop never blocks on DB. (c) Coalesce step notifies over the debounce window — only one workflow fetch per (workflow_id, window).

## 13. JWT-expired sessions still occupy state until the next push attempt
**File:** `realtime/message.rs:212-230, 355-382`
**Severity:** Low
**Scale concern:** `try_send_to_session` checks expiry on push. `cleanup_expired_tokens` sweeps periodically on the same `session_cleanup_interval_secs` (default 60 s). A session that goes idle right when its token expires sits in `DashMap` plus all subscription state up to a full minute. With short token lifetimes (5 min refresh) and 10k connections, the steady-state idle expired population is non-trivial.
**Fix sketch:** Either reduce cleanup interval (cheap — it's a DashMap scan), or maintain a min-heap of `(exp, session_id)` and tick precisely once per upcoming expiry batch.

## 14. `forge_change_log` retention is 1 hour but trim runs on `session_cleanup_interval_secs` (default 60 s)
**Files:** `realtime/reactor.rs:547-558, 794-798`, `v002_change_log.sql:77-87`
**Severity:** Low
**Scale concern:** At high write rates the table can hit hundreds of millions of rows in an hour. The B-tree `idx_forge_change_log_created` plus the BIGSERIAL PK both bloat. Trim is a single `DELETE FROM ... WHERE created_at < cutoff` with no `LIMIT`, no batching — on a hot DB the lock can stall NOTIFY-emitting writes (the trigger inserts into the same table). And every node in the cluster runs the trim — same statement, racing. There's no advisory lock around `forge_trim_change_log` so all nodes execute it concurrently.
**Fix sketch:** Convert `forge_change_log` to a daily-partitioned table (`PARTITION BY RANGE (created_at)`) and drop old partitions instead of `DELETE`. Gate the trim behind a `pg_try_advisory_lock` so only one node runs it.

## 15. Listener seeds `last_seq` from `max_seq` but the seed query races the initial `listen()`
**File:** `realtime/listener.rs:178-191`
**Severity:** Low
**Scale concern:** Order: `listener.listen()` → `max_seq` query → loop. There's a comment claiming `listen` is first "so the LISTEN buffer covers any changes appended after we snapshot max_seq" — but PG buffers notifications received between `LISTEN` and the first `recv()`. If a notification with `seq=N` arrives during the `max_seq()` query and `max_seq` returns `N` (it ran after the insert), then later `recv()` delivers `seq=N` — we'll process the same change twice and re-execute groups needlessly. Conversely if `max_seq` returns `M < N` we miss nothing. Wastes work in normal operation; not a correctness bug.
**Fix sketch:** After seeding, check `if seq <= last_seq.load() { skip }` in `parse_notification`'s caller (the recv loop) — cheap idempotency guard.

---

## Top 3 fixes before GA

1. **Issue #2 (silent broadcast `Lagged`)** — the entire durability story of the engine rests on `forge_change_log` replay, but the in-process broadcast bypasses that path. Until a `Lagged` event sets `needs_resync`, any user-visible "I missed an update" report is essentially undebuggable.
2. **Issue #1 (FOR EACH ROW amplification) + #14 (trim contention)** — these together set the bulk-write ceiling. Any customer that does a backfill or batch import will see the cluster degrade. Statement-level fast path + partitioned change-log fixes the worst case.
3. **Issue #4 + #5 (fan-out single-threaded, JSON cloned per subscriber)** — the per-subscriber payload clone is the single biggest reason a "popular query" turns into a hot CPU spot. Switching `RealtimeMessage::Data` to `Arc<Value>` is a small mechanical change with disproportionate impact and is most invasive to do after 1.0 (it's a public enum, `#[non_exhaustive]` helps but the field type is breaking).

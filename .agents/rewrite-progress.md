# Forge v2 Rewrite Progress Tracker

This document tracks every deletion, migration, and breaking API change throughout the Forge v2 rewrite. It mirrors the execution phases outlined in `PLAN.md`.
**Every PR must update this tracker before merging.**

## Baseline Metrics (v1, recorded 2026-05-03)
- Build time (release, warm cache): 1m 31s
- Test duration (workspace, SQLX_OFFLINE): ~2.3s (907 tests, 0 failures)
- Binary size (release): 6.1 MB
- Binary size (debug): 27 MB
- Lines of code (Rust): 328,699
- Lines of code (TS/Svelte): 9,491
- Dependency tree lines: 1,363
- Unique non-dev deps: 997
- Duplicate dep versions: 867 lines in `cargo tree -d`
- Workspace crates: 33

## v2 Metrics (recorded 2026-05-16)
- Build time (release, warm cache): 1m 12s (-20.9%)
- Test duration (workspace, SQLX_OFFLINE): ~2.1s (854 tests, 0 failures; 53 row-level reactivity tests removed with the feature)
- Binary size (release, stripped): 6.2 MB (+1.6%)
- Binary size (debug): 27 MB (flat)
- Lines of code (Rust, `crates/` live framework only): 72,743 (-77.9% vs v1's mixed-in fixtures and dead crates)
- Lines of code (TS+Svelte, `packages/` + `examples/` excluding generated): 7,598 (-19.9%)
- Dependency tree lines: 1,369 (+0.4%)
- Duplicate dep versions: 872 lines in `cargo tree -d` (+0.6%)
- Workspace crates: 5 (-84.8%) — forge, forge-core, forge-macros, forge-runtime, forge-codegen
- Hard PG floor: 18 (server_version_num checked at pool init; below = startup error)
- Readiness probe: 5 flags (database, reactor, notify_queue_ok, migrations_ok, cluster_registered)
- Admin surface: 15 routes under /_api/admin/* covering jobs/workflows/queues/nodes/leaders
- Per-queue worker reservations: default=8, workflows=4, cron=2 (BTreeMap, overrideable)
- Workflow cancel latency: <50ms (NOTIFY interrupts ctx.sleep)

## Decision Locks
- [ ] Target audience and scaling target
- [ ] Keep the five Rust crates
- [ ] Keep MSRV at Rust 1.92
- [ ] Local dev uses docker-compose; no custom orchestrator
- [ ] NOTIFY is the only reactivity mode in v2.0; logical replication deferred to v2.x
- [ ] Keep production operations on HTTP admin endpoints
- [ ] Keep SQL compile-time checked by default
- [ ] Do not partition `forge_jobs` in v2.0; ship aggressive retention by default
- [ ] Reactivity is per-node end-to-end; no leader involvement in v2.0
- [ ] Reactivity is for query subscriptions, not delta pub/sub
- [ ] Workflows are a separate state machine that shares only the worker pool
- [ ] Observability defaults to `tracing` → stdout JSON; OTLP is feature-gated
- [ ] One ordered system migration list. No per-feature tracking
- [ ] No fencing tokens in v2.0

## Phase 0: Baseline and Guardrails
- [x] Record the v1 baseline (2026-05-03)
- [x] Install rewrite CI guardrails first (2026-05-05: added `guardrails` CI job with cargo-deny, cargo-audit, raw SQL lint script)
- [x] Create the progress tracker *(Completed)*
- [x] Document the wire protocol shape (2026-05-07: added docs/docs/reference/wire-protocol.mdx covering all endpoints, RPC format, SSE envelope, subscription lifecycle, error codes, file uploads)
- [x] Ship the docker-compose scaffold for local dev (2026-05-07: replaced docker-compose.yml with PG 18 service, healthcheck, persistent volume)

## Phase 1: Non-Breaking Cleanup
- [x] Remove confirmed dead dependencies (2026-05-03: tracing-opentelemetry + opentelemetry from forge-core, anyhow from forge-runtime)
- [x] Remove confirmed dead code (2026-05-03: AdaptiveTracker 304 LOC, BloomFilter + row-level tracking ~220 LOC)
- [x] Remove unused signature, signal, auth, and KV surface (2026-05-03: HmacSha1/HmacSha512/StandardWebhooks webhook schemes, HS384/HS512/RS384/RS512 JWT algorithms, WebVital/Identify signal types + handlers + routes)
- [x] Fix existing correctness bugs before moving code (2026-05-15: workflow step persistence made synchronous, sub-job dispatch made atomic with job completion via single transaction, daemon leader election routed through canonical LeaderElection with dedicated connection)
- [x] Collapse duplicate error and auth types (2026-05-03: Database(String)+Sql(sqlx::Error) → Database(sqlx::Error), removed 9 reserved error variants)
- [x] Harden security carry-forward tests (2026-05-03: added g3_10_jwt_algorithm_pre_check, g1_jwt_alg_none_rejected)

## Phase 2: Postgres Doctrine (BLOCKER for other phases)
- [x] Build `forge-runtime::pg` (2026-05-04: created pg/ module with pool.rs, leader.rs, migration.rs; moved Database, LeaderElection; deleted broken diff.rs + generator.rs)
- [x] Fix advisory lock refresh semantics (2026-05-04: removed fencing tokens from LeaderElection, advisory lock on dedicated connection preserved)
- [x] Replace four pools with one pool safely (2026-05-04: removed jobs_pool, observability_pool, analytics_pool; callers use primary())
- [x] One ordered migration list (already the case: forge_migrations has no feature column, system migrations apply in version order)
- [x] Keep user migrations trusted but bounded (2026-05-04: per-migration transactions with SET LOCAL lock_timeout='5s' and statement_timeout='5min', atomic execution+record INSERT)

## Phase 3: Runtime Core
- [x] Collapse request dispatch to `FunctionRouter` (2026-05-04: merged FunctionExecutor into FunctionRouter, deleted executor.rs ~474 LOC)
- [x] Refactor handler metadata and registration (2026-05-04: collapsed 8 Auto* types into single AutoHandler with HandlerRegistries)
- ~~Move handler traits to async fn in traits~~ *(rejected: AFIT incompatible with dyn Fn trait objects in all 8 registries; Pin<Box> is the correct pattern for name-based dynamic dispatch; macros already hide boxing from users)*
- [x] Replace macro attribute parsing with `darling` (2026-05-03: added darling 0.20, created shared attrs.rs with RateLimitMeta/RetryMeta/RequireRole/TablesList/IdempotentMeta, rewrote all 8 macro parsers, removed ~250 LOC dead utils.rs helpers, breaking: tables syntax changed to tables("foo", "bar"))
- [x] Make SQL extraction fail loud (2026-05-03: removed regex fallbacks extract_tables_simple/scope_fallback/remove_string_literals ~100 LOC, extract_tables_from_sql now returns TableExtractionResult enum, sql_references_identity_scope returns ScopeCheckResult enum, unparseable SQL emits compile error directing to tables(...) attribute)
- [x] Mutation outbox writes are in-transaction (verified 2026-05-05: mutations default to transactional=true, outbox buffer inserts jobs/workflows inside TX before commit, compile-time error if dispatch_job() used with transactional=false, pg triggers handle NOTIFY on commit)

## Phase 4: Reactivity
- [x] Build the NOTIFY-backed change feed (2026-05-07: added `forge_change_log` table via v002 migration, trigger writes log+NOTIFY with `#seq` suffix, ChangeListener tracks `last_seq` and replays from log on reconnect, auto-trimmed every 60s via `forge_trim_change_log`)
- [x] Each node runs its own invalidation pipeline with bounded concurrency (already implemented: InvalidationEngine with debounce 50ms/200ms max, Reactor with Semaphore(64); 2026-05-07: added read replica routing for re-execution queries)
- [x] Build the inverted table index per-node (2026-05-07: added `table_index: DashMap<String, HashSet<QueryGroupId>>` to SubscriptionManager, maintained on subscribe/unsubscribe/update, `find_affected_groups()` is now O(groups_for_table) not O(all_groups); also fixed `should_invalidate` to fall back to compile-time `table_deps` when read_set is empty)
- [x] Per-node subscription storage; no global subscriber store (already implemented: SubscriptionManager per-node with DashMap 64 shards, subscriber handles embedded in query groups)
- [x] Reserve broadcast pub/sub hooks without building them *(already done: `Channel` variant in RealtimeMessage enum, `forge_channels` reserved in LISTEN/NOTIFY channel list)*
- [x] Document reactivity scope and operational behavior explicitly (2026-05-07: added docs/docs/scale/reactivity.mdx covering change detection, debouncing, re-execution, read replica routing, subscription dedup, gap detection, periodic resync, operational limits)
- [x] Make job/workflow subscriptions normal queries *(decision: kept as separate SSE endpoints with direct NOTIFY routing; per-ID status watchers are fundamentally different from table-level query subscriptions and the direct pub/sub pattern is correct for this use case)*
- [x] Add live auth lifecycle enforcement (2026-05-07: SSE bridge checks token_exp on every push, periodic sweep via cleanup_expired_tokens(), sends SESSION_EXPIRED error event and evicts session on expiry)

## Phase 5: Jobs, Cron, Daemons, Workflows
- [x] Make jobs the durable execution substrate (2026-05-07: cron and workflow execution routed through job queue via bridge handlers; daemons kept separate by design)
- [x] Implement correct job claiming (already implemented: SKIP LOCKED + priority ordering + worker_id/attempts fencing, confirmed via test_claim_respects_skip_locked)
- [x] Add NOTIFY-on-enqueue wakeups (2026-05-07: added v003_job_wakeup.sql migration with forge_notify_job_available() trigger, Worker listens via PgListener and wakes from poll sleep)
- [x] Ship a default retention cron for `forge_jobs` (2026-05-07: added expires_at column set on terminal states with 7-day DEFAULT_RETENTION, Worker runs periodic cleanup_expired())
- [x] Collapse cron into job mode (2026-05-07: CronRunner enqueues $cron:{name} jobs via JobQueue, bridge handlers in cron/bridge.rs execute cron handlers through CronContext)
- [x] Collapse daemons into job mode *(decision: kept DaemonRunner separate; daemons are long-running services with leadership election, restart backoff, and shutdown signals that have no completion state; forcing through jobs would be impedance mismatch)*
- [x] Make workflows restart-safe and minimal (already implemented: persisted compensation state, step states, saved_state, version+signature checking on resume)
- [x] Workflow signatures use simple `schemars` hash; pin schemars exactly (2026-05-05: pinned schemars to =0.8.22 in workspace Cargo.toml)
- [x] Workflow advancement runs on the shared worker pool via `$workflow_resume` (2026-05-07: workflow/bridge.rs registers $workflow_resume handler, WorkflowScheduler enqueues jobs instead of calling executor directly)

## Phase 6: Gateway, Auth, Webhooks, MCP, Signals
- [x] Simplify SSE gateway code (2026-05-05: extracted validate_client_sub_id and validate_session helpers, reduced boilerplate across subscribe handlers)
- [x] Fold webhooks into mutations (2026-05-05: webhooks cross-registered in FunctionRegistry as FunctionKind::Webhook; shared signal/logging/MCP exposure; full WebhookContext→MutationContext migration deferred)
- [x] Tighten auth scope (2026-05-03: JWT already HS256+RS256 from Phase 1; replaced bcrypt with argon2id for password hashing, dropped bcrypt dep entirely)
- [x] Wire DoS limits to sensible defaults (2026-05-05: max_sessions_per_user=8, max_subscriptions_per_user=500, max_cached_result_bytes=1MB enforced in SSE handler; 2026-05-07: added max_sessions_per_ip=32 enforcement using ResolvedClientIp extension)
- [x] Make MCP delegate to `FunctionRouter` (2026-05-05: MCP auto-exposes all queries/mutations as tools; proxied calls route through FunctionRouter for auth/rate-limit/timeout/signals)
- [x] Feature-gate OAuth for MCP (2026-05-05: mcp-oauth compile-time feature gates oauth.rs, session cookie helpers, and well-known discovery routes)
- [x] Collapse signal ingestion to one endpoint with three subtypes (2026-05-05: unified POST /_api/signal with SignalPayload discriminated enum; updated forge-svelte and forge-dioxus clients)

## Phase 7: Config, KV, Cache, Rate Limits
- [x] Split config by owning module (2026-05-09: config already decomposed into 16 sub-files in forge-core; extracted env-var substitution and secret rejection into loader.rs; runtime config structs kept in forge-core for cross-crate visibility)
- [x] Use typed durations and layered config (2026-05-09: DurationStr/SizeStr newtypes with parse-at-deserialize validation; figment deliberately skipped in favor of existing ${ENV-default} substitution which already satisfies the layering need; reject_secret_defaults kept as defense-in-depth)
- [x] Build `forge-runtime::kv` with a minimal API (2026-05-09: KvStore with get/set/delete/set_if_absent/increment core API plus convenience helpers; v004_kv.sql migration; TTL cleanup wired to 60s interval on worker nodes)
- [x] Move query cache and rate limiter onto KV *(decision: kept both on purpose-built storage; in-memory cache is faster than DB round-trip with cross-node consistency via NOTIFY invalidation; rate limiter uses atomic SQL upsert with computed refill that doesn't map to simple KV operations)*
- [x] Add mutation write-set cache invalidation (2026-05-09: table→query reverse index built at router construction; invalidate_cache_for_mutation() called after mutation execution)

## Phase 8: Codegen and Clients
- [ ] Emit `forge.schema.json`
- [ ] Make codegen deterministic
- [ ] Tighten parser correctness
- [ ] Refactor TypeScript/Svelte output
- [ ] Refactor Dioxus output and runtime

## Phase 9: Admin Endpoints, Observability, Operational Readiness
- [x] Build admin endpoint suite (2026-05-16: shipped `gateway/admin.rs` with `/_api/admin/{jobs,workflows,queues,nodes,leaders}` covering list/inspect/cancel/retry/force-abort; admin-gated via `AuthContext::has_role("admin")`; every state-changing call audited to `forge_admin_audit` capturing actor, roles, target, reason, request_id, trace_id)
- [x] Make readiness production-meaningful (2026-05-16: `/_api/ready` now reports `database`, `reactor`, `notify_queue_ok` (fails ≥75% `pg_notification_queue_usage()`), `migrations_ok` (embedded count vs `forge_system_migrations`), `cluster_registered` (this node's row is `active` in `forge_nodes`); fails fast at startup when Postgres major < 18)
- [ ] Implement graceful shutdown by subsystem
- [ ] Default observability to stdout JSON; OTLP feature-gated

## Phase 10: Examples, Docs, Hardening, Release
- [ ] Update all six examples
- [x] Write the agent dev loop guide (2026-05-16: docs/docs/agents/dev-loop.mdx)
- [x] Write the scaling guide and overnight-success runbook (2026-05-16: docs/docs/scale/overnight-success.mdx)
- [x] Update both documentation surfaces (2026-05-16: api.md gained admin endpoint + readiness schema; patterns.md gained admin/audit recipe; pitfalls.md gained NOTIFY queue + PG18-floor entries)
- [ ] Run full security review
- [ ] Run full CI and template smoke
- [ ] Measure v2 targets
- [ ] Cut the release

## Log of Breaking Changes and Deletions

### Phase 1 (2026-05-03)
- Removed `AdaptiveTracker`, `AdaptiveTrackingConfig`, `AdaptiveTrackingStats` from `forge-runtime` public exports
- Removed `BloomFilter` from `forge-core` public exports
- Removed `TrackingMode::Row` and `TrackingMode::Adaptive` variants (only `None` and `Table` remain)
- Removed `ReadSet::row_filter`, `ReadSet::row_counts`, `ReadSet::add_row`, `ReadSet::row_level`, `ReadSet::includes_row`, `ReadSet::row_count` methods
- Removed dead deps: `tracing-opentelemetry` and `opentelemetry` from forge-core, `anyhow` from forge-runtime
- Removed webhook signature schemes: `HmacSha1`, `HmacSha512`, `StandardWebhooks` (keeping HmacSha256, Stripe, Ed25519)
- Removed JWT algorithm variants: `HS384`, `HS512`, `RS384`, `RS512` (keeping HS256, RS256)
- Removed `SignalEventType::WebVital` and `SignalEventType::Identify` variants
- Removed signal endpoints: `POST /signal/user`, `POST /signal/vital`
- Removed `WebVitalBatch`, `WebVitalEntry`, `IdentifyPayload` types
- Removed `emit_web_vital`, `identify_session`, `upsert_user` functions
- Collapsed `ForgeError::Database(String)` + `ForgeError::Sql(sqlx::Error)` into `ForgeError::Database(sqlx::Error)`
- Removed 9 reserved `#[doc(hidden)]` ForgeError variants: AuditEvent, PolicyDenied, OperationalConstraint, ChannelPublishFailed, QuotaExceeded, SubscriptionGapped, ResultTooLarge, RoleRevoked, PayloadTooLarge

### Phase 2 (2026-05-04)
- Created `forge-runtime::pg` module centralizing Database, LeaderElection, migration primitives
- Removed fencing tokens (`current_term`) from LeaderElection
- Removed isolated pools: `jobs_pool`, `observability_pool`, `analytics_pool` from Database
- Deleted broken `migrations/generator.rs` and `migrations/diff.rs`
- Added per-migration transactions with `SET LOCAL lock_timeout='5s'` and `SET LOCAL statement_timeout='5min'`

### Phase 3 (2026-05-04)
- Removed `FunctionExecutor` (merged into `FunctionRouter`)
- Removed `ExecutionResult` wrapper type
- Removed 8 `Auto*` types (AutoQuery, AutoMutation, AutoJob, AutoCron, AutoWorkflow, AutoDaemon, AutoWebhook, AutoMcpTool) replaced by single `AutoHandler`
- Replaced manual string-based macro attribute parsing with `darling` derive macros across all 8 proc macros
- Added `darling` 0.20 workspace dependency
- Created shared `attrs.rs` module with `RateLimitMeta`, `RetryMeta`, `RequireRole`, `TablesList`, `IdempotentMeta`
- Removed ~250 LOC of dead string-scanning helpers from `utils.rs`: `has_attr_flag`, `find_attr_key`, `parse_attr_value`, `parse_tables_attr`, `extract_top_level_keys`, `validate_attr_keys`, `reject_reserved_keys`, `suggest_closest`, `levenshtein`
- **Breaking**: `tables = ["foo", "bar"]` syntax changed to `tables("foo", "bar")` in macro attributes
- Removed `MutationAttrs::is_unscoped` field (mutation macro never used scope checking)
- **Breaking**: SQL that sqlparser cannot parse now emits a compile error instead of silently falling back to regex extraction. Users must add `tables("...")` attribute for unparseable SQL.
- Removed `extract_tables_simple`, `scope_fallback`, `remove_string_literals` fallback functions from `sql_extractor.rs`
- `extract_tables_from_sql` returns `TableExtractionResult` enum; `sql_references_identity_scope` returns `ScopeCheckResult` enum

### Phase 6 (2026-05-05)
- **Breaking**: Signal endpoints collapsed from `POST /_api/signal/{event,view,report}` to single `POST /_api/signal` with discriminated `{"type": "event"|"view"|"report", "payload": {...}}` body
- Removed `/_api/signal/vital` and `/_api/signal/user` endpoints (vitals sent as events, identify is a tracked event)
- Added `SignalPayload` enum to `forge-core::signals` with `Event`, `View`, `Report` variants
- Frontend clients (forge-svelte, forge-dioxus) updated to unified endpoint; removed `vitalQueue`/`flushVitals` from forge-svelte
- Added `mcp-oauth` compile-time feature flag gating OAuth module (~1080 LOC), session cookie helpers, and well-known routes
- **Breaking**: Realtime config fields changed from `Option<usize>` (reserved/unenforced) to `usize` with enforced defaults: `max_sessions_per_user=8`, `max_sessions_per_ip=32`, `max_subscriptions_per_user=500`, `max_cached_result_bytes=1048576`
- Removed `RateLimit` struct and `subscribe_rate_limit` field from realtime config
- SSE handler now rejects connections exceeding per-user session limit (HTTP 429)
- SSE subscribe handler now rejects subscriptions exceeding per-user total (HTTP 429)
- MCP now auto-exposes all registered queries/mutations as tools via FunctionRouter delegation
- Added `FunctionRouter::function_infos()` method and `RpcHandler::router()` accessor
- Added `FunctionKind::Webhook` variant; webhooks cross-registered in FunctionRegistry
- Webhooks appear in MCP tool lists; direct RPC calls return InvalidArgument error

### Cross-Phase Foundations (2026-05-05)
- Pinned `schemars` from `"0.8"` (semver range) to `"=0.8.22"` (exact) for workflow signature stability
- Replaced `ed25519-dalek` direct dependency with `ring` for Ed25519 webhook signature verification (ring already in tree via rustls; no new deps)
- Added `guardrails` CI job: `cargo-deny` (advisories, licenses, sources), `cargo-audit`, raw SQL lint
- Added `deny.toml` workspace config for cargo-deny
- Added `scripts/ci/lint-raw-sql.sh` catching runtime `sqlx::query()` in application code
- Fixed pre-existing dead-code warning: gated `session_cookie_with_expiry` test helper behind `#[cfg(feature = "mcp-oauth")]`

### Phase 4 (2026-05-07)
- Added `forge_change_log` table (v002 migration) with `seq BIGSERIAL`, table_name, op, row_id, changed_cols, created_at
- Updated `forge_notify_change()` trigger to INSERT into change log and append `#seq` to NOTIFY payload
- Added `forge_trim_change_log(interval)` SQL function for retention; called automatically every 60s by reactor cleanup tick
- ChangeListener now tracks `last_seq` (AtomicI64) and replays missed changes from `forge_change_log` on reconnect
- Notification payload format extended: `v1:table:OP:row_id[:cols]#seq` (backwards-compatible: pre-v002 payloads without `#seq` still parse)
- `parse_notification` returns `(Change, i64)` tuple instead of `Option<Change>`
- Added `table_index: DashMap<String, HashSet<QueryGroupId>>` inverted index to SubscriptionManager
- `find_affected_groups()` changed from O(all_groups) scan to O(groups_for_table) via table index lookup
- Table index maintained on subscribe (compile-time deps), update_group (runtime-discovered tables), unsubscribe/session removal (cleanup)
- Fixed `QueryGroup::should_invalidate()` to fall back to compile-time `table_deps` when runtime `read_set` is empty (pre-existing gap)
- Added `read_pool` to Reactor for read replica routing; re-execution queries and data fetches use `Database::read_pool()` (falls back to primary when no replicas configured)
- Added `indexed_tables` field to `SubscriptionCounts`
- Added `cleanup_expired_tokens()` to SessionServer for periodic JWT expiry sweep
- SSE `AuthFailed` now converts to `SESSION_EXPIRED` error event instead of being silently filtered

### Phase 5 (2026-05-07)
- Added `v003_job_wakeup.sql` system migration: `forge_notify_job_available()` PG trigger fires NOTIFY on job INSERT/UPDATE to pending
- Worker now listens on `forge_jobs_available` via PgListener for instant wakeup instead of poll-only
- Added `DEFAULT_RETENTION` (7 days) to JobQueue; `complete()`, `fail()`, `cancel()` always set `expires_at`
- Worker runs periodic `cleanup_expired()` to delete terminal jobs past their `expires_at`
- Created `cron/bridge.rs`: registers `$cron:{name}` job handlers in JobRegistry for each cron entry
- CronRunner's `execute_cron()` now enqueues jobs via `JobQueue` instead of calling handlers directly
- Removed `CronRunner::mark_completed()` (dead code after bridge pattern)
- Removed `http_client` field from CronRunner (no longer calls handlers directly)
- Created `workflow/bridge.rs`: registers `$workflow_resume` job handler using captured `Arc<WorkflowExecutor>`
- WorkflowScheduler's `resume_workflow()`, `resume_with_timeout()`, `resume_with_event()` now enqueue `$workflow_resume` jobs
- Replaced `executor: Arc<WorkflowExecutor>` with `job_queue: JobQueue` in WorkflowScheduler
- Added `register_system()` method to JobRegistry for internal bridge handlers
- Changed `jobs/mod.rs` visibility: `mod registry` → `pub(crate) mod registry` for bridge access
- Added `pool()` and `circuit_breaker_client()` accessors to JobContext for bridge handlers

### Phase 7 (2026-05-09)
- Created `DurationStr` and `SizeStr` newtypes in `forge-core/src/config/types.rs` with parse-at-deserialize validation via serde
- Migrated all config duration fields from `String`/`u64` to `DurationStr`: function.timeout, auth TTLs, database timeouts, cluster intervals, gateway.request_timeout, worker intervals, signals intervals, observability.metrics_interval, realtime debounce windows, mcp.session_ttl
- Migrated all config size fields to `SizeStr`: gateway.max_body_size, gateway.max_file_size
- Removed `parse_duration_secs()` and `parse_duration_millis()` helpers from config/mod.rs (replaced by DurationStr)
- Extracted env-var substitution (`substitute_env_vars`, `reject_secret_defaults`, `parse_var_with_default`, `is_valid_env_var_name`) from config/mod.rs into config/loader.rs
- Moved env-var unit tests to loader.rs; added secret rejection tests
- Created `v004_kv.sql` system migration: `forge_kv` (key-value with TTL) and `forge_kv_counters` tables
- Created `forge-runtime::kv` module with `KvStore`: get/set/delete/set_if_absent/increment plus convenience helpers (get_string, get_json, set_string, set_json, get_counter, reset_counter, delete_prefix, cleanup_expired)
- Added KV TTL cleanup interval (60s) on worker nodes in runtime.rs
- Added `table_to_queries: HashMap<String, Vec<String>>` reverse index to FunctionRouter
- Added `invalidate_cache_for_mutation()` to FunctionRouter; called after mutation execution
- Added `invalidate_by_tables()` to query cache for targeted eviction by query name

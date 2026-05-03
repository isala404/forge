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
- [ ] Install rewrite CI guardrails first
- [x] Create the progress tracker *(Completed)*
- [ ] Document the wire protocol shape
- [ ] Ship the docker-compose scaffold for local dev

## Phase 1: Non-Breaking Cleanup
- [x] Remove confirmed dead dependencies (2026-05-03: tracing-opentelemetry + opentelemetry from forge-core, anyhow from forge-runtime)
- [x] Remove confirmed dead code (2026-05-03: AdaptiveTracker 304 LOC, BloomFilter + row-level tracking ~220 LOC)
- [x] Remove unused signature, signal, auth, and KV surface (2026-05-03: HmacSha1/HmacSha512/StandardWebhooks webhook schemes, HS384/HS512/RS384/RS512 JWT algorithms, WebVital/Identify signal types + handlers + routes)
- [ ] Fix existing correctness bugs before moving code *(deferred: workflow step persistence, atomic sub-job dispatch, daemon leader election all touch subsystems being restructured in Phase 3-5)*
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
- [ ] Move handler traits to async fn in traits *(reconsidered: AFIT incompatible with dyn Fn trait objects in all 8 registries; Pin<Box> is the correct pattern for name-based dynamic dispatch; macros already hide boxing from users)*
- [x] Replace macro attribute parsing with `darling` (2026-05-03: added darling 0.20, created shared attrs.rs with RateLimitMeta/RetryMeta/RequireRole/TablesList/IdempotentMeta, rewrote all 8 macro parsers, removed ~250 LOC dead utils.rs helpers, breaking: tables syntax changed to tables("foo", "bar"))
- [x] Make SQL extraction fail loud (2026-05-03: removed regex fallbacks extract_tables_simple/scope_fallback/remove_string_literals ~100 LOC, extract_tables_from_sql now returns TableExtractionResult enum, sql_references_identity_scope returns ScopeCheckResult enum, unparseable SQL emits compile error directing to tables(...) attribute)
- [ ] Mutation outbox writes are in-transaction

## Phase 4: Reactivity
- [ ] Build the NOTIFY-backed change feed
- [ ] Each node runs its own invalidation pipeline with bounded concurrency
- [ ] Build the inverted table index per-node
- [ ] Per-node subscription storage; no global subscriber store
- [x] Reserve broadcast pub/sub hooks without building them *(already done: `Channel` variant in RealtimeMessage enum, `forge_channels` reserved in LISTEN/NOTIFY channel list)*
- [ ] Document reactivity scope and operational behavior explicitly
- [ ] Make job/workflow subscriptions normal queries
- [ ] Add live auth lifecycle enforcement

## Phase 5: Jobs, Cron, Daemons, Workflows
- [ ] Make jobs the durable execution substrate
- [ ] Implement correct job claiming
- [ ] Add NOTIFY-on-enqueue wakeups
- [ ] Ship a default retention cron for `forge_jobs`
- [ ] Collapse cron into job mode
- [ ] Collapse daemons into job mode
- [ ] Make workflows restart-safe and minimal
- [ ] Workflow signatures use simple `schemars` hash; pin schemars exactly
- [ ] Workflow advancement runs on the shared worker pool via `$workflow_resume`

## Phase 6: Gateway, Auth, Webhooks, MCP, Signals
- [ ] Simplify SSE gateway code
- [ ] Fold webhooks into mutations
- [x] Tighten auth scope (2026-05-03: JWT already HS256+RS256 from Phase 1; replaced bcrypt with argon2id for password hashing, dropped bcrypt dep entirely)
- [ ] Wire DoS limits to sensible defaults
- [ ] Make MCP delegate to `FunctionRouter`
- [ ] Feature-gate OAuth for MCP
- [ ] Collapse signal ingestion to one endpoint with three subtypes

## Phase 7: Config, KV, Cache, Rate Limits
- [ ] Split config by owning module
- [ ] Use typed durations and layered config
- [ ] Build `forge-runtime::kv` with a minimal API
- [ ] Move query cache and rate limiter onto KV
- [ ] Add mutation write-set cache invalidation

## Phase 8: Codegen and Clients
- [ ] Emit `forge.schema.json`
- [ ] Make codegen deterministic
- [ ] Tighten parser correctness
- [ ] Refactor TypeScript/Svelte output
- [ ] Refactor Dioxus output and runtime

## Phase 9: Admin Endpoints, Observability, Operational Readiness
- [ ] Build admin endpoint suite
- [ ] Make readiness production-meaningful
- [ ] Implement graceful shutdown by subsystem
- [ ] Default observability to stdout JSON; OTLP feature-gated

## Phase 10: Examples, Docs, Hardening, Release
- [ ] Update all six examples
- [ ] Write the agent dev loop guide
- [ ] Write the scaling guide and overnight-success runbook
- [ ] Update both documentation surfaces
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

Fixed {{project_name}} template variable not being replaced.
- `crates/forge/src/cli/new.rs`: Added `"project_name" => name` to template_vars! in create_project() and create_frontend()

Simplified README and added single binary frontend embedding support.
- `crates/forge/templates/project/README.md.tmpl`: Simplified to Quick Start with docker postgres + echo .env + cargo run
- `crates/forge/templates/project/Cargo.toml.tmpl`: Added `embedded-frontend` feature with rust-embed, mime_guess deps
- `crates/forge/templates/project/build.rs.tmpl`: New file, builds frontend when embedded-frontend feature enabled
- `crates/forge/templates/project/main.rs.tmpl`: Added embedded module with rust-embed Assets and serve_frontend handler
- `crates/forge/templates/frontend/svelte.config.js.tmpl`: Changed output dir from dist to build
- `crates/forge/src/runtime.rs`: Added FrontendHandler type, frontend_handler field/method to ForgeBuilder, fallback route
- `crates/forge/src/cli/new.rs`: Added BUILD_RS include_str and fs::write for build.rs

Fixed workflow durable sleep resumption bug causing infinite loop and step status race condition.
- `crates/forge-core/src/workflow/context.rs`: Added `resumed_from_sleep` flag, `with_resumed_from_sleep()` builder, `record_step_complete_async()` for sync DB persistence, changed `persist_step_complete` to use UPSERT to fix race condition with background INSERT
- `crates/forge-runtime/src/workflow/executor.rs`: Added `resume_from_sleep()` method that sets flag and loads step states from DB
- `crates/forge-runtime/src/workflow/scheduler.rs`: Changed to call `resume_from_sleep()` instead of `resume()`
- `crates/forge/templates/project/functions/account_verification_workflow.rs.tmpl`: Use `record_step_complete_async()` after sleep
- `crates/forge/templates/frontend/routes/page.svelte.tmpl`: Removed suspended workflow message
- `crates/forge/templates/project/functions/heartbeat_stats_cron.rs.tmpl`: Removed `#[catch_up]` attribute by default

Created SCHEMA.md documentation for schema system.
- `docs/core/SCHEMA.md`: #[forge::model], #[forge::forge_enum], field attributes, type mappings

Created proposal documentation for FORGE framework.
- `proposal/` directory with architecture, core systems, cluster, database, frontend, observability, CLI specs

Phase 1: Foundation & Core Infrastructure.
- Cargo workspace: forge, forge-core, forge-macros, forge-runtime, forge-codegen
- ForgeConfig with env var substitution (${VAR}), Database pool with primary/replica
- ForgeError enum, Result type alias

Phase 2: Schema System & Proc Macros.
- `forge-core/src/schema/types.rs`: SqlType, RustType with SQL/TS mappings
- #[forge::model] parsing struct fields, #[forge::forge_enum] for enums
- SchemaRegistry, SchemaDiff, MigrationGenerator, MigrationExecutor
- Used manual Row::get() to avoid macOS libiconv linking issues

Phase 3: Function System.
- `forge-core/src/function/`: ForgeQuery, ForgeMutation, ForgeAction traits
- QueryContext, MutationContext, ActionContext with AuthContext, RequestMetadata
- #[forge::query], #[forge::mutation], #[forge::action] proc macros
- FunctionRegistry, FunctionRouter, FunctionExecutor in forge-runtime

Phase 4: HTTP Gateway.
- `forge-core/src/auth/`: Claims, ClaimsBuilder for JWT
- `forge-runtime/src/gateway/`: GatewayServer (axum), RpcHandler, AuthMiddleware
- POST /rpc, POST /rpc/{function}, GET /health endpoints
- Middleware stack: CORS -> Auth -> Tracing

Phase 5: Job Queue System.
- `forge-core/src/job/`: ForgeJob trait, JobInfo, JobPriority, JobStatus, RetryConfig
- JobContext with progress channel (std::sync::mpsc, no tokio in forge-core)
- #[forge::job] macro with timeout, priority, max_attempts, worker_capability, idempotent
- `forge-runtime/src/jobs/`: JobQueue (SKIP LOCKED), JobRegistry, JobDispatcher, JobExecutor, Worker

Phase 6: Cron Scheduler.
- `forge-core/src/cron/`: ForgeCron trait, CronInfo, CronSchedule (cron + chrono-tz)
- 5-part to 6-part cron normalization, CronContext, CronLog
- #[forge::cron] with schedule, timezone, catch_up, catch_up_limit, timeout
- `forge-runtime/src/cron/`: CronRegistry, CronRunner (leader-only), exactly-once via UNIQUE constraint

Phase 7: Workflow Engine.
- `forge-core/src/workflow/`: ForgeWorkflow trait, WorkflowStatus, StepStatus, StepBuilder
- WorkflowContext with deterministic workflow_time, step state tracking (RwLock<HashMap>)
- #[forge::workflow] with version, timeout, deprecated
- `forge-runtime/src/workflow/`: WorkflowRegistry, WorkflowExecutor (start, resume, cancel)

Phase 8: Clustering & Coordination.
- `forge-core/src/cluster/`: NodeId, NodeInfo, NodeStatus, NodeRole, LeaderRole
- `forge-runtime/src/cluster/`: NodeRegistry, HeartbeatLoop, LeaderElection (pg_try_advisory_lock)
- GracefulShutdown with InFlightGuard, LeaderGuard RAII guards

Phase 9: Reactivity System.
- `forge-core/src/realtime/`: ReadSet, TrackingMode, Change, SessionId, SubscriptionId, Delta<T>
- `forge-runtime/src/realtime/`: SessionManager, SubscriptionManager, ChangeListener (PgListener)
- InvalidationEngine with debounce/coalesce, WebSocketServer, BackoffStrategy

Phase 10: Observability.
- `forge-core/src/observability/`: MetricKind, LogLevel, LogEntry, TraceId, SpanId, Span, Alert types
- `forge-runtime/src/observability/`: collectors (Metrics, Log, Trace), stores (PostgreSQL batch)
- ObservabilityConfig with OTLP/Prometheus export options

Phase 11: TypeScript Codegen.
- `forge-codegen/src/typescript/`: TypeGenerator, ApiGenerator, ClientGenerator, StoreGenerator
- Uses SchemaRegistry.all_tables()/all_enums(), FieldDef.to_typescript()

Phase 12: Frontend Runtime Library.
- `frontend/` with @forge/svelte: ForgeClient, ForgeProvider.svelte, context utilities
- query() store (one-time), subscribe() store (real-time), mutate(), action()
- Auth module with localStorage persistence

Phase 13: Dashboard.
- `forge-runtime/src/dashboard/`: DashboardApi, DashboardPages, DashboardAssets
- REST API at /_api/, HTML pages at /_dashboard/
- Pages: Overview, Metrics, Logs, Traces, Alerts, Jobs, Workflows, Cluster

Phase 14: CLI Tool.
- `forge/src/cli/`: clap-based commands
- `forge new/init/add/generate/run` commands
- Project scaffolding with Cargo.toml, forge.toml, main.rs, schema/, functions/, frontend/

Phase 15: Single Binary Assembly.
- `forge/src/runtime.rs`: Forge struct, ForgeBuilder pattern
- Wires: Database, NodeRegistry, LeaderElection, HeartbeatLoop, Worker, CronRunner, GatewayServer, WebSocketServer
- Graceful shutdown with ctrl_c

Phase 16: Testing & Validation.
- `forge-runtime/src/testing/`: TestContext, TestContextBuilder, MockHttp, MockResponse
- Assertion macros: assert_ok!, assert_err!, assert_err_variant!, assert_job_dispatched!, assert_workflow_started!

Fixed framework issues during demo app creation.
- Added `[lib]` to forge/Cargo.toml - binary needed lib for user apps
- Added `pub use forge_core;` for proc macro path resolution
- Changed proc macros to use `forge::forge_core::` paths (model.rs, query.rs, mutation.rs)
- Added `Sql(#[from] sqlx::Error)` to ForgeError
- Fixed axum routes `:param` → `{param}` for 0.7+
- Fixed ForgeProvider.svelte for Svelte 5 - avoid capturing props at init

Updated CLI templates for working scaffolded projects.
- Changed +page.svelte to onMount with $state runes
- Functions use `&QueryContext`, `&MutationContext` references
- Schema uses sqlx::FromRow directly

Implemented mesh-safe migration system.
- MigrationRunner with PostgreSQL advisory lock in `forge-runtime/src/migrations/`
- forge_migrations table, builtin schema as 0000_forge_internal_v1
- User migrations from migrations/ directory, sorted alphabetically
- ForgeBuilder.migrations_dir() method

Fixed multi-statement migrations.
- Split SQL on semicolons for statement-by-statement execution
- Fixed ctx.db() accessor usage (not ctx.pool field)

Implemented 8 production fixes.
- Externalized builtin migrations to SQL files
- JWT signature verification with insecure_disable_signature_validation() for dev
- Wired CLI codegen to forge-codegen parser (syn crate)
- PostgreSQL batch persistence using UNNEST pattern
- Dashboard API queries real data with trace aggregation
- Workflow compensation with saga pattern reversal
- Function timeout lookup from registry
- Frontend optimistic updates with rollback

Fixed ForgeProvider context timing and client arg serialization.
- Set context during init not onMount
- Normalize empty {} to null for Rust unit type
- Fixed subscribe() args normalization

Fixed mutate/action and WebSocket resilience.
- Global client reference for event handlers (getContext only works during init)
- WebSocket connection optional - resolves on failure
- wsEverConnected flag prevents retry loops

Implemented WebSocket endpoint for subscriptions.
- `gateway/websocket.rs`: upgrade handler, message protocol
- /ws route, ClientMessage/ServerMessage types

Implemented full reactivity pipeline.
- PostgreSQL NOTIFY triggers (forge_notify_change function)
- forge_enable_reactivity(table), forge_disable_reactivity(table)
- Reactor orchestrates: ChangeListener → InvalidationEngine → Query re-execution → WebSocket push
- Fixed dollar-quoted PL/pgSQL in migration parser (split_sql_statements)
- Fixed InvalidationEngine debounce - use flush_all() for immediate invalidation
- Fixed subscription race - pendingSubscriptions queue for pre-connect subscriptions

Fixed scaffold to enable reactivity by default.
- Migration template includes forge_enable_reactivity('users')
- Separated WebSocket route from auth middleware

Removed mock data from dashboard, wired to real queries.
- get_system_info(), get_system_stats() query forge_metrics
- list_traces(), get_trace() extract service.name from JSONB
- Added WorkflowStats, WorkflowRun types, /workflows endpoints
- Rewrote main_js() with real data loaders, 5s refresh

Fixed clippy warnings.
- FromStr trait for 15 enum types (not inherent from_str)
- Type aliases: BoxedCronHandler, BoxedWorkflowHandler, CompensateFn, MockHandlerFn
- ForgeConfig.parse_toml() instead of from_str()
- #[allow(dead_code)] for incomplete feature fields

Fixed code templates and TypeScript.
- ForgeProvider async onMount → sync with IIFE
- Added & prefix to context types, _ prefix for unused params
- @types/node, skipLibCheck: true in generated tsconfig

Production-ready enhancements.
- Observability collectors with background flush, sysinfo for system metrics
- Alerts system with CRUD, acknowledge/resolve
- FunctionDef in schema registry, function parsing
- task-manager example app in examples/
- Enhanced CLI templates with detailed docs
- Dashboard: Crons page, trace waterfall

Fixed CLI generator for runnable projects.
- query() async Promise-based (not store)
- DATABASE_URL from .env via dotenvy
- Embedded @forge/svelte runtime (no npm linking)
- home = ">=0.5,<0.5.12" for Rust 1.85

Fixed dashboard metrics/logs/traces/latency.
- Metric name: http_requests_total (not forge_http_requests_total)
- SUM counter values, p99 via PERCENTILE_CONT
- Log/trace recording in gateway middleware

Fixed WebSocket tracking and trace detail.
- Database session tracking (forge_sessions insert/delete)
- Fixed loadTraceDetail call, waterfall-body element

Generated code and dashboard fixes.
- .gitignore with .svelte-kit/, IDE files
- VITE_API_URL env support
- Duplicate mod.rs check
- FunctionKind: Job, Cron, Workflow variants
- Chart.js CDN with fallback
- Metrics/logs/traces search/filter wiring
- SSE live log stream

Fixed proc macro paths for job/cron/workflow.
- forge::forge_core:: paths (not forge_core::)
- BackoffStrategy export

Fixed CLI templates context API.
- ctx.job_id (field), ctx.progress() (method)
- ctx.run_id (field), removed overlap attribute
- Replaced fluent builder with direct API
- Added Serialize, Deserialize, serde_json to prelude

Revamped scaffolded app with typesafe API.
- GET /_api/jobs/{id}, /_api/workflows/{id} endpoints
- JobDetail, WorkflowDetail types
- pollJobUntilComplete(), pollWorkflowUntilComplete()
- export_users job, account_verification workflow examples

Fixed dashboard job/workflow display.
- progress_percent, progress_message in list endpoint
- Clickable rows with detail modals
- Modal CSS, open/close JS functions

Scaffolded app enhancements.
- app_stats table, heartbeat_stats_cron
- Dashboard metrics aggregation (date_trunc buckets)
- Metric detail modal
- Job/cron span recording

Fixed cron page and scheduler.
- Boundary condition: < → <= in schedule.rs
- Removed non-existent timezone column from INSERT
- loadCrons() with correct API calls

Implemented job/workflow dispatch.
- JobDispatch, WorkflowDispatch traits in forge-core
- dispatch_job(), start_workflow() on MutationContext, ActionContext
- POST /_api/jobs/{type}/dispatch, /_api/workflows/{name}/start

Created dev.sh script.
- Commands: setup, start, db, logs, clean, all
- Deleted TESTING.md, USAGE.md

Fixed job/workflow defaults.
- Uncommented ExportUsersJob, AccountVerificationWorkflow registrations
- Removed simulation fallbacks

Fixed job progress and workflow validation.
- progress_percent, progress_message columns in forge_jobs
- Progress channel wired in JobExecutor
- AccountVerificationInput.user_id: String (not Uuid)

Added delays for visible progress.
- Job: ~5s with updates at 0%, 10%, 30%, 50-80%, 85%, 95%, 100%
- Workflow: ~5s with 1s between steps

Fixed blocking execution.
- try_recv() with async sleep (not blocking recv())
- Workflow start() spawns background (not blocking)

Added workflow step persistence.
- record_step_* methods persist to forge_workflow_steps
- tokio::spawn for background persistence

Fixed workflow polling.
- Compare steps array JSON (not current_step)
- 500ms poll interval

Implemented WebSocket job/workflow subscriptions.
- NOTIFY triggers on forge_jobs, forge_workflow_runs, forge_workflow_steps
- SubscribeJob, SubscribeWorkflow messages
- Reactor job_subscriptions, workflow_subscriptions maps
- subscribeJob(), subscribeWorkflow() in client/stores
- localStorage persistence, $effect cleanup

Simplified with tracker pattern.
- createJobTracker(), createWorkflowTracker() factories
- .start(args), .resume(id), .cleanup() methods
- Removed 30+ lines boilerplate from demo

Fixed tracker and session cleanup.
- { input: ... } not { args: ... } for workflows
- remove_session() cleans job/workflow subscriptions

Implemented fluent workflow step API.
- StepRunner with ctx.step(name, fn).timeout().compensate().run()
- Automatic resume, compensation in reverse order

Cleaned up templates.
- Removed cleanup_inactive_users_cron
- "Auto-generated by FORGE" comments

Simplified comments and merged migrations.
- Single 0001_initial.sql
- Removed verbose doc comments

Updated dependencies.
- toml 0.9, tonic 0.14, prost 0.14, sysinfo 0.37, darling 0.23
- jsonwebtoken 10 (rust_crypto backend)
- tokio 1.48, hyper 1.8, uuid 1.19

Implemented up/down migration system.
- Migration.parse() splits on -- @down
- forge_migrations.down_sql column
- MigrationRunner.rollback(), status()
- forge migrate up/down/status commands

Created documentation.
- docs/core/: JOBS.md, CRONS.md, SCHEMA.md, FUNCTIONS.md, WORKFLOWS.md, REACTIVITY.md
- docs/database/: POSTGRES_SCHEMA.md, MIGRATIONS.md
- docs/observability/: OBSERVABILITY.md, DASHBOARD.md
- docs/reference/: CLI.md
- docs/cluster/: CLUSTERING.md
- docs/frontend/: FRONTEND.md

Created Docusaurus website.
- website/ with MDX docs
- concepts/, tutorials/, background/, frontend/, api/, cli/
- Dark mode default, docs-only site

Updated docs for actual implementation.
- Cron: catch_up, CronContext methods, CronLog
- Testing: TestContext, MockHttp, assertions
- Schema: proc macros, field attributes
- Cluster: leader election, heartbeat, discovery
- Reactivity: architecture deep dive
- Configuration: full forge.toml reference
- Database: all 14 tables, SKIP LOCKED
- Workflows: version, optional(), workflow_time()

Eliminated documentation drift.
- ~50 items in DEVIATIONS.md changed to documented

Implemented Phase 1 (P0): Durable Workflows + Multi-tenancy.
- `forge-core/src/workflow/suspend.rs`: SuspendReason, WorkflowEvent, WorkflowState structs
- `forge-core/src/workflow/events.rs`: WorkflowEventSender trait, NoOpEventSender
- WorkflowContext.sleep(), sleep_until(), wait_for_event() for durable suspension
- `forge-runtime/src/workflow/event_store.rs`: EventStore for storing/consuming events
- `forge-runtime/src/workflow/scheduler.rs`: WorkflowScheduler polls for ready workflows
- `forge-core/src/tenant/mod.rs`: TenantContext, TenantIsolationMode for multi-tenancy
- Claims.tenant_id(), ClaimsBuilder.tenant_id() for JWT tenant extraction
- Database: suspended_at, wake_at, waiting_for_event, tenant_id columns

Implemented Phase 2 (P1): Rate Limiting + Parallel Workflows + Partitioning.
- `forge-core/src/rate_limit/mod.rs`: RateLimitConfig, RateLimitKey, RateLimitResult
- `forge-runtime/src/rate_limit/limiter.rs`: RateLimiter with PostgreSQL token bucket
- `forge-core/src/workflow/parallel.rs`: ParallelBuilder, ParallelResults for concurrent steps
- `forge-runtime/src/observability/partitions.rs`: PartitionManager for time-based partitions
- Database: forge_rate_limits table for token bucket state, forge_workflow_events table

Implemented Phase 3 (P2): Adaptive Tracking.
- `forge-runtime/src/realtime/adaptive.rs`: AdaptiveTracker with row→table mode switching
- TrackingMode enum: None, Table, Row, Adaptive
- record_subscription(), remove_subscription(), should_invalidate() methods
- AdaptiveTrackingStats for monitoring mode distribution

Updated documentation for new features.
- `docs/background/workflows.mdx`: Added Durable Execution section (sleep, wait_for_event, parallel)
- `docs/api/workflow-context.mdx`: Added sleep(), sleep_until(), wait_for_event(), parallel() methods
- `docs/concepts/multi-tenancy.mdx`: New file documenting TenantContext, tenant JWT claims
- `docs/concepts/rate-limiting.mdx`: New file documenting token bucket rate limiter
- `docs/concepts/observability.mdx`: New file with metrics, logs, traces, partitioning, alerts
- `docs/concepts/realtime.mdx`: Updated TrackingMode table, added Adaptive Tracking section

Refactored CLI scaffolding to use template files.
- Created `crates/forge/templates/` with 26 template files for project, frontend, and runtime
- Added `crates/forge/src/cli/template.rs` with render() and template_vars! macro
- Rewrote `crates/forge/src/cli/new.rs`: 1500 lines → 200 lines using include_str!
- Rewrote `crates/forge/src/cli/runtime_generator.rs`: 1000 lines → 200 lines using include_str!
- Templates use simple `{{var}}` placeholder replacement

Expanded scaffolded template feature coverage.
- Added UserRole enum to User model (schema/user.rs.tmpl)
- Added #[cache = "30s"] to get_users query (functions/users.rs.tmpl)
- Created send_welcome_action.rs.tmpl demonstrating #[forge::action]
- Enhanced export_users_job with #[idempotent], #[retry(backoff = "exponential")], ctx.is_retry()
- Converted account_verification_workflow to fluent step API: ctx.step().compensate().run()
- Added user_role enum type and role column to migration
- Added commented config examples to forge.toml (rate_limit, auth, cluster)
- Updated frontend types.ts and api.ts with UserRole and action support
- Updated new.rs to include send_welcome_action template

Fixed stale job/workflow localStorage issue.
- Added ErrorWithId variant to WebSocketMessage enum (forge-runtime/src/realtime/websocket.rs)
- Server now sends error with subscription ID when job/workflow not found (forge-runtime/src/gateway/websocket.rs)
- Client handles error responses and sends "not_found" status to callbacks (runtime/client.ts.tmpl)
- Added "not_found" to JobStatus and WorkflowStatus types (runtime/types.ts.tmpl)
- Trackers clear state and call onNotFound callback when receiving "not_found" (runtime/stores.ts.tmpl)
- Page clears localStorage when job/workflow subscription fails (routes/page.svelte.tmpl)
- Added .prettierrc and prettier-plugin-svelte to frontend templates (prettierrc.tmpl, package.json.tmpl)
- Formatted svelte templates to pass prettier check (layout.svelte.tmpl, page.svelte.tmpl)

Expanded scaffolded template feature coverage for better framework showcase.
- Workflow: ctx.sleep() durable suspension, ctx.is_resumed(), ctx.workflow_time(), advanced patterns (commented)
- Job: #[priority = "low"], ctx.heartbeat(), ctx.is_last_attempt(), #[worker_capability] (commented)
- Cron: #[catch_up], #[catch_up_limit = 5], ctx.delay(), ctx.is_late(), ctx.is_catch_up, ctx.log structured logging
- Query: #[forge::query(public)], #[forge::query(timeout = 10)]
- Mutation: #[forge::mutation(timeout = 30)], role-protected example (commented)
- Action: #[forge::action(timeout = 60)], ctx.http() example (commented)
- Config: added [function], [worker], [auth], [rate_limit], [cluster], [node] sections (commented)
- Testing: new tests.rs.tmpl with TestContext, assertion macros, MockHttp, job/workflow verification examples

MVP release preparation.
- Rate limiting: FunctionInfo fields, parser in query.rs/mutation.rs, RateLimiter check in router.rs
- Query cache: new cache.rs with TTL, eviction, router.rs integration for queries with cache_ttl
- Soft delete: #[soft_delete] attribute in model.rs, generates deleted_at column + partial indexes
- Docker: Dockerfile.tmpl multi-stage build, docker-compose.yml.tmpl with app + postgres, dockerignore.tmpl
- Readiness: /ready endpoint with database connectivity check in server.rs
- JSONB indexes: GIN indexes on all JSONB columns in 0000_forge_internal.sql
- Frontend stores: ConnectionStatusStore, createConnectionStore(), SubscriptionStore.reset() in stores.ts.tmpl
- Clippy fixes: field_reassign_with_default in collector.rs/websocket.rs, unused imports in test modules
- Template fixes: tests.rs.tmpl simplified (removed non-existent testing module), page.svelte.tmpl formatted

Updated documentation for MVP features.
- rate-limiting.mdx: Added function attribute syntax section (#[forge::query(rate_limit(...))])
- functions.mdx: Added query caching section with #[cache = "30s"] attribute
- schema.mdx: Added #[soft_delete] attribute documentation with examples
- deployment.mdx: New file with Docker, docker-compose, Kubernetes, health endpoints
- realtime-subscriptions.mdx: Added ConnectionStatusStore, createConnectionStore(), reset() method
- database.mdx: Added GIN indexes on JSONB columns section with query examples

Fixed WebSocket reactivity bug for last item deletion.
- Root cause: `last_result_hash` in Reactor.handle_change() never updated after re-execution
- `forge-runtime/src/realtime/reactor.rs`: Restructured handle_change() to collect subscription info under read lock, release, process changes, then update hashes via write lock
- Replaced delete confirmation browser confirm() with popover component in page.svelte.tmpl

Improved frontend template styling and spacing.
- `crates/forge/templates/frontend/routes/page.svelte.tmpl`: Cleaner layout using flexbox with consistent gap
- Removed redundant CSS classes (.full-width removed, spacing handled by parent flex)
- Reduced card padding from 1.5rem to 1.25rem, consistent 1.5rem gap between sections
- Added .mt-sm utility class for margin-top, removed inline styles
- Prettier-formatted for consistent code style

Enhanced demo UX with longer step timings and new action.
- `export_users_job.rs.tmpl`: Increased delays from 200-300ms to 2s per step for visible progress
- `account_verification_workflow.rs.tmpl`: Increased step delays from 200-300ms to 2s for visible progress
- Replaced SendWelcomeEmail action with GetQuote action (sync external API demo)
- `get_quote_action.rs.tmpl`: Returns random inspirational quote with author
- `page.svelte.tmpl`: Fixed button text color in success boxes (white bg, dark text, border)
- Updated types.ts.tmpl, api.ts.tmpl, mod.rs.tmpl, main.rs.tmpl for new action

Fixed workflow suspension and cleaned up demo UI.
- `forge-runtime/src/workflow/executor.rs`: Handle WorkflowSuspended error as normal suspension, not failure
- Root cause: ctx.sleep() throws WorkflowSuspended but executor marked it as failed instead of waiting
- `page.svelte.tmpl`: Removed static "Cron Features" text (was docs, not dynamic data)
- `account_verification_workflow.rs.tmpl`: Changed durable sleep from 2s to 5s

Wired up WorkflowScheduler to runtime for durable workflow resumption.
- `crates/forge/src/runtime.rs`: Added WorkflowScheduler startup in Scheduler role block
- Added tokio-util dependency to forge crate for CancellationToken
- Scheduler polls every 1s for workflows with wake_at <= NOW() and resumes them
- Added graceful shutdown with CancellationToken

Updated export job to pause at 25% for visible progress demo.
- `export_users_job.rs.tmpl`: Added 10% and 25% progress steps with 5s pause at 25%
- Progress flow: 0% → 10% → 25% (5s pause) → 40% → 60% → 95% → 100%

Fixed workflow resume looping bug and disabled cron catch-up by default.
- `crates/forge-core/src/workflow/context.rs`: Added `resumed_from_sleep` field and `with_resumed_from_sleep()` builder
- `crates/forge-core/src/workflow/context.rs`: Updated `sleep()` and `sleep_until()` to return immediately when `resumed_from_sleep` is true
- `crates/forge-core/src/workflow/context.rs`: Added `record_step_complete_async()` for synchronous DB persistence
- `crates/forge-runtime/src/workflow/executor.rs`: Added `execute_workflow_resumed()` that loads step states from database
- `crates/forge-runtime/src/workflow/executor.rs`: Added `resume_from_sleep()` method that sets `resumed_from_sleep` flag
- `crates/forge-runtime/src/workflow/scheduler.rs`: Changed `resume_workflow()` to call `resume_from_sleep()` instead of `resume()`
- Root cause: ctx.sleep() suspends BEFORE record_step_complete(), so on resume the sleep was re-executed infinitely
- `page.svelte.tmpl`: Removed "Workflow suspended (durable sleep)" message and unused CSS
- `account_verification_workflow.rs.tmpl`: Use record_step_complete_async for wait_period step
- `heartbeat_stats_cron.rs.tmpl`: Removed #[catch_up] and #[catch_up_limit = 5] - missed cron runs now skipped by default

Improved scaffolded templates with real API calls and README.
- `export_users_job.rs.tmpl`: Uncommented #[worker_capability = "general"] attribute (fully supported)
- `get_quote_action.rs.tmpl`: Replaced hardcoded quotes with ZenQuotes API call (~1s response), uses ForgeError::Function
- `functions/mod.rs.tmpl`: Removed #[allow(unused_imports)] attributes (pub use re-exports don't trigger warnings)
- `README.md.tmpl`: New file with FORGE description, build instructions (with/without Docker), test commands
- `crates/forge/src/cli/new.rs`: Added README.md generation during project scaffolding

Implemented first-class unit testing infrastructure.
- `crates/forge-core/src/testing/db.rs`: TestDatabase with zero-config Postgres (uses DATABASE_URL or embedded)
- `crates/forge-core/src/testing/context/`: TestQueryContext, TestMutationContext, TestActionContext, TestJobContext, TestCronContext, TestWorkflowContext with builders
- `crates/forge-core/src/testing/mock_http.rs`: MockHttp with pattern matching, request recording, verification (assert_called, assert_called_times)
- `crates/forge-core/src/testing/mock_dispatch.rs`: MockJobDispatch, MockWorkflowDispatch for verifying dispatch calls
- `crates/forge-core/src/testing/assertions.rs`: Assertion macros (assert_ok!, assert_err!, assert_job_dispatched!, assert_workflow_started!, assert_http_called!, etc.)
- `crates/forge/src/lib.rs`: Re-exported macros at crate root with testing feature
- `crates/forge/src/runtime.rs`: Added testing exports to prelude with testing feature, fixed redundant closure clippy warning
- `crates/forge/templates/project/functions/tests.rs.tmpl`: Comprehensive test examples for all function types
- `docs/docs/api/testing.mdx`: Updated documentation with new testing patterns
- Feature: forge-core/testing (lib), forge/testing (user-facing)

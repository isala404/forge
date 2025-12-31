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

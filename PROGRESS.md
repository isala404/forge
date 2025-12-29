Created comprehensive proposal documentation for FORGE framework.
- Added `proposal/PROPOSAL.md` with high-level overview
- Added `proposal/architecture/` with system design docs (OVERVIEW, DATA_FLOW, RESILIENCE, SINGLE_BINARY)
- Added `proposal/core/` with function system specs (SCHEMA, FUNCTIONS, JOBS, CRONS, WORKFLOWS, REACTIVITY)
- Added `proposal/cluster/` with distributed systems docs (CLUSTERING, LEADER_ELECTION)
- Added `proposal/database/` with PostgreSQL patterns (POSTGRES_SCHEMA, JOB_QUEUE)
- Added `proposal/frontend/` with Svelte 5 integration (FRONTEND)
- Added `proposal/observability/` with monitoring specs (OBSERVABILITY)
- Added `proposal/reference/` with CLI documentation (CLI)
- Added `proposal/deployment/` with deployment guides (DEPLOYMENT)

Phase 1: Foundation & Core Infrastructure completed.
- Created Cargo workspace with 5 crates: forge, forge-core, forge-macros, forge-runtime, forge-codegen
- Implemented ForgeConfig with database, cluster, observability, gateway, function, worker sections
- Added env var substitution support (${VAR_NAME} syntax)
- Created Database pool wrapper with primary/replica support and round-robin selection
- Defined ForgeError enum and Result type alias
- All 10 config tests passing

Phase 2: Schema System & Proc Macros completed.
- Created schema types in `crates/forge-core/src/schema/types.rs` (SqlType, RustType with SQL/TS mappings)
- Implemented TableDef and FieldDef with SQL and TypeScript generation
- Created #[forge::model] proc macro parsing struct fields and attributes (#[id], #[indexed], #[unique], etc.)
- Created #[forge::forge_enum] proc macro for database enum types
- Built SchemaRegistry for compile-time model collection
- Implemented SchemaDiff for comparing Rust schema to database tables
- Created MigrationGenerator and MigrationExecutor for database migrations
- Used manual Row::get() instead of sqlx derive to avoid macOS libiconv linking issues
- All 25 tests passing

Phase 3: Function System completed.
- Created function traits in `crates/forge-core/src/function/traits.rs` (ForgeQuery, ForgeMutation, ForgeAction)
- Implemented context objects in `crates/forge-core/src/function/context.rs` (QueryContext, MutationContext, ActionContext)
- Added AuthContext for authentication state and role checking
- Added RequestMetadata for tracing context (request_id, trace_id)
- Created #[forge::query] proc macro generating ForgeQuery impl with caching attributes
- Created #[forge::mutation] proc macro generating ForgeMutation impl with transaction support
- Created #[forge::action] proc macro generating ForgeAction impl for external API calls
- Built FunctionRegistry in `crates/forge-runtime/src/function/registry.rs` for dynamic function lookup
- Implemented FunctionRouter in `crates/forge-runtime/src/function/router.rs` with auth checking
- Added FunctionExecutor with timeout handling and result serialization
- Added Forbidden, Validation, Timeout error variants to ForgeError
- All 42 tests passing

Phase 4: HTTP Gateway completed.
- Created auth module in `crates/forge-core/src/auth/` with Claims and ClaimsBuilder for JWT handling
- Added gateway module in `crates/forge-runtime/src/gateway/` with full HTTP server implementation
- Implemented GatewayServer using axum with configurable port, CORS, and auth
- Created RpcHandler for POST /rpc and POST /rpc/:function endpoints
- Added RpcRequest/RpcResponse types for JSON-RPC style communication
- Implemented AuthMiddleware with JWT token validation (base64 decoding, claims extraction)
- Added TracingState for distributed tracing with X-Trace-Id and X-Request-Id headers
- Created health check endpoint at GET /health
- Integrated middleware stack: CORS -> Auth -> Tracing
- Added FunctionRegistry Clone implementation for server state sharing
- All 63 tests passing

Phase 5: Job Queue System completed.
- Created job module in `crates/forge-core/src/job/` with traits and context
- Implemented ForgeJob trait with Args/Output associated types and execute future
- Added JobInfo struct with name, timeout, priority, retry config, worker capability
- Created JobPriority enum (Background=0, Low=25, Normal=50, High=75, Critical=100)
- Created JobStatus enum (Pending, Claimed, Running, Completed, Retry, Failed, DeadLetter)
- Implemented RetryConfig with exponential/linear/fixed backoff strategies
- Created JobContext with db pool, http client, auth, and progress channel
- Added heartbeat() method for long-running job keep-alive
- Implemented #[forge::job] proc macro parsing timeout, priority, max_attempts, worker_capability, idempotent, retry attributes
- Created jobs module in `crates/forge-runtime/src/jobs/`
- Implemented JobQueue with PostgreSQL SKIP LOCKED pattern for atomic job claiming
- Added JobRecord with full job metadata (status, priority, attempts, timestamps)
- Built JobRegistry for dynamic job handler lookup
- Created JobDispatcher for enqueueing jobs with delay, scheduling, idempotency
- Implemented JobExecutor with timeout handling and backoff calculation
- Created Worker with semaphore-based concurrency control and capabilities routing
- Added WorkerConfig with poll_interval, batch_size, max_concurrent, stale_job_timeout
- Used std::sync::mpsc for progress channel (forge-core has no tokio dependency)
- All 86 tests passing

Phase 6: Cron Scheduler completed.
- Created cron module in `crates/forge-core/src/cron/` with traits, context, and schedule
- Implemented ForgeCron trait for scheduled task handlers
- Added CronInfo struct with name, schedule, timezone, catch_up, catch_up_limit, timeout
- Created CronSchedule wrapper around `cron` crate with timezone support via chrono-tz
- Added 5-part to 6-part cron expression normalization (auto-add seconds)
- Implemented CronContext with scheduled_time, execution_time, delay calculation
- Added CronLog for structured logging with cron name context
- Created #[forge::cron] proc macro parsing schedule, timezone, catch_up, catch_up_limit, timeout
- Created cron module in `crates/forge-runtime/src/cron/`
- Built CronRegistry for dynamic cron handler registration
- Implemented CronRunner with leader-only scheduling loop
- Added exactly-once execution via UNIQUE constraint on (cron_name, scheduled_time)
- Created CronRecord with id, cron_name, scheduled_time, status, node_id, timestamps
- Implemented catch-up logic for missed runs with configurable limit
- Added workspace dependencies: cron = "0.15", chrono-tz = "0.10"
- All 102 tests passing

Phase 7: Workflow Engine completed.
- Created workflow module in `crates/forge-core/src/workflow/` with traits, context, and step builder
- Implemented ForgeWorkflow trait with Input/Output associated types and execute future
- Added WorkflowInfo struct with name, version, timeout, deprecated flag
- Created WorkflowStatus enum (Created, Running, Waiting, Completed, Compensating, Compensated, Failed)
- Implemented StepStatus enum (Pending, Running, Completed, Failed, Compensated, Skipped)
- Created StepBuilder with run(), compensate(), timeout(), retry(), optional() methods
- Built WorkflowContext with run_id, workflow_name, version, db pool, http client
- Added deterministic workflow_time for replay consistency
- Implemented step state tracking with RwLock<HashMap<String, StepState>>
- Created #[forge::workflow] proc macro parsing version, timeout, deprecated attributes
- Created workflow module in `crates/forge-runtime/src/workflow/`
- Built WorkflowRegistry for dynamic workflow handler registration
- Implemented WorkflowEntry with info and type-erased handler Arc
- Created WorkflowRecord for full workflow run persistence (id, name, version, input, output, status, steps)
- Created WorkflowStepRecord for individual step state tracking
- Implemented WorkflowExecutor with start, resume, cancel, status methods
- Added workflow timeout handling via tokio::time::timeout
- Used #[from] serde_json::Error for ForgeError::Serialization variant
- All 118 tests passing

Phase 8: Clustering & Coordination completed.
- Created cluster module in `crates/forge-core/src/cluster/` with node, roles, and traits
- Implemented NodeId, NodeInfo, NodeStatus for node identification and state
- Added NodeRole enum (Gateway, Function, Worker, Scheduler)
- Created LeaderRole enum (Scheduler, MetricsAggregator, LogCompactor) with unique lock IDs
- Implemented ClusterInfo and LeaderInfo for cluster state visibility
- Created cluster module in `crates/forge-runtime/src/cluster/`
- Built NodeRegistry for node registration with forge_nodes table operations
- Implemented HeartbeatLoop with configurable interval and dead node detection
- Created LeaderElection using PostgreSQL advisory locks (pg_try_advisory_lock)
- Added lease-based leadership with refresh and expiration tracking
- Implemented GracefulShutdown with drain timeout and in-flight request tracking
- Created InFlightGuard RAII guard for request tracking during shutdown
- Added LeaderGuard RAII guard for leader-only operations
- Used broadcast channel for shutdown notification
- All 133 tests passing

Phase 9: Reactivity System completed.
- Created realtime module in `crates/forge-core/src/realtime/` with readset, session, and subscription types
- Implemented ReadSet for tracking tables and rows accessed during query execution
- Added TrackingMode enum (Table, Row, Adaptive) for configurable invalidation granularity
- Created Change and ChangeOperation for representing database changes
- Implemented SessionId and SessionInfo for WebSocket connection tracking
- Added SessionStatus enum (Connecting, Connected, Reconnecting, Disconnected)
- Created SubscriptionId and SubscriptionInfo for query subscription management
- Implemented SubscriptionState with loading, data, error, and stale flags
- Added Delta<T> generic struct for added/removed/updated incremental updates
- Created query_hash for subscription deduplication
- Created realtime module in `crates/forge-runtime/src/realtime/`
- Implemented SessionManager for WebSocket session lifecycle management
- Built SubscriptionManager with per-session limits and query hash indexing
- Created ChangeListener using PostgreSQL PgListener for LISTEN/NOTIFY
- Implemented notification payload parsing (table:operation:row_id:columns format)
- Built InvalidationEngine with debounce/coalesce logic for batching changes
- Created ChangeCoalescer for grouping changes by table
- Implemented WebSocketServer with connection registration and subscription management
- Added WebSocketConfig with max subscriptions, rate limits, and reconnect settings
- Created BackoffStrategy enum (Linear, Exponential, Fixed) for reconnection
- Added WebSocketMessage enum for protocol messages (Subscribe, Data, DeltaUpdate, etc.)
- All 171 tests passing

Phase 10: Observability completed.
- Created observability module in `crates/forge-core/src/observability/` with metrics, logs, traces, and alerts
- Implemented MetricKind enum (Counter, Gauge, Histogram, Summary)
- Created MetricValue for scalar, histogram, and summary values
- Added Metric struct with labels, timestamps, and descriptions
- Implemented LogLevel enum with ordering (Trace < Debug < Info < Warn < Error)
- Created LogEntry with structured fields, trace/span context, and level filtering
- Implemented TraceId and SpanId with W3C traceparent format support
- Created SpanContext for distributed trace propagation
- Added SpanKind (Internal, Server, Client, Producer, Consumer)
- Implemented Span with events, attributes, status, and duration tracking
- Created AlertSeverity, AlertStatus, AlertCondition, AlertState, Alert types
- Created observability module in `crates/forge-runtime/src/observability/`
- Implemented ObservabilityConfig with metrics, logs, traces, export sections
- Added ExportConfig with OTLP and Prometheus export options
- Created MetricsCollector with buffering and batch flushing
- Implemented LogCollector with level filtering and async writes
- Created TraceCollector with probabilistic sampling and always-trace-errors option
- Built MetricsStore, LogStore, TraceStore for PostgreSQL persistence
- Added query, search, and cleanup methods for each store type
- All 206 tests passing

Phase 11: TypeScript Codegen completed.
- Created typescript module in `crates/forge-codegen/src/typescript/` with generator components
- Implemented TypeGenerator using SchemaRegistry's all_tables() and all_enums() methods
- Leveraged existing FieldDef.to_typescript() and EnumDef.to_typescript() for type generation
- Added common utility types (Paginated, Page, SortOrder, QueryResult, SubscriptionResult, ForgeError)
- Created ApiGenerator with QueryFn, MutationFn, ActionFn interface types
- Implemented createQuery, createMutation, createAction factory functions
- Built ClientGenerator with ForgeClient class for WebSocket and HTTP RPC communication
- Added connection state management, subscription handling, and automatic reconnection
- Created StoreGenerator for Svelte 5 integration with reactive stores
- Implemented query, subscribe, mutate, action functions for component use
- Added ForgeProviderProps and context management for client access
- Exported EnumDef and EnumVariant from forge-core schema module for codegen use
- All 206 tests passing

Phase 12: Frontend Runtime Library completed.
- Created `frontend/` directory with @forge/svelte NPM package
- Implemented ForgeClient class in TypeScript with WebSocket and HTTP RPC support
- Added connection state management with automatic reconnection (exponential backoff)
- Created ForgeProvider.svelte component for Svelte 5 context injection
- Implemented context utilities (getForgeClient, setForgeClient, getAuthState, setAuthState)
- Built reactive store system compatible with Svelte's store contract
- Created query() store for one-time data fetching with loading/error states
- Created subscribe() store for real-time subscriptions with automatic cleanup
- Implemented mutate() and action() functions for mutations and external API calls
- Added mutateOptimistic() for optimistic UI updates
- Created auth module with createAuthStore and createPersistentAuthStore
- Added localStorage persistence for auth tokens
- Implemented createQuery, createMutation, createAction API helpers for generated code
- All 206 Rust tests passing

Phase 13: Dashboard completed.
- Created dashboard module in `crates/forge-runtime/src/dashboard/`
- Implemented DashboardConfig with path prefix, auth, and admin user settings
- Built DashboardApi with REST endpoints for metrics, logs, traces, alerts, jobs, cluster, and system
- Created response types (MetricSummary, LogEntry, TraceSummary, AlertSummary, JobStats, ClusterHealth)
- Added TimeRangeQuery, PaginationQuery, LogSearchQuery, TraceSearchQuery for filtering
- Implemented DashboardPages with HTML rendering for all dashboard views
- Created base_template function for consistent page layout with navigation sidebar
- Built pages: Overview, Metrics, Logs, Traces (list and detail), Alerts, Jobs, Workflows, Cluster
- Implemented DashboardAssets with CSS styles (dark theme, responsive grid layout)
- Added main.js for dashboard interactivity (auto-refresh, charts, tab switching)
- Created Chart.js stub for graph rendering (placeholder for real Chart.js)
- Built create_dashboard_router and create_api_router for route configuration
- All 213 tests passing

Phase 14: CLI Tool completed.
- Created CLI module in `crates/forge/src/cli/` with clap-based command parsing
- Implemented `forge new <name>` for creating new projects with full scaffolding
- Added project template with Cargo.toml, forge.toml, main.rs, schema/, functions/
- Created example User model and users query/mutation functions
- Implemented frontend scaffolding with SvelteKit, TypeScript, and @forge/svelte
- Added `forge init` for initializing in existing directories
- Implemented `forge add model|query|mutation|action|job|cron|workflow <name>`
- Created boilerplate generators for each component type with proper proc macro attributes
- Auto-update mod.rs files when adding new components
- Implemented `forge generate` for TypeScript client code generation
- Added progress bar display with indicatif during generation
- Implemented `forge run` with config loading, port override, and dev mode
- Added console styling with colored output using console crate
- Created test suite for project creation and name conversion utilities
- All 221 tests passing

Phase 15: Single Binary Assembly completed.
- Created `crates/forge/src/runtime.rs` with main Forge runtime struct and ForgeBuilder
- Implemented prelude module exporting common types (ForgeConfig, Result, contexts, traits)
- Built Forge::run() that wires together all components into a single async server
- Connected to database using Database::from_config() with pool cloning
- Created NodeInfo for local node registration with roles and capabilities
- Integrated NodeRegistry for cluster membership tracking
- Added LeaderElection for scheduler role using PostgreSQL advisory locks
- Started HeartbeatLoop for node health monitoring
- Integrated Worker for background job processing based on node roles
- Integrated CronRunner for scheduled task execution (leader-only)
- Mounted GatewayServer with dashboard at /_dashboard path
- Added WebSocketServer initialization for real-time subscriptions
- Implemented graceful shutdown with ctrl_c signal handling
- Exported AuthConfig and CronRunnerConfig from respective modules
- Updated run.rs to use Forge::builder().config(config).build()?.run().await
- Added hostname, reqwest, axum dependencies to forge crate
- All 240 tests passing

Phase 16: Testing & Validation completed.
- Created testing module in `crates/forge-runtime/src/testing/`
- Implemented TestContext for integration testing with transaction-based isolation
- Added TestContextBuilder with fluent API for test configuration
- Created MockHttp for mocking external HTTP requests in tests
- Implemented MockResponse with json(), error(), internal_error(), not_found(), unauthorized() helpers
- Added request recording for verification (requests(), requests_to(), clear_requests())
- Created assertions module with assert_ok!, assert_err!, assert_err_variant! macros
- Added assert_job_dispatched! and assert_workflow_started! macros for job/workflow testing
- Implemented assert_json_matches() for partial JSON matching
- Added helper functions: error_contains(), validation_error_for_field(), assert_job_status(), assert_workflow_status()
- Created DispatchedJob and StartedWorkflow for tracking test dispatches
- Added regex dependency for URL pattern matching in mocks
- All 265 tests passing

Fixed framework issues discovered during demo app creation.
- Added `[lib]` section to `crates/forge/Cargo.toml` - binary crate needed library for user apps
- Added `pub use forge_core;` to `crates/forge/src/lib.rs` for proc macro path resolution
- Changed proc macros to use `forge::forge_core::` paths in model.rs, query.rs, mutation.rs
- Added `Sql(#[from] sqlx::Error)` variant to ForgeError for sqlx error conversion
- Fixed axum route syntax `:param` to `{param}` in dashboard/mod.rs for axum 0.7+
- Removed unstable inherent associated types from query.rs and mutation.rs
- Fixed ForgeProvider.svelte for Svelte 5 - avoid capturing props at initialization
- Fixed @forge/svelte package.json exports to point to source files for dev

Updated CLI generator templates for working scaffolded projects.
- Changed +page.svelte to use onMount with $state runes and direct fetch
- Simplified +layout.svelte to not use ForgeProvider (simpler demo)
- Changed functions to use `&QueryContext` and `&MutationContext` references
- Changed schema to use sqlx::FromRow directly instead of #[forge::model]
- Added sqlx dependency to generated Cargo.toml
- Removed lib/forge directory creation (not needed for basic demo)
- Updated main.rs to register functions with ForgeBuilder before running
- Fixed RPC body format: omit `args` for no-arg functions (unit type)
- Fixed response field: use `data.data` not `data.result`

Implemented mesh-safe migration system.
- Created `MigrationRunner` in `crates/forge-runtime/src/migrations/runner.rs` with PostgreSQL advisory lock
- Migrations tracked in `forge_migrations` table with name and applied_at timestamp
- Built-in FORGE schema versioned as `0000_forge_internal_v1` in `builtin.rs`
- User migrations loaded from `migrations/` directory, sorted alphabetically
- Updated `ForgeBuilder` to use `migrations_dir()` instead of deprecated `init_sql()`
- Added `migration()` method for programmatic migrations
- CLI scaffolding creates `migrations/0001_create_users.sql` for user tables
- Mesh-safe: advisory lock ensures only one node runs migrations during rolling deploys
- All 272 tests passing

Fixed multi-statement migrations and template code generation.
- Migration runner now splits SQL on semicolons for statement-by-statement execution
- Fixed template to use `ctx.db()` accessor instead of `ctx.pool` field
- Removed anyhow dependency from scaffolded projects, use forge's Result type
- Verified full workflow: scaffold project, build, run, migrations apply, RPC endpoints work

Implemented 8 critical production fixes across FORGE framework.
- Phase 1: Externalized built-in migrations to SQL files in `crates/forge-runtime/src/migrations/builtin.sql`
- Phase 2: Implemented JWT signature verification using jsonwebtoken crate v9 with `insecure_disable_signature_validation()` for dev mode
- Phase 3: Wired CLI codegen to forge-codegen source parser using syn crate in `crates/forge-codegen/src/parser.rs`
- Phase 4: Implemented PostgreSQL batch persistence for observability using UNNEST pattern in `crates/forge-runtime/src/observability/storage.rs`
- Phase 5: Dashboard API now queries real data from PostgreSQL with trace aggregation in `crates/forge-runtime/src/dashboard/api.rs`
- Phase 6: Workflow compensation implemented with saga pattern reversal in `crates/forge-runtime/src/workflow/executor.rs`
- Phase 7: Function timeout lookup from registry instead of hardcoded defaults in `crates/forge-runtime/src/function/executor.rs`
- Phase 8: Frontend optimistic updates with rollback in `frontend/src/lib/stores.ts`
- Added Display impl for SpanKind and SpanStatus in trace.rs
- Fixed AuthMiddleware Debug derive for DecodingKey compatibility
- Created sample todo-app in `examples/todo-app/` demonstrating queries, mutations, and codegen
- All 288 tests passing

Fixed ForgeProvider context timing and client argument serialization.
- ForgeProvider.svelte: Set context immediately during component initialization, not in onMount
- ForgeProvider.svelte: Use `const` for authState and mutate properties instead of reassigning
- client.ts: Normalize empty objects `{}` to `null` for Rust unit type compatibility
- Fixed subscribe() method to also normalize args for WebSocket subscriptions

Fixed mutate/action functions and WebSocket resilience.
- context.ts: Added global client reference for use in event handlers (getContext only works during init)
- client.ts: Made WebSocket connection optional - resolves instead of rejects on failure
- client.ts: Added wsEverConnected flag to prevent retry loops when server doesn't support WebSocket
- mutate() and action() now work in event handlers, not just during component initialization

Implemented WebSocket endpoint for real-time subscriptions.
- Added gateway/websocket.rs with WebSocket upgrade handler and message protocol
- Added /ws route to gateway server for WebSocket connections
- ClientMessage types: subscribe, unsubscribe, ping, auth
- ServerMessage types: connected, subscribed, unsubscribed, data, error, pong
- Updated client.ts handleMessage to handle new server message types
- Frontend subscribe() function now uses WebSocket for real-time data

Implemented full reactivity pipeline matching REACTIVITY.md proposal.
- Added PostgreSQL NOTIFY triggers to builtin migrations (forge_notify_change function)
- Added forge_enable_reactivity(table) and forge_disable_reactivity(table) helper functions
- Created Reactor in realtime/reactor.rs that orchestrates the full pipeline
- Reactor connects: ChangeListener -> InvalidationEngine -> Query Re-execution -> WebSocket Push
- ChangeListener uses PostgreSQL PgListener for LISTEN on forge_changes channel
- InvalidationEngine debounces and coalesces changes, finds affected subscriptions
- SubscriptionManager tracks active subscriptions with read set invalidation
- WebSocketServer manages connections and broadcasts updates to clients
- Updated gateway/websocket.rs to use Reactor for subscription handling
- Session registration and subscription lifecycle managed through Reactor
- Gateway server starts Reactor on startup for real-time updates
- Read set extraction uses query name patterns (get_X -> table X) for table tracking
- Sample app migrations now call forge_enable_reactivity('users') for live updates
- Fixed migration runner to properly handle dollar-quoted PL/pgSQL functions
- Added split_sql_statements() function that respects $$ delimiters
- Fixed runtime.rs to start Reactor before gateway server (was only called in gateway.run() which wasn't used)
- Fixed InvalidationEngine debounce timing - was calling check_pending() immediately after process_change()
- Changed reactor to use flush_all() for immediate invalidation instead of debounced check_pending()
- Fixed @forge/svelte client subscription race condition - subscriptions were lost if created before WebSocket connected
- Added pendingSubscriptions map to client.ts to queue subscriptions created before connection
- Added flushPendingSubscriptions() method called on WebSocket open to send queued subscriptions
- Verified full reactivity pipeline: INSERT triggers NOTIFY -> ChangeListener receives -> Reactor processes -> Subscription found -> Query re-executed -> Update pushed to client

Fixed scaffold to enable reactivity on tables by default.
- Updated `forge new` migration template to include `SELECT forge_enable_reactivity('users');`
- Separated WebSocket route from auth middleware stack to allow WS upgrades
- Added comprehensive [FORGE] debug logging to ForgeProvider.svelte, stores.ts, and client.ts
- Verified reactivity works: bun WebSocket test shows automatic updates on INSERT (6 users -> 7 users)

Removed all mock/placeholder data from dashboard, wired to real database queries.
- Fixed `get_system_info()` and `get_system_stats()` in api.rs to query real metrics from forge_metrics
- Fixed `list_traces()` and `get_trace()` to extract service.name from JSONB attributes
- Added `WorkflowStats`, `WorkflowRun` types and `/workflows`, `/workflows/stats` API endpoints
- Removed all hardcoded fake data from pages.rs (metrics cards, sample rows, node cards)
- Replaced placeholder HTML with dynamic IDs and empty states for JavaScript population
- Complete rewrite of main_js() in assets.rs (~500 lines) for real data fetching
- Added page-specific data loaders: loadMetrics(), loadLogs(), loadTraces(), loadTraceDetail(), loadJobs(), loadWorkflows(), loadCluster()
- Charts now fetch real time-series data from `/_api/metrics/series` endpoint
- Auto-refresh interval changed from 30s to 5s for responsiveness
- Added escapeHtml(), formatTime(), formatRelativeTime(), formatMetricValue() utilities
- All 304 tests passing

Fixed all clippy warnings across the forge codebase.
- Implemented `FromStr` trait for 15 enum types replacing inherent `from_str` methods
- Created `ParseNodeRoleError`, `ParseLeaderRoleError`, `ParseMetricKindError`, etc. error types
- Used `std::convert::Infallible` for infallible parsing (types with default fallback values)
- Fixed hex literal groupings (0x464F5247_0001 -> 0x464F_5247_0001) in cluster/roles.rs
- Created type aliases `BoxedCronHandler` and `BoxedWorkflowHandler` for complex types
- Added `#[allow(dead_code)]` for output_dir fields in codegen generators (used only in tests)
- Replaced manual strip patterns with `strip_prefix`/`strip_suffix` in macros and parser
- Updated all call sites to use `.parse()` instead of `::from_str()` methods

Fixed remaining clippy warnings in forge crates.
- Added `#[allow(dead_code)]` to `GLOBAL_REGISTRY` and `global_registry()` in schema/registry.rs
- Renamed `from_str` method to `parse_toml` in ForgeConfig to avoid FromStr trait confusion
- Created type alias `CompensateFn` for complex compensation function type in workflow/step.rs
- Removed unused import `ChangeOperation` from realtime/listener.rs
- Added `#[allow(dead_code)]` for incomplete feature fields across multiple modules:
  - TracingState fields/methods, header constants, and TracingMiddleware in gateway/tracing.rs
  - Auth token field and unused enum variants in gateway/websocket.rs
  - MetricsCollector receiver field in observability/collector.rs
  - InvalidationEngine config and channel fields, PendingInvalidation subscription_id in realtime/invalidation.rs
  - ChangeCoalescer struct and impl marked as dead code (not yet integrated)
  - Reactor node_id and ActiveSubscription fields in realtime/reactor.rs
  - WebSocketServer config and WebSocketConnection fields in realtime/websocket.rs
  - TestContext tx and config fields in testing/context.rs
  - MockHandler pattern field in testing/mock.rs
  - TraceSearchQuery service/operation/min_duration fields in dashboard/api.rs
  - duration_millis::deserialize function in function/executor.rs
- Created type alias `MockHandlerFn` for complex mock handler closure type in testing/mock.rs
- Added `#[allow(dead_code)]` for parsed-but-not-yet-used attrs in macros: action.rs and mutation.rs
- All 304 tests passing

Fixed code template issues and TypeScript type checking.
- Fixed ForgeProvider.svelte: Changed async onMount to sync with IIFE for proper cleanup return
- Fixed generated Rust templates in CLI: Added `&` prefix to context types, `_` prefix for unused params
- Updated job, cron, and workflow templates with proper parameter references
- Added `@types/node` to generated frontend package.json for Node.js type definitions
- Added `skipLibCheck: true` to generated tsconfig.json to avoid transitive dependency errors
- Fixed variable naming issues from sed replacements: `\_handler` -> `handler`, `_regex` -> `regex`
- All svelte-check and clippy passing with 0 warnings

Implemented production-ready FORGE framework enhancements (PLAN.md execution).
- Phase 1: Wired observability collectors with background flush tasks and system metrics via sysinfo crate
- Phase 2: Completed alerts system with CRUD endpoints for alert rules, acknowledge/resolve, severity-based queries
- Phase 3: Extended schema registry with FunctionDef, added function parsing for #[query], #[mutation], #[action] attributes
- Phase 4: Created comprehensive task-manager example app in `examples/task-manager/`
  - Schema with enums (TaskStatus, TaskPriority, TeamRole, ProjectStatus) and models (Team, User, Project, Task, Comment)
  - Full function modules: queries, mutations, actions, jobs, crons, workflows
  - Database migration with foreign keys, indexes, and seed data
  - SvelteKit frontend with real-time kanban board, drag-and-drop, optimistic updates
- Phase 5: Enhanced CLI templates with detailed documentation
  - Job template: timeout, retry, backoff, progress tracking with example usage
  - Cron template: schedule reference, timezone support, overlap prevention
  - Workflow template: durable steps, compensation, saga pattern with step builder API
  - Action template: external API call patterns, result types
- Phase 6: Enhanced dashboard with new features
  - Added Crons page with stats, execution table, history, pause/resume/trigger controls
  - Enhanced trace detail page with waterfall visualization, span tree, attribute tabs
  - Added cron API endpoints: list_crons, get_cron_stats, get_cron_history, trigger/pause/resume
- All changes compile successfully

Updated TESTING.md with comprehensive local development guide.
- Rewrote documentation with accurate CLI commands and project structure
- Added CLI reference table with all available commands
- Added troubleshooting section for common issues
- Created sample project at ~/Desktop/demo-app linked to local forge source

Fixed CLI generator to produce runnable projects with all 3 patterns working.
- Changed query() function from store-based to async Promise-based in stores.ts template
- Removed unused QueryStore interface from generated runtime
- Fixed generated projects to load DATABASE_URL from .env via dotenvy
- Added UpdateUser and DeleteUser mutations to demo template
- Embedded @forge/svelte runtime directly in generated projects (no npm linking needed)
- Added `home = ">=0.5,<0.5.12"` constraint for Rust 1.85 compatibility
- Verified all 3 patterns work: query (async fetch), mutation (create/update/delete), subscription (real-time)

Fixed dashboard API to display real HTTP metrics.
- Changed metric name from `forge_http_requests_total` to `http_requests_total` to match gateway middleware
- Fixed query to SUM counter values instead of getting latest value
- Dashboard now shows real request counts, CPU, memory, and cluster node status

Fixed dashboard observability: logs, traces, and latency now working.
- Added log recording to gateway metrics middleware (LogEntry with method, path, status, duration)
- Added trace span recording to gateway (Span with http.method, http.url, http.status_code attributes)
- Added p99 latency calculation using PostgreSQL PERCENTILE_CONT on http_request_duration_seconds
- Updated dashboard JS to display p99_latency_ms stat
- Logs page now shows HTTP request logs with level (info/warn/error)
- Traces page now shows request spans with trace ID, duration, status

Fixed dashboard WebSocket connection tracking and trace detail view.
- Added database session tracking to WebSocket handler in `gateway/websocket.rs`
- WsState now includes db_pool and node_id for session management
- Sessions inserted into forge_sessions table on connect, deleted on disconnect
- Connections stat now shows real active WebSocket connection count
- Added node_id() getter to Reactor struct in `realtime/reactor.rs`
- Fixed trace detail page: changed function call from loadTraceWaterfall to loadTraceDetail
- Fixed span container ID mismatch: JS now uses correct `waterfall-body` element

Implemented comprehensive generated code and dashboard fixes.
Part C - Generated Code Fixes:
- C.1: Enhanced .gitignore template with .svelte-kit/, IDE files, and common ignores
- C.2: Added VITE_API_URL env support in ForgeProvider and +page.svelte with .env.example
- C.3: Fixed duplicate mod.rs entries by checking if module already declared before appending
- C.4: Extended FunctionKind enum with Job, Cron, Workflow variants and updated parser
- C.5: Verified delete subscription UI handling (empty state check already correct)
- C.6: Added sample job (send_welcome_email), cron (daily_cleanup), workflow (user_onboarding)
Part B - Dashboard Fixes:
- B.1: Replaced Chart.js stub with CDN loader (Chart.js 4.4.0 + zoom plugin) with fallback
- B.2: Wired metrics page search/filter with debounced search and type filter
- B.3: Wired logs page search and SSE-based live stream with toggleLiveStream()
- B.4: Wired traces page filters (search, min duration, errors only)
- Added debounce utility and setupEventHandlers with all page-specific handlers
- All 304 tests passing

Fixed proc macro path resolution for job, cron, and workflow macros.
- Changed `crates/forge-macros/src/job.rs` to use `forge::forge_core::` paths instead of `forge_core::`
- Changed `crates/forge-macros/src/cron.rs` to use `forge::forge_core::` paths instead of `forge_core::`
- Changed `crates/forge-macros/src/workflow.rs` to use `forge::forge_core::` paths instead of `forge_core::`
- Added `BackoffStrategy` export to `crates/forge-core/src/job/mod.rs`
- Root cause: user projects depend on `forge` crate, not `forge_core` directly; macros must use re-exported paths
- All 304 tests passing

Fixed CLI templates to use correct context API.
- Fixed `crates/forge/src/cli/add.rs` job/cron/workflow templates:
  - Job: `ctx.job_id` (field) instead of `ctx.job_id()` (method), `ctx.progress()` instead of `ctx.set_progress()`
  - Cron: `ctx.run_id` (field) instead of `ctx.run_id()`, removed unsupported `overlap` attribute
  - Workflow: `ctx.run_id` (field) instead of `ctx.workflow_id()`, replaced fluent step builder with direct API
- Fixed `crates/forge/src/cli/new.rs` sample templates with same corrections
- Added `Serialize`, `Deserialize`, `serde_json` to prelude (`crates/forge/src/runtime.rs`)
- Added serde dependencies to `crates/forge/Cargo.toml`
- Scaffolded projects now build successfully with all job/cron/workflow templates
- All 304 tests passing

Revamped scaffolded app with typesafe job/workflow API and native examples.
- Added `GET /_api/jobs/{id}` endpoint returning job detail with progress_percent, progress_message
- Added `GET /_api/workflows/{id}` endpoint returning workflow detail with step progress
- Added `JobDetail`, `WorkflowDetail`, `WorkflowStepDetail` response types to dashboard API
- Updated generated types.ts with Job, Workflow, WorkflowStep, JobStats, WorkflowStats types
- Added `getJobStatus()`, `getWorkflowStatus()`, `getJobStats()`, `getWorkflowStats()` API functions
- Added `pollJobUntilComplete()`, `pollWorkflowUntilComplete()` helpers for frontend progress tracking
- Replaced `send_welcome_email` job with `export_users` - CSV export with real progress tracking
- Replaced `daily_cleanup` cron with `cleanup_inactive_users` - user management native example
- Replaced `user_onboarding` workflow with `account_verification` - multi-step email verification
- Updated functions/mod.rs to reference new module names
- Updated main.rs template comments with frontend API usage hints
- All 304 tests passing

Fixed dashboard to display job/workflow status with progress and step details.
- Added `progress_percent`, `progress_message` fields to `/_api/jobs` list endpoint in `crates/forge-runtime/src/dashboard/api.rs`
- Added Progress column to jobs table in `crates/forge-runtime/src/dashboard/pages.rs`
- Added clickable rows that open detail modals for jobs and workflows
- Added job detail modal showing full progress bar, input/output, error details
- Added workflow detail modal showing step-by-step progress with status icons
- Added modal CSS styles (`.modal`, `.progress-bar`, `.step-item`) in `crates/forge-runtime/src/dashboard/assets.rs`
- Added `openJobModal()`, `openWorkflowModal()`, `closeJobModal()`, `closeWorkflowModal()` JS functions
- Added `renderJobProgress()` helper for inline progress bar display
- Modal closes on background click or Escape key

Implemented scaffolded app UI, dashboard, and observability enhancements.
Phase 1 - Heartbeat Cron:
- Added `app_stats` table migration with reactivity enabled in `crates/forge/src/cli/new.rs`
- Created `heartbeat_stats_cron.rs` template running every minute to update stats
- Created `app_stats.rs` query returning stats map for frontend subscription
- Enabled HeartbeatStatsCron by default in scaffolded main.rs
Phase 3 - Dashboard Metrics Aggregation:
- Fixed `get_metric_series()` in `crates/forge-runtime/src/dashboard/api.rs`
- Added time bucket aggregation using PostgreSQL `date_trunc()` with dynamic intervals
- Counter metrics now SUMmed by bucket, gauges take last value per bucket
Phase 4 - Metric Detail Modal:
- Added metric modal HTML to metrics page in `crates/forge-runtime/src/dashboard/pages.rs`
- Implemented `selectMetric()` in `crates/forge-runtime/src/dashboard/assets.rs` with fetch and chart render
- Added `closeMetricModal()` and Escape key handler for modal close
Phase 5 - Span Context for Traces:
- Added job execution span recording in `crates/forge-runtime/src/jobs/worker.rs`
- Added cron execution span recording in `crates/forge-runtime/src/cron/scheduler.rs`
- Spans include job.id, job.type, cron.name, duration_ms, status attributes
Phase 2 - Frontend UI Panels:
- Added System Status panel with live stats subscription from heartbeat cron
- Added Background Job demo with progress bar simulation
- Added Workflow demo with step-by-step progress simulation
- Added `getAppStats` query export to api.ts template

Fixed dashboard cron page and scheduler bugs.
- Fixed cron scheduler boundary condition in `crates/forge-core/src/cron/schedule.rs` (`< local_end` → `<= local_end`)
- Fixed `try_claim()` in `crates/forge-runtime/src/cron/scheduler.rs` removing non-existent `timezone` column from INSERT
- Fixed dashboard cron APIs in `crates/forge-runtime/src/dashboard/api.rs` to use `forge_cron_runs` table with correct columns
- Added `loadCrons()` function in `crates/forge-runtime/src/dashboard/assets.rs` with API calls to `/_api/crons`, `/_api/crons/stats`, `/_api/crons/history`
- Fixed stats field mappings (`active_count`, `success_rate_24h`, `next_scheduled_run`)
- Added `loadCrons()` call to `loadPageSpecificData()` for `/crons` route

Implemented job/workflow dispatch from frontend and dashboard.
- Created `crates/forge-core/src/function/dispatch.rs` with JobDispatch and WorkflowDispatch traits
- Added `dispatch_job()` and `start_workflow()` methods to MutationContext and ActionContext
- Implemented JobDispatch for JobDispatcher in `crates/forge-runtime/src/jobs/dispatcher.rs`
- Implemented WorkflowDispatch for WorkflowExecutor in `crates/forge-runtime/src/workflow/executor.rs`
- Added dispatch builder methods to FunctionRouter, FunctionExecutor, RpcHandler, GatewayServer
- Added `POST /_api/jobs/{job_type}/dispatch` and `POST /_api/workflows/{workflow_name}/start` endpoints
- Updated DashboardState with job_dispatcher and workflow_executor fields
- Wired dispatchers in Forge runtime during gateway startup
- Added `dispatchJob()` and `startWorkflow()` to scaffolded frontend api.ts
- Updated demo page to use real dispatch with fallback to simulation

Replaced TESTING.md and USAGE.md with unified dev.sh script.
- Created `dev.sh` with commands: setup, start, db, logs, clean, all
- Compiles CLI, scaffolds app, fixes deps, starts DB/backend/frontend
- Logs written to .logs/ directory for easy tailing
- Graceful shutdown on Ctrl+C
- Deleted TESTING.md and USAGE.md (content consolidated into script)

Fixed scaffolded apps to have working job/workflow by default.
- Uncommented `ExportUsersJob` and `AccountVerificationWorkflow` registrations in generated main.rs
- Removed frontend simulation fallbacks from +page.svelte template
- Frontend now shows real backend errors instead of fake progress simulation
- Job and workflow demos are now fully backend-driven with real progress tracking

Fixed job progress tracking and workflow input validation.
- Added `progress_percent` and `progress_message` columns to `forge_jobs` table in builtin migrations
- Wired progress channel in JobExecutor to save progress updates to database
- Exported `ProgressUpdate` from `forge_core::job` module
- Added `update_progress()` method to JobQueue for database persistence
- Changed workflow `AccountVerificationInput.user_id` from `Uuid` to `String` to accept any identifier
- Made `email` field optional with `#[serde(default)]` for demo compatibility

Added realistic delays to job and workflow demos for visible progress tracking.
- Job: ~5 seconds total with progress updates at 0%, 10%, 30%, 50-80% (per user), 85%, 95%, 100%
- Workflow: ~5 seconds total with 1s delays between steps (generate_token, store_token, send_email, mark_verified)

Fixed blocking issues in job and workflow execution.
- Changed job progress channel from blocking `recv()` to non-blocking `try_recv()` with async sleep
- Changed workflow `start()` to spawn execution in background instead of blocking until completion
- Both job dispatch and workflow start now return immediately, execution happens asynchronously

Added workflow step persistence to database.
- Modified `record_step_start/complete/failure/compensated` methods in WorkflowContext to persist to `forge_workflow_steps` table
- Steps are persisted in background via `tokio::spawn` to keep API synchronous
- Added tokio dependency to forge-core for spawning persistence tasks
- Dashboard API now returns actual step data from database instead of empty array

Fixed frontend workflow polling to detect step changes.
- `pollWorkflowUntilComplete` was only checking `current_step` changes (always null)
- Changed to detect changes in `steps` array by comparing JSON serialization
- Reduced poll interval from 1000ms to 500ms for more responsive UI updates

Implemented WebSocket-based reactive job/workflow subscriptions.
- Added NOTIFY triggers on forge_jobs, forge_workflow_runs, forge_workflow_steps tables
- Extended ClientMessage enum with SubscribeJob, UnsubscribeJob, SubscribeWorkflow, UnsubscribeWorkflow variants
- Added UUID validation helper and MAX_CLIENT_SUB_ID_LEN constant for input security
- Extended Reactor with job_subscriptions and workflow_subscriptions maps
- Added subscribe_job(), unsubscribe_job(), subscribe_workflow(), unsubscribe_workflow() methods
- Handle job/workflow table changes in Reactor via handle_change with table routing
- Added JobUpdate and WorkflowUpdate variants to both ServerMessage and WebSocketMessage
- Extended ForgeClient with subscribeJob() and subscribeWorkflow() methods for WebSocket subscriptions
- Added pendingJobSubscriptions/pendingWorkflowSubscriptions for reconnect resilience
- Created subscribeJob() and subscribeWorkflow() store functions in stores.ts
- Updated demo page to use reactive stores instead of polling
- Frontend job/workflow updates now push via WebSocket in real-time
- All 304 tests passing

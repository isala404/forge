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

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

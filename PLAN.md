# PLAN: Implement FORGE Framework from Ground Up

Overall Goal: Build a complete, production-ready application framework in Rust with Svelte 5 frontend that provides schema-driven development, background jobs, real-time subscriptions, clustering, and built-in observability—all in a single binary with PostgreSQL as the sole infrastructure dependency.

---

## Phase 1: Foundation & Core Infrastructure

**📚 Reference Documents:**
- **Proposal Overview:** `./proposal/PROPOSAL.md`
- **Vision:** `./proposal/PROPOSAL.md` (Executive summary and goals)
- **Architecture:** `./proposal/architecture/OVERVIEW.md` (System components and layers)
- **Binary Design:** `./proposal/architecture/SINGLE_BINARY.md` (Main entry point and role initialization)
- **Configuration:** `./proposal/reference/CONFIGURATION.md` (The `forge.toml` spec)
- **Database:** `./proposal/database/POSTGRES_SCHEMA.md` (Specifically the `forge_nodes` table and general setup)
- **Decisions:** `./proposal/appendix/DECISIONS.md` (Context on why specific tech was chosen)

### Step 1.1: Project Structure & Cargo Workspace
- Goal: Establish the Rust workspace with crate boundaries
- Files:
  - `Cargo.toml` (workspace root)
  - `crates/forge/Cargo.toml` (main binary)
  - `crates/forge-core/Cargo.toml` (shared types, traits)
  - `crates/forge-macros/Cargo.toml` (proc macros)
  - `crates/forge-runtime/Cargo.toml` (execution engine)
  - `crates/forge-codegen/Cargo.toml` (TypeScript generation)
- Verify: `cargo check` passes

### Step 1.2: Configuration System
- Goal: Parse and validate forge.toml configuration
- Files:
  - `crates/forge-core/src/config.rs` (config structs with serde)
  - `crates/forge-core/src/config/database.rs`
  - `crates/forge-core/src/config/cluster.rs`
  - `crates/forge-core/src/config/observability.rs`
- Verify: Unit tests for config parsing

### Step 1.3: PostgreSQL Connection Pool
- Goal: Establish database connectivity with sqlx
- Files:
  - `crates/forge-runtime/src/db/pool.rs` (connection pooling)
  - `crates/forge-runtime/src/db/mod.rs`
- Verify: Integration test connecting to local PostgreSQL

### Step 1.4: Error Handling Foundation
- Goal: Define error types and Result patterns
- Files:
  - `crates/forge-core/src/error.rs` (ForgeError enum)
  - `crates/forge-core/src/result.rs` (type aliases)
- Verify: Unit tests for error conversions

---

## Phase 2: Schema System & Proc Macros

**📚 Reference Documents:**
- **Proposal Overview:** `./proposal/PROPOSAL.md`
- **Schema DSL:** `./proposal/core/SCHEMA.md` (Attributes reference: `#[model]`, `#[id]`, `#[index]`)
- **Migration Logic:** `./proposal/database/MIGRATIONS.md` (How diffs are calculated and SQL generated)
- **Database:** `./proposal/database/POSTGRES_SCHEMA.md` (Target SQL output format)

### Step 2.1: Model Proc Macro (#[forge::model])
- Goal: Generate schema metadata and SQL from Rust structs
- Files:
  - `crates/forge-macros/src/model.rs`
  - `crates/forge-macros/src/lib.rs`
  - `crates/forge-core/src/schema/model.rs` (ModelMeta trait)
- Verify: Compile test with sample model struct

### Step 2.2: Enum Proc Macro (#[forge::enum])
- Goal: Generate enum handling for database storage
- Files:
  - `crates/forge-macros/src/enum_type.rs`
  - `crates/forge-core/src/schema/enum_type.rs`
- Verify: Compile test with sample enum

### Step 2.3: Schema Registry
- Goal: Collect all models at compile time for codegen
- Files:
  - `crates/forge-core/src/schema/registry.rs`
  - `crates/forge-core/src/schema/mod.rs`
- Verify: Registry correctly collects model metadata

### Step 2.4: Migration Generator
- Goal: Generate SQL migrations from schema diff
- Files:
  - `crates/forge-runtime/src/migrations/generator.rs`
  - `crates/forge-runtime/src/migrations/diff.rs`
  - `crates/forge-runtime/src/migrations/executor.rs`
- Verify: Generate and apply migration for sample schema

---

## Phase 3: Function System

**📚 Reference Documents:**
- **Proposal Overview:** `./proposal/PROPOSAL.md`
- **Function Spec:** `./proposal/core/FUNCTIONS.md` (Query/Mutation/Action traits and rules)
- **Context Injection:** `./proposal/architecture/DATA_FLOW.md` (How context passes through layers)
- **Security:** `./proposal/reference/SECURITY.md` (Auth guards, Rate limiting attributes)
- **Storage:** `./proposal/reference/STORAGE.md` (File upload handling via Context)

### Step 3.1: Function Traits & Context
- Goal: Define function signatures and context injection
- Files:
  - `crates/forge-core/src/function/traits.rs` (ForgeQuery, ForgeMutation, ForgeAction)
  - `crates/forge-core/src/function/context.rs` (QueryContext, MutationContext, ActionContext)
  - `crates/forge-core/src/function/mod.rs`
- Verify: Trait definitions compile

### Step 3.2: Query Proc Macro (#[forge::query])
- Goal: Transform async functions into query handlers
- Files:
  - `crates/forge-macros/src/query.rs`
- Verify: Compile test with sample query function

### Step 3.3: Mutation Proc Macro (#[forge::mutation])
- Goal: Transform async functions into mutation handlers with transaction support
- Files:
  - `crates/forge-macros/src/mutation.rs`
- Verify: Compile test with sample mutation function

### Step 3.4: Action Proc Macro (#[forge::action])
- Goal: Transform async functions into action handlers
- Files:
  - `crates/forge-macros/src/action.rs`
- Verify: Compile test with sample action function

### Step 3.5: Function Registry & Router
- Goal: Collect functions and route RPC calls
- Files:
  - `crates/forge-runtime/src/function/registry.rs`
  - `crates/forge-runtime/src/function/router.rs`
  - `crates/forge-runtime/src/function/executor.rs`
- Verify: Route and execute sample function

---

## Phase 4: HTTP Gateway

**📚 Reference Documents:**
- **Proposal Overview:** `./proposal/PROPOSAL.md`
- **Request Flow:** `./proposal/architecture/DATA_FLOW.md` (Gateway pipeline details)
- **Security:** `./proposal/reference/SECURITY.md` (JWT validation, CORS)
- **Config:** `./proposal/reference/CONFIGURATION.md` (Port, limits, timeouts)

### Step 4.1: HTTP Server Setup
- Goal: Create HTTP server with axum
- Files:
  - `crates/forge-runtime/src/gateway/server.rs`
  - `crates/forge-runtime/src/gateway/mod.rs`
- Verify: Server starts and responds to health check

### Step 4.2: RPC Endpoint
- Goal: Handle function calls via HTTP POST
- Files:
  - `crates/forge-runtime/src/gateway/rpc.rs`
  - `crates/forge-runtime/src/gateway/request.rs`
  - `crates/forge-runtime/src/gateway/response.rs`
- Verify: Call query/mutation via curl

### Step 4.3: Authentication Middleware
- Goal: Parse JWT tokens and inject user context
- Files:
  - `crates/forge-runtime/src/gateway/auth.rs`
  - `crates/forge-core/src/auth/mod.rs`
  - `crates/forge-core/src/auth/claims.rs`
- Verify: Protected endpoint rejects unauthenticated requests

### Step 4.4: Request Tracing
- Goal: Assign trace IDs and propagate context
- Files:
  - `crates/forge-runtime/src/gateway/tracing.rs`
- Verify: Trace ID appears in logs

---

## Phase 5: Job Queue System

**📚 Reference Documents:**
- **Proposal Overview:** `./proposal/PROPOSAL.md`
- **Job Logic:** `./proposal/core/JOBS.md` (Defining jobs, `#[forge::job]`)
- **Queue Architecture:** `./proposal/database/JOB_QUEUE.md` (The **SKIP LOCKED** SQL pattern)
- **Worker Config:** `./proposal/cluster/WORKERS.md` (Capabilities and routing)

### Step 5.1: Job Table Schema
- Goal: Create forge_jobs table with proper indexes
- Files:
  - `crates/forge-runtime/src/jobs/schema.sql` (embedded migration)
  - `crates/forge-runtime/src/jobs/mod.rs`
- Verify: Table created with indexes

### Step 5.2: Job Proc Macro (#[forge::job])
- Goal: Transform async functions into job handlers
- Files:
  - `crates/forge-macros/src/job.rs`
  - `crates/forge-core/src/job/traits.rs`
  - `crates/forge-core/src/job/context.rs` (JobContext)
- Verify: Compile test with sample job

### Step 5.3: Job Dispatcher
- Goal: Enqueue jobs with priority and scheduling
- Files:
  - `crates/forge-runtime/src/jobs/dispatcher.rs`
- Verify: Dispatch job and verify in database

### Step 5.4: Job Claimer (SKIP LOCKED)
- Goal: Claim jobs atomically without conflicts
- Files:
  - `crates/forge-runtime/src/jobs/claimer.rs`
- Verify: Multiple workers claim different jobs

### Step 5.5: Job Executor & Retry
- Goal: Execute jobs with timeout, retry, and dead letter
- Files:
  - `crates/forge-runtime/src/jobs/executor.rs`
  - `crates/forge-runtime/src/jobs/retry.rs`
- Verify: Job executes, retries on failure, moves to DLQ

### Step 5.6: Worker Loop
- Goal: Background task claiming and processing jobs
- Files:
  - `crates/forge-runtime/src/jobs/worker.rs`
- Verify: Worker processes jobs continuously

---

## Phase 6: Cron Scheduler

**📚 Reference Documents:**
- **Proposal Overview:** `./proposal/PROPOSAL.md`
- **Cron Spec:** `./proposal/core/CRONS.md` (Syntax, Timezones, Context)
- **Leader Election:** `./proposal/cluster/LEADER_ELECTION.md` (Ensuring only one node schedules)
- **Database:** `./proposal/database/POSTGRES_SCHEMA.md` (`forge_cron_runs` table)

### Step 6.1: Cron Proc Macro (#[forge::cron])
- Goal: Define scheduled tasks with cron expressions
- Files:
  - `crates/forge-macros/src/cron.rs`
  - `crates/forge-core/src/cron/traits.rs`
  - `crates/forge-core/src/cron/schedule.rs` (cron parser)
- Verify: Parse cron expression correctly

### Step 6.2: Cron Registry
- Goal: Collect all cron definitions
- Files:
  - `crates/forge-runtime/src/cron/registry.rs`
- Verify: Registry lists all crons

### Step 6.3: Cron Scheduler (Leader-Only)
- Goal: Scheduler dispatches cron jobs at scheduled times
- Files:
  - `crates/forge-runtime/src/cron/scheduler.rs`
  - `crates/forge-runtime/src/cron/schema.sql` (forge_cron_runs table)
- Verify: Cron triggers at scheduled time

### Step 6.4: Missed Run Handling
- Goal: Handle missed runs when scheduler was down
- Files:
  - `crates/forge-runtime/src/cron/catchup.rs`
- Verify: Missed runs are caught up or skipped per config

---

## Phase 7: Workflow Engine

**📚 Reference Documents:**
- **Proposal Overview:** `./proposal/PROPOSAL.md`
- **Workflow Spec:** `./proposal/core/WORKFLOWS.md` (Steps, Compensation, Durability)
- **Database:** `./proposal/database/POSTGRES_SCHEMA.md` (`forge_workflow_runs`, `forge_workflow_steps`)

### Step 7.1: Workflow Proc Macro (#[forge::workflow])
- Goal: Define multi-step durable workflows
- Files:
  - `crates/forge-macros/src/workflow.rs`
  - `crates/forge-core/src/workflow/traits.rs`
  - `crates/forge-core/src/workflow/context.rs`
- Verify: Compile test with sample workflow

### Step 7.2: Workflow State Persistence
- Goal: Store workflow state for resume
- Files:
  - `crates/forge-runtime/src/workflow/schema.sql` (forge_workflow_runs, forge_workflow_steps)
  - `crates/forge-runtime/src/workflow/state.rs`
- Verify: Workflow state persists across restarts

### Step 7.3: Step Execution
- Goal: Execute workflow steps with result caching
- Files:
  - `crates/forge-runtime/src/workflow/executor.rs`
  - `crates/forge-runtime/src/workflow/step.rs`
- Verify: Steps execute in order, results cached

### Step 7.4: Compensation (Rollback)
- Goal: Run compensation steps on failure
- Files:
  - `crates/forge-runtime/src/workflow/compensation.rs`
- Verify: Compensation runs in reverse order on failure

### Step 7.5: Wait States
- Goal: Pause workflow waiting for external signal
- Files:
  - `crates/forge-runtime/src/workflow/wait.rs`
- Verify: Workflow pauses and resumes on signal

---

## Phase 8: Clustering & Coordination

**📚 Reference Documents:**
- **Proposal Overview:** `./proposal/PROPOSAL.md`
- **Architecture:** `./proposal/cluster/CLUSTERING.md` (Node lifecycle, states)
- **Discovery:** `./proposal/cluster/DISCOVERY.md` (Postgres/DNS/K8s modes)
- **Leader Election:** `./proposal/cluster/LEADER_ELECTION.md` (Advisory locks implementation)
- **Meshing:** `./proposal/cluster/MESHING.md` (gRPC setup)

### Step 8.1: Node Registry
- Goal: Nodes register in forge_nodes table
- Files:
  - `crates/forge-runtime/src/cluster/schema.sql`
  - `crates/forge-runtime/src/cluster/node.rs`
  - `crates/forge-runtime/src/cluster/registry.rs`
- Verify: Node appears in table on startup

### Step 8.2: Heartbeat Loop
- Goal: Nodes send periodic heartbeats
- Files:
  - `crates/forge-runtime/src/cluster/heartbeat.rs`
- Verify: last_heartbeat updates every 5 seconds

### Step 8.3: Dead Node Detection
- Goal: Mark nodes as dead when heartbeat stops
- Files:
  - `crates/forge-runtime/src/cluster/health.rs`
- Verify: Dead node marked after threshold

### Step 8.4: Leader Election (Advisory Locks)
- Goal: Elect single scheduler leader using pg_advisory_lock
- Files:
  - `crates/forge-runtime/src/cluster/leader.rs`
  - `crates/forge-runtime/src/cluster/schema.sql` (forge_leaders table)
- Verify: Only one node becomes leader

### Step 8.5: Graceful Shutdown
- Goal: Drain connections and release leadership on shutdown
- Files:
  - `crates/forge-runtime/src/cluster/shutdown.rs`
- Verify: Node drains and deregisters cleanly

### Step 8.6: gRPC Mesh (Inter-Node Communication)
- Goal: Nodes communicate via gRPC for subscription routing
- Files:
  - `crates/forge-runtime/src/cluster/grpc/server.rs`
  - `crates/forge-runtime/src/cluster/grpc/client.rs`
  - `crates/forge-runtime/src/cluster/grpc/proto/cluster.proto`
- Verify: Nodes can send messages to each other

---

## Phase 9: Reactivity System

**📚 Reference Documents:**
- **Proposal Overview:** `./proposal/PROPOSAL.md`
- **Reactivity Spec:** `./proposal/core/REACTIVITY.md` (The "Read Set" concept, Invalidation logic)
- **Change Tracking:** `./proposal/database/CHANGE_TRACKING.md` (Postgres Triggers, `LISTEN/NOTIFY`)
- **WebSockets:** `./proposal/frontend/WEBSOCKET.md` (Protocol definition)

### Step 9.1: WebSocket Server
- Goal: Accept WebSocket connections for subscriptions
- Files:
  - `crates/forge-runtime/src/realtime/websocket.rs`
  - `crates/forge-runtime/src/realtime/mod.rs`
- Verify: Client connects via WebSocket

### Step 9.2: Session Management
- Goal: Track WebSocket sessions and auth
- Files:
  - `crates/forge-runtime/src/realtime/session.rs`
  - `crates/forge-runtime/src/realtime/schema.sql` (forge_sessions, forge_subscriptions)
- Verify: Session created on connect, removed on disconnect

### Step 9.3: Subscription Registration
- Goal: Register query subscriptions with parameters
- Files:
  - `crates/forge-runtime/src/realtime/subscription.rs`
- Verify: Subscription stored with query hash

### Step 9.4: Read Set Tracking
- Goal: Track tables/rows read during query execution
- Files:
  - `crates/forge-runtime/src/realtime/readset.rs`
- Verify: Read set captured during query

### Step 9.5: Change Detection (LISTEN/NOTIFY)
- Goal: Listen for database changes via pg_notify
- Files:
  - `crates/forge-runtime/src/realtime/listener.rs`
- Verify: Changes trigger notification

### Step 9.6: Subscription Invalidation
- Goal: Re-execute queries when read set changes
- Files:
  - `crates/forge-runtime/src/realtime/invalidation.rs`
- Verify: Client receives update on data change

### Step 9.7: Cross-Node Routing
- Goal: Route invalidations to correct node via gRPC
- Files:
  - `crates/forge-runtime/src/realtime/routing.rs`
- Verify: Change on node A updates client on node B

---

## Phase 10: Observability

**📚 Reference Documents:**
- **Proposal Overview:** `./proposal/PROPOSAL.md`
- **Overview:** `./proposal/observability/OBSERVABILITY.md` (Architecture)
- **Metrics:** `./proposal/observability/METRICS.md` (Counters, Gauges, Histograms)
- **Logs:** `./proposal/observability/LOGGING.md` (Structured logging schema)
- **Traces:** `./proposal/observability/TRACING.md` (Spans and context propagation)
- **Database:** `./proposal/database/POSTGRES_SCHEMA.md` (Observability tables)

### Step 10.1: Metrics Collector
- Goal: Collect and buffer metrics in memory
- Files:
  - `crates/forge-runtime/src/observability/metrics/collector.rs`
  - `crates/forge-runtime/src/observability/metrics/mod.rs`
- Verify: Metrics increment correctly

### Step 10.2: Metrics Storage
- Goal: Flush metrics to forge_metrics table
- Files:
  - `crates/forge-runtime/src/observability/metrics/storage.rs`
  - `crates/forge-runtime/src/observability/schema.sql`
- Verify: Metrics appear in database

### Step 10.3: Structured Logging
- Goal: JSON logging with context fields
- Files:
  - `crates/forge-runtime/src/observability/logging/mod.rs`
  - `crates/forge-runtime/src/observability/logging/storage.rs`
- Verify: Logs stored with trace_id, function_name

### Step 10.4: Distributed Tracing
- Goal: Create and propagate spans
- Files:
  - `crates/forge-runtime/src/observability/tracing/mod.rs`
  - `crates/forge-runtime/src/observability/tracing/span.rs`
  - `crates/forge-runtime/src/observability/tracing/storage.rs`
- Verify: Trace with spans stored in forge_traces

### Step 10.5: Automatic Instrumentation
- Goal: Auto-add spans for functions, queries, HTTP
- Files:
  - `crates/forge-runtime/src/observability/auto.rs`
- Verify: Spans created without explicit code

### Step 10.6: Data Retention & Downsampling
- Goal: Aggregate old metrics, delete old data
- Files:
  - `crates/forge-runtime/src/observability/retention.rs`
- Verify: Old data aggregated and cleaned

### Step 10.7: OTLP Export (Optional)
- Goal: Export to external observability platforms
- Files:
  - `crates/forge-runtime/src/observability/export/otlp.rs`
  - `crates/forge-runtime/src/observability/export/prometheus.rs`
- Verify: Data appears in external system

### Step 10.8: Alerting Engine
- Goal: Evaluate alert conditions and notify
- Files:
  - `crates/forge-runtime/src/observability/alerts/engine.rs`
  - `crates/forge-runtime/src/observability/alerts/notify.rs`
- Verify: Alert triggers and sends notification

---

## Phase 11: TypeScript Codegen

**📚 Reference Documents:**
- **Proposal Overview:** `./proposal/PROPOSAL.md`
- **Frontend Spec:** `./proposal/frontend/FRONTEND.md` (Desired output format)
- **RPC Client:** `./proposal/frontend/RPC_CLIENT.md` (Type generation for API)
- **Stores:** `./proposal/frontend/STORES.md` (Svelte store generation)

### Step 11.1: Type Generator
- Goal: Generate TypeScript interfaces from Rust models
- Files:
  - `crates/forge-codegen/src/typescript/types.rs`
  - `crates/forge-codegen/src/typescript/mod.rs`
- Verify: Generated types.ts matches schema

### Step 11.2: API Bindings Generator
- Goal: Generate function call bindings
- Files:
  - `crates/forge-codegen/src/typescript/api.rs`
- Verify: Generated api.ts with typed functions

### Step 11.3: Store Generator
- Goal: Generate Svelte 5 reactive stores
- Files:
  - `crates/forge-codegen/src/typescript/stores.rs`
- Verify: Generated stores.ts with Svelte runes

### Step 11.4: Client Generator
- Goal: Generate WebSocket client and RPC client
- Files:
  - `crates/forge-codegen/src/typescript/client.rs`
- Verify: Generated client.ts with full API

---

## Phase 12: Frontend Runtime Library

**📚 Reference Documents:**
- **Proposal Overview:** `./proposal/PROPOSAL.md`
- **Frontend Spec:** `./proposal/frontend/FRONTEND.md`
- **WebSocket:** `./proposal/frontend/WEBSOCKET.md` (Reconnection logic)
- **Stores:** `./proposal/frontend/STORES.md` (Svelte 5 Runes implementation)

### Step 12.1: Core Client (TypeScript)
- Goal: HTTP/WebSocket client for FORGE
- Files:
  - `frontend-lib/src/client.ts`
  - `frontend-lib/src/transport.ts`
  - `frontend-lib/package.json`
- Verify: Client connects and calls functions

### Step 12.2: Query/Mutation Helpers
- Goal: Typed wrappers for function calls
- Files:
  - `frontend-lib/src/query.ts`
  - `frontend-lib/src/mutation.ts`
  - `frontend-lib/src/action.ts`
- Verify: Type-safe function calls work

### Step 12.3: Subscription System
- Goal: Real-time subscription management
- Files:
  - `frontend-lib/src/subscription.ts`
  - `frontend-lib/src/reconnect.ts`
- Verify: Subscriptions update on data change

### Step 12.4: Svelte 5 Integration
- Goal: Svelte-specific components and stores
- Files:
  - `frontend-lib/src/svelte/provider.svelte`
  - `frontend-lib/src/svelte/stores.ts`
  - `frontend-lib/src/svelte/hooks.ts`
- Verify: Svelte app with real-time updates

### Step 12.5: Error Handling
- Goal: Typed error handling for frontend
- Files:
  - `frontend-lib/src/error.ts`
- Verify: Errors parsed and typed correctly

---

## Phase 13: Dashboard

**📚 Reference Documents:**
- **Proposal Overview:** `./proposal/PROPOSAL.md`
- **Dashboard Spec:** `./proposal/observability/DASHBOARD.md` (UI requirements)
- **Metrics/Logs:** Ref previous observability docs for data sources.

### Step 13.1: Dashboard Server
- Goal: Serve embedded dashboard at /_dashboard
- Files:
  - `crates/forge-runtime/src/dashboard/server.rs`
  - `crates/forge-runtime/src/dashboard/mod.rs`
- Verify: Dashboard loads in browser

### Step 13.2: Dashboard API
- Goal: Internal API for dashboard operations
- Files:
  - `crates/forge-runtime/src/dashboard/api/mod.rs`
  - `crates/forge-runtime/src/dashboard/api/jobs.rs`
  - `crates/forge-runtime/src/dashboard/api/crons.rs`
  - `crates/forge-runtime/src/dashboard/api/metrics.rs`
  - `crates/forge-runtime/src/dashboard/api/logs.rs`
  - `crates/forge-runtime/src/dashboard/api/migrations.rs`
- Verify: API endpoints return data

### Step 13.3: Dashboard UI (Svelte)
- Goal: Build dashboard frontend
- Files:
  - `dashboard/src/routes/+layout.svelte`
  - `dashboard/src/routes/jobs/+page.svelte`
  - `dashboard/src/routes/crons/+page.svelte`
  - `dashboard/src/routes/logs/+page.svelte`
  - `dashboard/src/routes/metrics/+page.svelte`
  - `dashboard/src/routes/traces/+page.svelte`
  - `dashboard/src/routes/migrations/+page.svelte`
  - `dashboard/src/routes/cluster/+page.svelte`
- Verify: All dashboard pages functional

### Step 13.4: Embed Dashboard in Binary
- Goal: Include built dashboard in Rust binary
- Files:
  - `crates/forge-runtime/src/dashboard/embed.rs`
  - `build.rs` (build script to embed assets)
- Verify: Dashboard served from single binary

---

## Phase 14: CLI Tool

**📚 Reference Documents:**
- **Proposal Overview:** `./proposal/PROPOSAL.md`
- **CLI Spec:** `./proposal/reference/CLI.md` (Commands: `new`, `add`, `generate`)
- **Dev Workflow:** `./proposal/deployment/LOCAL_DEV.md` (`forge dev` behavior)

### Step 14.1: CLI Framework
- Goal: Set up clap-based CLI
- Files:
  - `crates/forge-cli/Cargo.toml`
  - `crates/forge-cli/src/main.rs`
  - `crates/forge-cli/src/commands/mod.rs`
- Verify: `forge --help` works

### Step 14.2: Project Scaffolding Commands
- Goal: `forge new` and `forge init`
- Files:
  - `crates/forge-cli/src/commands/new.rs`
  - `crates/forge-cli/src/commands/init.rs`
  - `crates/forge-cli/src/templates/` (project templates)
- Verify: `forge new my-app` creates project

### Step 14.3: Add Commands
- Goal: `forge add model/query/mutation/action/job/cron`
- Files:
  - `crates/forge-cli/src/commands/add.rs`
  - `crates/forge-cli/src/templates/model.rs.template`
  - `crates/forge-cli/src/templates/query.rs.template`
  - etc.
- Verify: `forge add model Task` creates file

### Step 14.4: Generate Command
- Goal: `forge generate` for TypeScript codegen
- Files:
  - `crates/forge-cli/src/commands/generate.rs`
- Verify: TypeScript files generated

### Step 14.5: Dev Server Command
- Goal: `forge dev` with hot reload
- Files:
  - `crates/forge-cli/src/commands/dev.rs`
- Verify: Dev server starts with watch mode

---

## Phase 15: Single Binary Assembly

**📚 Reference Documents:**
- **Proposal Overview:** `./proposal/PROPOSAL.md`
- **Concept:** `./proposal/architecture/SINGLE_BINARY.md` (Role-based startup)
- **Resilience:** `./proposal/architecture/RESILIENCE.md` (Graceful shutdown, Signal handling)
- **Deployment:** `./proposal/deployment/DEPLOYMENT.md` (Env vars, Docker)

### Step 15.1: Runtime Unification
- Goal: Combine all components into single runtime
- Files:
  - `crates/forge-runtime/src/runtime.rs`
  - `crates/forge-runtime/src/lib.rs`
- Verify: All components initialize together

### Step 15.2: Role-Based Startup
- Goal: Enable/disable components via config
- Files:
  - `crates/forge-runtime/src/roles.rs`
- Verify: Can run gateway-only or worker-only

### Step 15.3: Main Binary
- Goal: Create final `forge` binary
- Files:
  - `crates/forge/src/main.rs`
- Verify: `cargo build --release` produces single binary

### Step 15.4: Signal Handling
- Goal: Handle SIGTERM/SIGINT for graceful shutdown
- Files:
  - `crates/forge/src/signals.rs`
- Verify: Graceful shutdown on Ctrl+C

---

## Phase 16: Testing & Validation

**📚 Reference Documents:**
- **Proposal Overview:** `./proposal/PROPOSAL.md`
- **Testing:** `./proposal/development/TESTING.md` (Unit, Integration, Cluster tests)
- **Resilience:** `./proposal/architecture/RESILIENCE.md` (Chaos testing scenarios)

### Step 16.1: Unit Test Suite
- Goal: Comprehensive unit tests for all crates
- Files:
  - Tests in each crate's `src/` with `#[cfg(test)]`
- Verify: `cargo test` passes with >80% coverage

### Step 16.2: Integration Test Suite
- Goal: End-to-end tests with real PostgreSQL
- Files:
  - `tests/integration/` directory
  - `tests/integration/jobs.rs`
  - `tests/integration/subscriptions.rs`
  - `tests/integration/cluster.rs`
- Verify: Integration tests pass

### Step 16.3: Example Application
- Goal: Build reference application
- Files:
  - `examples/todo-app/`
  - `examples/todo-app/src/schema/`
  - `examples/todo-app/src/functions/`
  - `examples/todo-app/frontend/`
- Verify: Example app runs end-to-end

### Step 16.4: API Documentation
- Goal: Rustdoc for all public APIs
- Files:
  - Doc comments in all public items
- Verify: `cargo doc` generates complete docs

---

## Final Step: Validation & Release

- Goal: Validate all functionality works together
- Verify:
  - [ ] `forge new my-app` creates working project
  - [ ] Schema compiles and generates TypeScript
  - [ ] Queries, mutations, actions work
  - [ ] Jobs dispatch, execute, retry
  - [ ] Crons trigger on schedule
  - [ ] Workflows execute with persistence
  - [ ] Subscriptions update in real-time
  - [ ] Multi-node cluster forms and fails over
  - [ ] Dashboard shows all observability data
  - [ ] Production build is single binary <50MB
  - [ ] All tests pass

---

## Implementation Order Summary

| Phase | Duration Estimate | Dependencies |
|-------|------------------|--------------|
| 1. Foundation | - | None |
| 2. Schema & Macros | - | Phase 1 |
| 3. Functions | - | Phase 2 |
| 4. HTTP Gateway | - | Phase 3 |
| 5. Job Queue | - | Phase 4 |
| 6. Cron Scheduler | - | Phase 5, 8 (leader) |
| 7. Workflows | - | Phase 5 |
| 8. Clustering | - | Phase 1 |
| 9. Reactivity | - | Phase 4, 8 |
| 10. Observability | - | Phase 1 |
| 11. TypeScript Codegen | - | Phase 2, 3 |
| 12. Frontend Runtime | - | Phase 11 |
| 13. Dashboard | - | Phase 10, 12 |
| 14. CLI | - | Phase 11 |
| 15. Single Binary | - | All |
| 16. Testing & Docs | - | All |

---

## Critical Path

```
Phase 1 → Phase 2 → Phase 3 → Phase 4 ─────────────────────────┐
                         │                                      │
                         ├─→ Phase 5 → Phase 6                  │
                         │      │                               │
                         │      └─→ Phase 7                     │
                         │                                      │
                         └─→ Phase 8 → Phase 9 ─────────────────┤
                                                                │
Phase 10 (parallel with 4-9) ──────────────────────────────────┤
                                                                │
Phase 11 (after 2,3) → Phase 12 → Phase 13 ────────────────────┤
                          │                                     │
                          └─→ Phase 14 ────────────────────────┤
                                                                │
                                                   Phase 15 ←───┘
                                                       │
                                                   Phase 16
```

Phases 1-4 are strictly sequential (core foundation).
Phases 5-7 (jobs, crons, workflows) can proceed in parallel.
Phase 8-9 (clustering, reactivity) can proceed in parallel with 5-7.
Phase 10 (observability) can proceed in parallel with most phases.
Phases 11-14 (frontend/CLI) depend on core but can proceed once Phase 3 is done.
Phase 15 integrates everything.
Phase 16 validates and documents.

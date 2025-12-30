# FORGE Architecture Overview

This document describes the actual architecture of FORGE as implemented. FORGE is a batteries-included Rust framework for full-stack web applications that collapses the typical infrastructure stack (PostgreSQL, Redis, Kafka, Prometheus, etc.) into a single binary backed by PostgreSQL.

---

## Design Philosophy

FORGE eliminates operational complexity by using:
- **PostgreSQL** for all persistent state (data, jobs, sessions, metrics, logs, traces)
- **Single binary** containing all runtime components

No Redis, Kafka, or separate observability stack required.

---

## Single Binary Architecture

All components run within a single Rust binary. Nodes are configured with roles that determine which components are active:

```
┌─────────────────────────────────────────────────────────────┐
│                      FORGE Binary                            │
├─────────────────────────────────────────────────────────────┤
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐         │
│  │   Gateway   │  │  Function   │  │   Worker    │         │
│  │  (HTTP/WS)  │  │  Executor   │  │   (Jobs)    │         │
│  └─────────────┘  └─────────────┘  └─────────────┘         │
│  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐         │
│  │  Scheduler  │  │   Reactor   │  │ Observability│        │
│  │   (Crons)   │  │ (Real-time) │  │  Collector   │        │
│  └─────────────┘  └─────────────┘  └─────────────┘         │
│                                                              │
│  ┌───────────────────────────────────────────────────────┐  │
│  │                    Registries                          │  │
│  │  FunctionRegistry | JobRegistry | CronRegistry        │  │
│  │  WorkflowRegistry                                      │  │
│  └───────────────────────────────────────────────────────┘  │
└─────────────────────────────────────────────────────────────┘
                              │
                              ▼
                    ┌─────────────────┐
                    │   PostgreSQL    │
                    └─────────────────┘
```

---

## Core Components

### 1. Forge Runtime (`crates/forge/src/runtime.rs`)

The central `Forge` struct orchestrates all components. It is constructed via the `ForgeBuilder` pattern:

```rust
Forge::builder()
    .config(config)
    .build()?
    .run()
    .await
```

**Key responsibilities:**
- Database connection and migration execution
- Node registration in cluster
- Component startup based on configured roles
- Graceful shutdown coordination

### 2. Gateway Layer (`crates/forge-runtime/src/gateway/`)

The HTTP/WebSocket gateway built on axum:

| Component | File | Purpose |
|-----------|------|---------|
| `GatewayServer` | `server.rs` | HTTP server with middleware stack |
| `RpcHandler` | `rpc.rs` | Function invocation via JSON-RPC |
| `AuthMiddleware` | `auth.rs` | JWT token validation |
| `ws_handler` | `websocket.rs` | WebSocket upgrade and message handling |
| `MetricsState` | `metrics.rs` | Request metrics recording |

**Endpoints:**
- `GET /health` - Health check
- `POST /rpc` - JSON-RPC function calls
- `POST /rpc/{function}` - REST-style function calls
- `/ws` - WebSocket for subscriptions
- `/_dashboard/*` - Built-in admin dashboard
- `/_api/*` - Dashboard REST API

### 3. Function Executor (`crates/forge-runtime/src/function/`)

Executes query, mutation, and action functions with:
- Timeout handling per function
- Authentication context injection
- Job/workflow dispatch capabilities

**FunctionRegistry** stores type-erased handlers keyed by function name.

### 4. Worker (`crates/forge-runtime/src/jobs/worker.rs`)

Background job processor using PostgreSQL's `SKIP LOCKED` pattern:

```sql
SELECT * FROM forge_jobs
WHERE status = 'pending'
  AND scheduled_at <= NOW()
ORDER BY priority DESC, created_at
FOR UPDATE SKIP LOCKED
LIMIT $batch_size
```

**Features:**
- Semaphore-based concurrency control
- Capability-based job routing
- Progress tracking via database updates
- Observability span recording

### 5. Scheduler (`crates/forge-runtime/src/cron/scheduler.rs`)

Runs cron jobs on the leader node:
- Parses 5/6-part cron expressions
- Timezone-aware scheduling via chrono-tz
- Exactly-once execution via unique constraint on (cron_name, scheduled_time)
- Catch-up logic for missed runs

### 6. Reactor (`crates/forge-runtime/src/realtime/reactor.rs`)

Orchestrates the real-time reactivity pipeline:

```
PostgreSQL NOTIFY → ChangeListener → InvalidationEngine → Query Re-execution → WebSocket Push
```

**Pipeline:**
1. **ChangeListener** - Uses `PgListener` on `forge_changes` channel
2. **InvalidationEngine** - Debounces/coalesces changes, matches subscriptions
3. **SubscriptionManager** - Tracks active subscriptions per session
4. **WebSocketServer** - Broadcasts updates to connected clients

Tables can opt-in to reactivity via: `SELECT forge_enable_reactivity('table_name');`

### 7. Observability (`crates/forge-runtime/src/observability/`)

All nodes collect and persist observability data:

| Collector | Data | Storage |
|-----------|------|---------|
| `MetricsCollector` | Counters, gauges | `forge_metrics` |
| `LogCollector` | Structured logs | `forge_logs` |
| `TraceCollector` | Distributed traces | `forge_traces` |
| `SystemMetricsCollector` | CPU, memory via sysinfo | `forge_metrics` |

Background tasks flush collectors every 10-15 seconds and run hourly cleanup based on retention policies.

---

## Node Roles

Configured in `forge.toml` under `[node]`:

```toml
[node]
roles = ["gateway", "worker", "scheduler"]
worker_capabilities = ["general"]
```

| Role | Components Enabled |
|------|-------------------|
| `gateway` | HTTP server, WebSocket, Reactor |
| `function` | Function executor only |
| `worker` | Background job processing |
| `scheduler` | Cron execution (leader-only) |

A single node can have multiple roles. In development, all roles typically run together.

---

## Node Registration and Leadership

### Node Registry (`crates/forge-runtime/src/cluster/registry.rs`)

Each node registers itself in `forge_nodes`:

```rust
NodeInfo::new_local(hostname, ip, http_port, grpc_port, roles, capabilities, version)
```

The `HeartbeatLoop` updates `last_heartbeat` every 5 seconds. Nodes with stale heartbeats are marked dead.

### Leader Election (`crates/forge-runtime/src/cluster/leader.rs`)

Uses PostgreSQL advisory locks for leader election:

```rust
LeaderElection::new(pool, node_id, LeaderRole::Scheduler, config)
```

**Process:**
1. Try to acquire advisory lock: `pg_try_advisory_lock(lock_id)`
2. If acquired, record leadership in `forge_leaders` table
3. Refresh lease periodically
4. On shutdown, release lock and clear record

Only the scheduler leader runs cron jobs. Multiple nodes can attempt leadership; advisory locks ensure exactly one wins.

---

## ForgeBuilder Pattern

Components are registered before building and running:

```rust
let mut builder = Forge::builder();

// Register functions
builder.function_registry_mut().register_query::<ListUsers>();
builder.function_registry_mut().register_mutation::<CreateUser>();

// Register background work
builder.job_registry_mut().register::<SendEmailJob>();
builder.cron_registry_mut().register::<DailyCleanup>();
builder.workflow_registry_mut().register::<UserOnboarding>();

// Configure and run
let forge = builder
    .config(config)
    .migrations_dir("migrations")
    .build()?;

forge.run().await?;
```

---

## PostgreSQL as the Backbone

FORGE uses PostgreSQL for all coordination and persistence:

| Table | Purpose |
|-------|---------|
| `forge_nodes` | Cluster membership |
| `forge_leaders` | Leader election state |
| `forge_jobs` | Job queue with SKIP LOCKED |
| `forge_cron_runs` | Cron execution history |
| `forge_workflow_runs` | Workflow state |
| `forge_workflow_steps` | Individual step state |
| `forge_metrics` | Time-series metrics |
| `forge_logs` | Structured logs |
| `forge_traces` | Distributed traces |
| `forge_sessions` | WebSocket sessions |
| `forge_subscriptions` | Active real-time subscriptions |
| `forge_alert_rules` | Alerting rules |
| `forge_alerts` | Alert instances |

Built-in migrations are in `crates/forge-runtime/migrations/0000_forge_internal.sql`.

---

## Request Flow

### Synchronous Function Call

```
Client → HTTP POST /rpc → AuthMiddleware → TracingMiddleware
       → MetricsMiddleware → RpcHandler → FunctionExecutor
       → Query/Mutation/Action handler → PostgreSQL → Response
```

### Background Job

```
Client → POST /rpc → CreateUser mutation
                   → dispatch_job(SendEmailJob, args)
                   → INSERT into forge_jobs
       ← Response (job_id)

Worker → poll forge_jobs (SKIP LOCKED)
       → claim job → execute handler → update status
       → NOTIFY forge_changes → Reactor → WebSocket push
```

### Real-time Subscription

```
Client → WebSocket /ws → subscribe message
       → Reactor registers subscription
       ← Initial data response

(Data changes via INSERT/UPDATE/DELETE)
       → PostgreSQL trigger → NOTIFY forge_changes
       → ChangeListener receives → InvalidationEngine matches
       → Query re-executed → Delta computed → WebSocket push
```

---

## Graceful Shutdown

`GracefulShutdown` in `crates/forge-runtime/src/cluster/shutdown.rs`:

1. Receive shutdown signal (ctrl+c or shutdown_tx)
2. Set node status to "draining"
3. Wait for in-flight requests (tracked via `InFlightGuard`)
4. Stop leader election (release advisory lock)
5. Stop reactor
6. Final observability flush
7. Close database connections

---

## Key Patterns

| Pattern | Implementation |
|---------|---------------|
| Dependency injection | Context objects (`QueryContext`, `MutationContext`, `JobContext`) |
| Job queue | PostgreSQL `FOR UPDATE SKIP LOCKED` |
| Leader election | PostgreSQL advisory locks |
| Real-time updates | PostgreSQL `LISTEN/NOTIFY` + WebSocket |
| Type-erased handlers | `Arc<dyn Trait>` with runtime registry lookup |
| Proc macro code generation | `forge::query`, `forge::mutation`, `forge::job`, etc. |

---

## Crate Structure

```
crates/
├── forge/           # Main crate: CLI, runtime, prelude
│   └── src/
│       ├── cli/     # Command-line interface
│       └── runtime.rs  # Forge struct and ForgeBuilder
├── forge-core/      # Domain types (no async runtime)
│   └── src/
│       ├── function/   # Query, Mutation, Action traits
│       ├── job/        # ForgeJob trait
│       ├── cron/       # ForgeCron trait
│       ├── workflow/   # ForgeWorkflow trait
│       ├── cluster/    # NodeInfo, NodeRole, LeaderRole
│       └── observability/  # Metric, LogEntry, Span
├── forge-runtime/   # Runtime implementations
│   └── src/
│       ├── gateway/    # HTTP/WebSocket server
│       ├── function/   # Function executor
│       ├── jobs/       # Job queue and worker
│       ├── cron/       # Cron scheduler
│       ├── workflow/   # Workflow executor
│       ├── cluster/    # Node registry, leader election
│       ├── realtime/   # Reactor, change listener
│       └── observability/  # Collectors and stores
├── forge-macros/    # Proc macros
└── forge-codegen/   # TypeScript code generation
```

---

## Related Documentation

- `proposal/architecture/SINGLE_BINARY.md` - Original design rationale
- `proposal/core/REACTIVITY.md` - Reactivity system design
- `proposal/cluster/LEADER_ELECTION.md` - Leader election design
- `MEMORIES.md` - Stack info, patterns, conventions

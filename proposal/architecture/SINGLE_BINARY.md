# Single Binary Architecture

> *One binary to rule them all*

---

## The Monolith Paradox

The industry spent 15 years breaking monoliths into microservices. FORGE brings them back together—but smarter.

**Microservices solved:**
- Independent scaling
- Technology diversity
- Team autonomy

**Microservices created:**
- Network complexity
- Distributed transactions
- Operational overhead (x10)
- Debugging nightmares

**FORGE's answer:** A **distributed monolith**. One binary that forms a self-organizing cluster.

---

## How It Works

Every FORGE node runs the same binary. The binary contains all functionality:

```rust
// Conceptually, the FORGE binary is:
fn main() {
    let config = load_config();
    let cluster = join_cluster();
    
    // All capabilities are compiled in
    // Configuration determines what runs
    
    if config.roles.contains("gateway") {
        start_http_server();
        start_websocket_server();
    }
    
    if config.roles.contains("function") {
        start_function_executor();
    }
    
    if config.roles.contains("worker") {
        start_job_worker(config.worker_capabilities);
    }
    
    if config.roles.contains("scheduler") {
        try_become_scheduler_leader();
    }
    
    // Always runs
    start_observability_collector();
    start_cluster_mesh();
    
    run_forever();
}
```

---

## Roles

A **role** is a responsibility a node can take on. Roles are not separate services—they're modules within the same process.

### Available Roles

| Role | Responsibility | Instances |
|------|---------------|-----------|
| `gateway` | HTTP/WebSocket, external API | Any node |
| `function` | Execute queries/mutations/actions | Any node |
| `worker` | Process background jobs | Any node |
| `scheduler` | Assign jobs, run crons | **Exactly 1** (leader) |

### Role Configuration

```toml
# forge.toml

[node]
# Run all roles (default, good for dev/small deployments)
roles = ["gateway", "function", "worker", "scheduler"]

# Or specialize:
# roles = ["worker"]
# worker_capabilities = ["media"]
```

### Role Assignment Diagram

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           FORGE NODE INTERNALS                               │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│    ┌─────────────────────────────────────────────────────────────────┐      │
│    │                        CORE (always running)                     │      │
│    │                                                                  │      │
│    │   ┌────────────┐  ┌────────────┐  ┌────────────┐               │      │
│    │   │ Config     │  │ Cluster    │  │ PostgreSQL │               │      │
│    │   │ Loader     │  │ Mesh       │  │ Pool       │               │      │
│    │   └────────────┘  └────────────┘  └────────────┘               │      │
│    │                                                                  │      │
│    │   ┌────────────────────────────────────────────────────────┐   │      │
│    │   │              Observability Collector                    │   │      │
│    │   │   Metrics │ Logs │ Traces │ Health Checks               │   │      │
│    │   └────────────────────────────────────────────────────────┘   │      │
│    └─────────────────────────────────────────────────────────────────┘      │
│                                                                              │
│    ┌───────────────────────────────────────────────────────────────────┐    │
│    │                    ROLES (conditionally enabled)                   │    │
│    │                                                                    │    │
│    │    ┌─────────────┐    ┌─────────────┐    ┌─────────────┐         │    │
│    │    │   Gateway   │    │  Function   │    │   Worker    │         │    │
│    │    │             │    │  Executor   │    │             │         │    │
│    │    │ ✓ or ✗     │    │ ✓ or ✗     │    │ ✓ or ✗     │         │    │
│    │    │             │    │             │    │             │         │    │
│    │    │ • HTTP      │    │ • Queries   │    │ • Jobs      │         │    │
│    │    │ • WebSocket │    │ • Mutations │    │ • Crons     │         │    │
│    │    │ • Dashboard │    │ • Actions   │    │ • Workflows │         │    │
│    │    └─────────────┘    └─────────────┘    └─────────────┘         │    │
│    │                                                                    │    │
│    │    ┌─────────────────────────────────────────────────────────┐   │    │
│    │    │                     Scheduler                            │   │    │
│    │    │                                                          │   │    │
│    │    │  ✓ or ✗ (only ONE node is leader at a time)             │   │    │
│    │    │                                                          │   │    │
│    │    │  • Job assignment    • Dead letter handling              │   │    │
│    │    │  • Cron triggering   • Cluster coordination              │   │    │
│    │    └─────────────────────────────────────────────────────────┘   │    │
│    └───────────────────────────────────────────────────────────────────┘    │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Why Single Binary?

### 1. Operational Simplicity

**Microservices:**
```bash
# Deploy 8 services
kubectl apply -f api-gateway.yaml
kubectl apply -f user-service.yaml
kubectl apply -f order-service.yaml
kubectl apply -f job-worker.yaml
kubectl apply -f scheduler.yaml
kubectl apply -f metrics-collector.yaml
kubectl apply -f log-aggregator.yaml
kubectl apply -f trace-collector.yaml
```

**FORGE:**
```bash
# Deploy everything
kubectl apply -f forge.yaml
```

### 2. Consistent Versioning

**Microservices:**
```
api-gateway:      v2.3.1
user-service:     v1.8.4
order-service:    v2.1.0 ← incompatible with user-service v1.8.4!
```

**FORGE:**
```
forge: v1.5.0  ← Everything is one version
```

### 3. Shared Resources

**Microservices:**
- Each service has its own database connection pool
- Memory duplicated across services
- CPU overhead from container isolation

**FORGE:**
- Single connection pool, shared efficiently
- Single memory space, zero duplication
- No container overhead between "services"

### 4. Debugging

**Microservices:**
```
User reports bug → Check API gateway logs → Check user service logs 
→ Check order service logs → Check database logs → Still confused
```

**FORGE:**
```
User reports bug → Check logs with trace_id → See entire request flow
```

---

## Role Isolation

Even though everything runs in one process, roles are isolated:

### Resource Isolation

```toml
# forge.toml

[function]
max_concurrent = 1000
timeout = "30s"
memory_limit = "512Mi"  # Per-function limit

[worker]
max_concurrent_jobs = 50
job_timeout = "1h"
```

### Error Isolation

A panic in one function doesn't crash the node:

```rust
// FORGE wraps function execution
async fn execute_function<F, R>(f: F) -> Result<R>
where
    F: FnOnce() -> R + UnwindSafe,
{
    match catch_unwind(f) {
        Ok(result) => Ok(result),
        Err(panic) => {
            // Log, increment error metric, continue serving
            metrics::increment("function_panics");
            Err(Error::FunctionPanicked)
        }
    }
}
```

### Thread Pool Separation

```
┌─────────────────────────────────────────────────────────────────┐
│                     FORGE RUNTIME                                │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  Tokio Runtime (shared)                                          │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │                                                             │ │
│  │  Gateway Tasks         Function Tasks       Worker Tasks    │ │
│  │  ┌─────────────┐      ┌─────────────┐      ┌─────────────┐ │ │
│  │  │ HTTP Conn 1 │      │ Query 1     │      │ Job 1       │ │ │
│  │  │ HTTP Conn 2 │      │ Query 2     │      │ Job 2       │ │ │
│  │  │ WS Conn 1   │      │ Mutation 1  │      │ Job 3       │ │ │
│  │  │ WS Conn 2   │      │ ...         │      │ ...         │ │ │
│  │  └─────────────┘      └─────────────┘      └─────────────┘ │ │
│  │                                                             │ │
│  │  All tasks share the thread pool but are independent        │ │
│  │  A slow job doesn't block queries (async/await)             │ │
│  └────────────────────────────────────────────────────────────┘ │
│                                                                  │
│  Blocking Thread Pool (for CPU-heavy work)                       │
│  ┌────────────────────────────────────────────────────────────┐ │
│  │  spawn_blocking() tasks: crypto, compression, etc.          │ │
│  └────────────────────────────────────────────────────────────┘ │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## Deployment Patterns

### Pattern 1: All Roles (Default)

Every node does everything. Simple and effective for most apps.

```toml
[node]
roles = ["gateway", "function", "worker", "scheduler"]
```

```
     Load Balancer
           │
    ┌──────┼──────┐
    │      │      │
    ▼      ▼      ▼
┌──────┐┌──────┐┌──────┐
│Node 1││Node 2││Node 3│
│ G F  ││ G F  ││ G F  │  G = Gateway
│ W S  ││ W S  ││ W S  │  F = Function
└──────┘└──────┘└──────┘  W = Worker
                          S = Scheduler (only 1 leader)
```

### Pattern 2: Gateway/Worker Split

Separate external traffic from background processing.

```toml
# Gateway nodes
[node]
roles = ["gateway", "function"]

# Worker nodes
[node]
roles = ["worker", "scheduler"]
```

```
     Load Balancer
           │
    ┌──────┼──────┐
    │      │      │
    ▼      ▼      ▼
┌──────┐┌──────┐┌──────┐
│Gate 1││Gate 2││Gate 3│ ← Handle user traffic
│ G F  ││ G F  ││ G F  │
└──────┘└──────┘└──────┘
    │      │      │
    └──────┼──────┘
           │ (gRPC mesh)
    ┌──────┼──────┐
    │      │      │
    ▼      ▼      ▼
┌──────┐┌──────┐┌──────┐
│Work 1││Work 2││Work 3│ ← Process jobs
│ W S  ││ W    ││ W    │
└──────┘└──────┘└──────┘
```

### Pattern 3: Specialized Workers

Different worker pools for different workloads.

```toml
# General workers
[node]
roles = ["worker"]
worker_capabilities = ["general"]

# Media workers (ffmpeg, more CPU)
[node]
roles = ["worker"]
worker_capabilities = ["media"]

# ML workers (GPU)
[node]
roles = ["worker"]
worker_capabilities = ["ml"]
```

```
     Load Balancer
           │
    ┌──────┴──────┐
    ▼             ▼
┌──────┐      ┌──────┐
│Gate 1│      │Gate 2│
│ G F  │      │ G F  │
└──────┘      └──────┘
    │             │
    └──────┬──────┘
           │
    ┌──────┼──────┬──────┐
    │      │      │      │
    ▼      ▼      ▼      ▼
┌──────┐┌──────┐┌──────┐┌──────┐
│Gen 1 ││Gen 2 ││Media ││ ML   │
│W(gen)││W(gen)││W(med)││W(ml) │
│      ││      ││ FFmpeg││ GPU  │
└──────┘└──────┘└──────┘└──────┘
```

→ See [Workers](../cluster/WORKERS.md) for detailed worker configuration.

---

## Memory Layout

A single FORGE node's memory:

```
┌─────────────────────────────────────────────────────────────────┐
│                    PROCESS MEMORY                                │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Static Data (~10 MB)                                    │    │
│  │  • Compiled schema definitions                           │    │
│  │  • Route tables                                          │    │
│  │  • Configuration                                         │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Connection Pools (~50 MB per 100 connections)           │    │
│  │  • PostgreSQL connections                                │    │
│  │  • gRPC connections to peers                             │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Query Cache (~100 MB, configurable)                     │    │
│  │  • Recent query results                                  │    │
│  │  • LRU eviction                                          │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Metrics Buffer (~20 MB)                                 │    │
│  │  • Pre-aggregated metrics waiting for flush              │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │  Active Requests (variable)                              │    │
│  │  • In-flight function executions                         │    │
│  │  • WebSocket session state                               │    │
│  │  • Job processing state                                  │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
│  Typical total: 200-500 MB (without large workloads)            │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

---

## Binary Size

The FORGE binary is self-contained:

```
┌───────────────────────────────────────────────────────┐
│           FORGE BINARY (~50-80 MB)                    │
├───────────────────────────────────────────────────────┤
│                                                       │
│  Core Runtime                          ~10 MB         │
│  ├─ Tokio async runtime                               │
│  ├─ HTTP/WebSocket server (hyper)                     │
│  └─ gRPC (tonic)                                      │
│                                                       │
│  Database Layer                        ~5 MB          │
│  ├─ sqlx (PostgreSQL driver)                          │
│  └─ Connection pooling                                │
│                                                       │
│  Application Code                      ~5-20 MB       │
│  ├─ Your schema definitions                           │
│  ├─ Your functions                                    │
│  └─ Your jobs/workflows                               │
│                                                       │
│  Observability                         ~5 MB          │
│  ├─ Metrics collection                                │
│  ├─ Structured logging                                │
│  └─ Tracing                                           │
│                                                       │
│  Dashboard (embedded)                  ~5 MB          │
│  └─ Svelte SPA (gzipped)                              │
│                                                       │
│  Cluster Mesh                          ~3 MB          │
│  ├─ Gossip protocol                                   │
│  └─ Leader election                                   │
│                                                       │
│  Other Dependencies                    ~10-30 MB      │
│  └─ Crypto, compression, etc.                         │
│                                                       │
└───────────────────────────────────────────────────────┘
```

---

## Comparison

### vs Microservices

| Aspect | Microservices | FORGE |
|--------|--------------|-------|
| Deployment units | 5-20 services | 1 binary |
| Network hops | Multiple | Zero (in-process) |
| Version skew | Common problem | Impossible |
| Debugging | Distributed tracing required | Stack traces work |
| Scaling | Per-service | Per-node |

### vs Traditional Monolith

| Aspect | Monolith | FORGE |
|--------|----------|-------|
| Scaling | Vertical only | Horizontal (cluster) |
| Failure blast radius | Whole app | Single node |
| Deployment | Big bang | Rolling update |
| Team coupling | High | Low (module boundaries) |

---

## Related Documentation

- [Clustering](../cluster/CLUSTERING.md) — How nodes form a cluster
- [Leader Election](../cluster/LEADER_ELECTION.md) — Scheduler leader selection
- [Workers](../cluster/WORKERS.md) — Specialized worker pools
- [Deployment](../deployment/DEPLOYMENT.md) — How to deploy FORGE

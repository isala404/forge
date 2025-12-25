# Architecture Overview

> *The 10,000-foot view of how FORGE works*

---

## Design Philosophy

FORGE is built on a simple premise: **modern applications are over-engineered**. 

A typical stack looks like:
- PostgreSQL for data
- Redis for caching, sessions, and job queues
- Kafka/RabbitMQ for events
- Prometheus for metrics
- Loki for logs
- Jaeger for traces
- Kubernetes for orchestration
- 15 microservices for "separation of concerns"

FORGE collapses this into:
- **PostgreSQL** for everything persistent
- **FORGE binary** for everything else

---

## System Architecture

```
┌─────────────────────────────────────────────────────────────────────────────────┐
│                                   CLIENTS                                        │
│                                                                                  │
│   ┌──────────────┐  ┌──────────────┐  ┌──────────────┐  ┌──────────────┐        │
│   │  Web App     │  │  Mobile App  │  │  CLI Tool    │  │  Webhook     │        │
│   │  (Svelte)    │  │  (Native)    │  │              │  │  Sender      │        │
│   └──────┬───────┘  └──────┬───────┘  └──────┬───────┘  └──────┬───────┘        │
│          │                 │                 │                 │                 │
└──────────┼─────────────────┼─────────────────┼─────────────────┼─────────────────┘
           │                 │                 │                 │
           │  WebSocket      │  HTTP           │  HTTP           │  HTTP
           │                 │                 │                 │
           ▼                 ▼                 ▼                 ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│                              LOAD BALANCER                                       │
│                      (nginx, traefik, cloud LB, etc.)                           │
└─────────────────────────────────────────────────────────────────────────────────┘
                                      │
                    ┌─────────────────┼─────────────────┐
                    │                 │                 │
                    ▼                 ▼                 ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│                              FORGE CLUSTER                                       │
│                                                                                  │
│   ┌───────────────────┐   ┌───────────────────┐   ┌───────────────────┐         │
│   │      Node 1       │   │      Node 2       │   │      Node 3       │         │
│   │                   │   │                   │   │                   │         │
│   │  ┌─────────────┐  │   │  ┌─────────────┐  │   │  ┌─────────────┐  │         │
│   │  │  Gateway    │  │   │  │  Gateway    │  │   │  │  Gateway    │  │         │
│   │  │  (HTTP/WS)  │  │   │  │  (HTTP/WS)  │  │   │  │  (HTTP/WS)  │  │         │
│   │  └─────────────┘  │   │  └─────────────┘  │   │  └─────────────┘  │         │
│   │  ┌─────────────┐  │   │  ┌─────────────┐  │   │  ┌─────────────┐  │         │
│   │  │  Functions  │  │   │  │  Functions  │  │   │  │  Functions  │  │         │
│   │  │  Executor   │  │   │  │  Executor   │  │   │  │  Executor   │  │         │
│   │  └─────────────┘  │   │  └─────────────┘  │   │  └─────────────┘  │         │
│   │  ┌─────────────┐  │   │  ┌─────────────┐  │   │  ┌─────────────┐  │         │
│   │  │   Worker    │  │   │  │   Worker    │  │   │  │   Worker    │  │         │
│   │  │   (jobs)    │  │   │  │   (jobs)    │  │   │  │   (media)   │  │         │
│   │  └─────────────┘  │   │  └─────────────┘  │   │  └─────────────┘  │         │
│   │  ┌─────────────┐  │   │  ┌─────────────┐  │   │  ┌─────────────┐  │         │
│   │  │ Scheduler   │  │   │  │ Scheduler   │  │   │  │ Scheduler   │  │         │
│   │  │ (standby)   │  │   │  │ ★ LEADER ★  │  │   │  │ (standby)   │  │         │
│   │  └─────────────┘  │   │  └─────────────┘  │   │  └─────────────┘  │         │
│   │  ┌─────────────┐  │   │  ┌─────────────┐  │   │  ┌─────────────┐  │         │
│   │  │ Observability│ │   │  │ Observability│ │   │  │ Observability│ │         │
│   │  │  Collector  │  │   │  │  Collector  │  │   │  │  Collector  │  │         │
│   │  └─────────────┘  │   │  └─────────────┘  │   │  └─────────────┘  │         │
│   │                   │   │                   │   │                   │         │
│   └─────────┬─────────┘   └─────────┬─────────┘   └─────────┬─────────┘         │
│             │                       │                       │                    │
│             └───────────────────────┼───────────────────────┘                    │
│                                     │                                            │
│                              gRPC Mesh (inter-node)                              │
│                                     │                                            │
└─────────────────────────────────────┼────────────────────────────────────────────┘
                                      │
                                      ▼
┌─────────────────────────────────────────────────────────────────────────────────┐
│                                POSTGRESQL                                        │
│                                                                                  │
│   ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐            │
│   │   App Data  │  │  Job Queue  │  │   Events    │  │   Cluster   │            │
│   │   Tables    │  │   Tables    │  │    Log      │  │    State    │            │
│   └─────────────┘  └─────────────┘  └─────────────┘  └─────────────┘            │
│   ┌─────────────┐  ┌─────────────┐  ┌─────────────┐  ┌─────────────┐            │
│   │   Metrics   │  │    Logs     │  │   Traces    │  │   Sessions  │            │
│   │   Storage   │  │   Storage   │  │   Storage   │  │   Storage   │            │
│   └─────────────┘  └─────────────┘  └─────────────┘  └─────────────┘            │
│                                                                                  │
└─────────────────────────────────────────────────────────────────────────────────┘
```

---

## Core Components

### 1. Gateway Layer

The Gateway handles all external communication:

| Protocol | Purpose | Features |
|----------|---------|----------|
| HTTP | REST-like API calls | Request validation, rate limiting |
| WebSocket | Real-time subscriptions | Auto-reconnect, heartbeat |
| gRPC | Internal mesh | Service-to-service calls |

Every node can serve as a gateway. Load balancers distribute traffic across all gateway-enabled nodes.

→ See [Data Flow](DATA_FLOW.md) for request routing details.

### 2. Function Executor

Executes your application logic:

| Function Type | Characteristics | Use Case |
|---------------|-----------------|----------|
| **Query** | Read-only, cached, subscribable | Fetching data |
| **Mutation** | Transactional, writes data | Creating/updating data |
| **Action** | Can call external services | Third-party APIs |

→ See [Functions](../core/FUNCTIONS.md) for the complete guide.

### 3. Worker Pool

Processes background work:

| Work Type | Execution | Guarantees |
|-----------|-----------|------------|
| **Jobs** | Async, queued | At-least-once, retries |
| **Crons** | Scheduled | Exactly-once per interval |
| **Workflows** | Multi-step | Saga pattern, compensation |

Workers can be specialized (media, ML, general) and are routed work based on capabilities.

→ See [Jobs](../core/JOBS.md) and [Workers](../cluster/WORKERS.md).

### 4. Scheduler

The scheduler is a **singleton leader** (exactly one active across the cluster):

- Assigns jobs to workers
- Triggers cron jobs
- Manages dead letter queue
- Coordinates cluster-wide tasks

Leader election uses PostgreSQL advisory locks.

→ See [Leader Election](../cluster/LEADER_ELECTION.md).

### 5. Observability Collector

Every node collects:

- **Metrics**: Function latency, job throughput, error rates
- **Logs**: Structured JSON, contextual
- **Traces**: Distributed, cross-node correlation

Data is stored in PostgreSQL, queryable via the built-in dashboard.

→ See [Observability](../observability/OBSERVABILITY.md).

---

## Data Storage Model

PostgreSQL serves multiple purposes:

```
┌─────────────────────────────────────────────────────────────────┐
│                       POSTGRESQL                                 │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │                  APPLICATION DATA                        │    │
│  │  • User-defined tables (from schema)                     │    │
│  │  • Automatically indexed                                 │    │
│  │  • Change tracking via triggers                          │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │                    SYSTEM TABLES                         │    │
│  │  • forge_jobs (job queue with SKIP LOCKED)               │    │
│  │  • forge_events (change log for reactivity)              │    │
│  │  • forge_nodes (cluster membership)                      │    │
│  │  • forge_leaders (leader election)                       │    │
│  │  • forge_sessions (WebSocket sessions)                   │    │
│  │  • forge_locks (distributed locks)                       │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
│  ┌─────────────────────────────────────────────────────────┐    │
│  │                  OBSERVABILITY DATA                      │    │
│  │  • forge_metrics (time-series, partitioned)              │    │
│  │  • forge_logs (structured logs, partitioned)             │    │
│  │  • forge_traces (spans, partitioned)                     │    │
│  └─────────────────────────────────────────────────────────┘    │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

→ See [PostgreSQL Schema](../database/POSTGRES_SCHEMA.md) for complete table definitions.

---

## Request Types

### Synchronous Requests

Client waits for response:

```
Client → Gateway → Function Executor → PostgreSQL → Response
                                                 ↓
                                            (< 100ms typical)
```

### Asynchronous Requests

Client gets acknowledgment, work happens later:

```
Client → Gateway → Mutation (dispatch job) → Response (job_id)
                           ↓
                    Job Queue (PostgreSQL)
                           ↓
                    Worker picks up job
                           ↓
                    Job executes (seconds to hours)
                           ↓
                    Client notified via subscription
```

### Subscriptions

Client receives updates when data changes:

```
Client → Gateway → Subscribe to query
                       ↓
                  Initial result sent
                       ↓
                  (data changes elsewhere)
                       ↓
                  Change detected
                       ↓
                  Query re-executed
                       ↓
                  Delta sent to client
```

→ See [Reactivity](../core/REACTIVITY.md) for subscription internals.

---

## Scaling Model

### Horizontal Scaling

Add more nodes. They auto-join the cluster.

```bash
# Start with 1 node
docker run forge

# Scale to 5 nodes
docker-compose up --scale forge=5
```

All nodes are equal. Specialized roles emerge through configuration.

### Vertical Scaling

Give nodes more resources:

```yaml
# More concurrent functions
[function]
max_concurrent = 2000

# More concurrent jobs
[worker]
max_concurrent_jobs = 100
```

### Database Scaling

PostgreSQL scales separately:

| Technique | When to Use |
|-----------|-------------|
| Connection pooling (PgBouncer) | > 100 concurrent connections |
| Read replicas | Read-heavy workloads |
| Table partitioning | Large tables (> 100M rows) |
| Citus/Sharding | Extreme scale (rare) |

→ See [PostgreSQL Schema](../database/POSTGRES_SCHEMA.md#scaling) for details.

---

## Failure Modes

### Node Failure

- **Stateless**: All state is in PostgreSQL
- **Auto-failover**: Other nodes take over
- **No data loss**: Transactions are durable

### Leader Failure

- **Detection**: Heartbeat timeout (5 seconds)
- **Election**: PostgreSQL advisory lock transfer
- **Recovery**: New leader resumes scheduling

→ See [Leader Election](../cluster/LEADER_ELECTION.md#failure-handling).

### Database Failure

- **Connection retry**: Exponential backoff
- **Query timeout**: Configurable limits
- **Graceful degradation**: Cache recent results

---

## Security Model

```
┌─────────────────────────────────────────────────────────────────┐
│                     SECURITY LAYERS                              │
├─────────────────────────────────────────────────────────────────┤
│                                                                  │
│  Layer 1: Transport                                              │
│  • TLS for all connections (HTTP, WebSocket, gRPC)               │
│  • Certificate validation                                        │
│                                                                  │
│  Layer 2: Authentication                                         │
│  • JWT validation (external providers)                           │
│  • API keys for service-to-service                               │
│  • Session management                                            │
│                                                                  │
│  Layer 3: Authorization                                          │
│  • Function-level permissions                                    │
│  • Row-level security (RLS in PostgreSQL)                        │
│  • Resource ownership                                            │
│                                                                  │
│  Layer 4: Data Protection                                        │
│  • Encryption at rest (optional, field-level)                    │
│  • Audit logging                                                 │
│  • PII handling                                                  │
│                                                                  │
└─────────────────────────────────────────────────────────────────┘
```

→ See [Security](../reference/SECURITY.md) for implementation details.

---

## Related Documentation

- [Single Binary Design](SINGLE_BINARY.md) — Why one binary, how roles work
- [Data Flow](DATA_FLOW.md) — How requests move through the system
- [Clustering](../cluster/CLUSTERING.md) — How nodes form a cluster
- [PostgreSQL Schema](../database/POSTGRES_SCHEMA.md) — All database tables

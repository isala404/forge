# Clustering

FORGE implements a distributed cluster system using PostgreSQL as the sole coordination layer. Nodes automatically discover each other, elect leaders, and coordinate through the database without requiring additional infrastructure like Redis, etcd, or ZooKeeper.

---

## Core Types

### NodeId

A unique identifier for each node in the cluster.

```rust
// crates/forge-core/src/cluster/node.rs
pub struct NodeId(pub Uuid);
```

**Methods:**
- `NodeId::new()` - Generate a new random node ID
- `NodeId::from_uuid(uuid)` - Create from an existing UUID
- `NodeId::as_uuid()` - Get the inner UUID

NodeId implements `Default` (generates a new ID) and `Display` (formats as UUID string).

### NodeStatus

Represents the lifecycle state of a node.

```rust
// crates/forge-core/src/cluster/node.rs
pub enum NodeStatus {
    Joining,   // Node is starting up
    Active,    // Node is healthy and active
    Draining,  // Node is shutting down gracefully
    Dead,      // Node has stopped sending heartbeats
}
```

**Methods:**
- `as_str()` - Convert to database string ("joining", "active", "draining", "dead")
- `can_accept_work()` - Returns true only for `Active` status

Implements `FromStr` for parsing from database values. Unknown values default to `Dead`.

### NodeRole

Defines the roles a node can perform in the cluster.

```rust
// crates/forge-core/src/cluster/roles.rs
pub enum NodeRole {
    Gateway,    // HTTP gateway for client requests
    Function,   // Function executor
    Worker,     // Background job worker
    Scheduler,  // Scheduler (leader-only) for crons and job assignment
}
```

**Methods:**
- `as_str()` - Convert to database string
- `all()` - Returns all four roles

A single FORGE binary can run any combination of roles. By default, all roles are enabled.

### NodeInfo

Complete information about a node in the cluster.

```rust
// crates/forge-core/src/cluster/node.rs
pub struct NodeInfo {
    pub id: NodeId,
    pub hostname: String,
    pub ip_address: IpAddr,
    pub http_port: u16,
    pub grpc_port: u16,
    pub roles: Vec<NodeRole>,
    pub worker_capabilities: Vec<String>,
    pub status: NodeStatus,
    pub last_heartbeat: DateTime<Utc>,
    pub version: String,
    pub started_at: DateTime<Utc>,
    pub current_connections: u32,
    pub current_jobs: u32,
    pub cpu_usage: f32,
    pub memory_usage: f32,
}
```

**Methods:**
- `new_local(hostname, ip, http_port, grpc_port, roles, capabilities, version)` - Create info for the local node
- `has_role(role)` - Check if node has a specific role
- `has_capability(capability)` - Check for worker capability
- `load()` - Calculate node load (0.0 to 1.0) as average of CPU and memory usage

---

## Leader Election

### LeaderRole

Defines roles that require exactly one active leader across the cluster.

```rust
// crates/forge-core/src/cluster/roles.rs
pub enum LeaderRole {
    Scheduler,          // Job assignment and cron triggering
    MetricsAggregator,  // Metrics aggregation
    LogCompactor,       // Log compaction
}
```

**Advisory Lock IDs:**

Each leader role has a unique PostgreSQL advisory lock ID derived from "FORGE" in hex:
- `Scheduler`: `0x464F_5247_0001`
- `MetricsAggregator`: `0x464F_5247_0002`
- `LogCompactor`: `0x464F_5247_0003`

### LeaderInfo

Information about current leadership for a role.

```rust
// crates/forge-core/src/cluster/traits.rs
pub struct LeaderInfo {
    pub role: LeaderRole,
    pub node_id: NodeId,
    pub acquired_at: DateTime<Utc>,
    pub lease_until: DateTime<Utc>,
}
```

**Methods:**
- `is_valid()` - Check if the lease is still valid (lease_until > now)

### LeaderElection

The runtime component that manages leader election using PostgreSQL advisory locks.

```rust
// crates/forge-runtime/src/cluster/leader.rs
pub struct LeaderElection {
    pool: PgPool,
    node_id: NodeId,
    role: LeaderRole,
    config: LeaderConfig,
    is_leader: Arc<AtomicBool>,
    // ...
}
```

**Configuration:**

```rust
pub struct LeaderConfig {
    pub check_interval: Duration,    // Default: 5 seconds
    pub lease_duration: Duration,    // Default: 60 seconds
    pub refresh_interval: Duration,  // Default: 30 seconds
}
```

**Key Methods:**

- `new(pool, node_id, role, config)` - Create a new election instance
- `is_leader()` - Check if this node is currently the leader
- `try_become_leader()` - Attempt to acquire leadership (non-blocking)
- `refresh_lease()` - Refresh the leadership lease
- `release_leadership()` - Voluntarily release leadership
- `check_leader_health()` - Check if current leader's lease is valid
- `get_leader()` - Get current leader info
- `run()` - Run the election loop

**Election Algorithm:**

1. Use `pg_try_advisory_lock(lock_id)` to attempt lock acquisition
2. If acquired, record leadership in `forge_leaders` table with lease expiry
3. Periodically refresh lease by updating `lease_until`
4. Standbys check leader health by verifying lease validity
5. If lease expired or no leader, standbys attempt to acquire lock
6. On shutdown, release lock with `pg_advisory_unlock(lock_id)`

### LeaderGuard

RAII guard for leader-only operations.

```rust
// crates/forge-runtime/src/cluster/leader.rs
pub struct LeaderGuard<'a> {
    election: &'a LeaderElection,
}
```

**Methods:**
- `try_new(election)` - Returns `Some(guard)` if currently leader, `None` otherwise
- `is_leader()` - Check if still leader

---

## Node Registry

Manages node membership in the cluster.

```rust
// crates/forge-runtime/src/cluster/registry.rs
pub struct NodeRegistry {
    pool: PgPool,
    local_node: NodeInfo,
}
```

**Methods:**

| Method | Description |
|--------|-------------|
| `register()` | Register local node (INSERT with ON CONFLICT UPDATE) |
| `set_status(status)` | Update node status |
| `deregister()` | Remove node from registry |
| `get_active_nodes()` | Get all active nodes |
| `get_nodes_by_status(status)` | Get nodes with specific status |
| `get_node(node_id)` | Get specific node by ID |
| `count_by_status()` | Get node counts grouped by status |
| `mark_dead_nodes(threshold)` | Mark nodes as dead if heartbeat older than threshold |
| `cleanup_dead_nodes(older_than)` | Delete dead nodes older than duration |

### NodeCounts

Statistics about node distribution.

```rust
pub struct NodeCounts {
    pub active: usize,
    pub draining: usize,
    pub dead: usize,
    pub joining: usize,
    pub total: usize,
}
```

---

## Heartbeat System

Maintains node health through periodic heartbeat updates.

```rust
// crates/forge-runtime/src/cluster/heartbeat.rs
pub struct HeartbeatLoop {
    pool: PgPool,
    node_id: NodeId,
    config: HeartbeatConfig,
    // ...
}
```

**Configuration:**

```rust
pub struct HeartbeatConfig {
    pub interval: Duration,        // Default: 5 seconds
    pub dead_threshold: Duration,  // Default: 15 seconds
    pub mark_dead_nodes: bool,     // Default: true
}
```

**Behavior:**

1. Every `interval`, update `last_heartbeat = NOW()` for local node
2. If `mark_dead_nodes` is true, mark nodes as dead if their `last_heartbeat` is older than `dead_threshold`
3. Optionally update load metrics (connections, jobs, CPU, memory)

**Methods:**

| Method | Description |
|--------|-------------|
| `run()` | Start the heartbeat loop |
| `stop()` | Stop the loop gracefully |
| `is_running()` | Check if loop is running |
| `update_load(...)` | Update node load metrics |

### Dead Node Detection

The heartbeat loop automatically detects dead nodes:

```sql
UPDATE forge_nodes
SET status = 'dead'
WHERE status = 'active'
  AND last_heartbeat < NOW() - make_interval(secs => threshold)
```

When a node is marked dead:
- Its jobs can be reassigned to other workers
- Any leader locks it held are automatically released by PostgreSQL
- Load balancers stop routing traffic to it

---

## Graceful Shutdown

Coordinates clean shutdown of a node.

```rust
// crates/forge-runtime/src/cluster/shutdown.rs
pub struct GracefulShutdown {
    registry: Arc<NodeRegistry>,
    leader_election: Option<Arc<LeaderElection>>,
    config: ShutdownConfig,
    in_flight_count: Arc<AtomicU32>,
    // ...
}
```

**Configuration:**

```rust
pub struct ShutdownConfig {
    pub drain_timeout: Duration,  // Default: 30 seconds
    pub poll_interval: Duration,  // Default: 100 milliseconds
}
```

**Shutdown Sequence:**

1. Set `shutdown_requested` flag to stop accepting new work
2. Broadcast shutdown signal to all listeners
3. Set node status to `Draining`
4. Wait for in-flight requests to complete (with timeout)
5. Leader lock release is handled by LeaderElection's run loop on shutdown
6. Deregister from cluster (DELETE from forge_nodes)

**Methods:**

| Method | Description |
|--------|-------------|
| `is_shutdown_requested()` | Check if shutdown has been initiated |
| `in_flight_count()` | Get current in-flight request count |
| `should_accept_work()` | Check if new work should be accepted |
| `shutdown()` | Perform graceful shutdown |
| `subscribe()` | Subscribe to shutdown notifications |

### InFlightGuard

RAII guard for tracking in-flight requests.

```rust
pub struct InFlightGuard {
    shutdown: Arc<GracefulShutdown>,
}
```

**Usage:**
- `try_new(shutdown)` - Returns `None` if shutdown is in progress, preventing new work
- On drop, automatically decrements in-flight counter

---

## Database Schema

### forge_nodes

Stores cluster membership information.

```sql
CREATE TABLE IF NOT EXISTS forge_nodes (
    id UUID PRIMARY KEY,
    hostname VARCHAR(255) NOT NULL,
    ip_address VARCHAR(64) NOT NULL,
    http_port INTEGER NOT NULL,
    grpc_port INTEGER NOT NULL,
    roles TEXT[] NOT NULL DEFAULT '{}',
    worker_capabilities TEXT[] NOT NULL DEFAULT '{}',
    status VARCHAR(32) NOT NULL DEFAULT 'starting',
    version VARCHAR(64),
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    last_heartbeat TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
```

Additional columns tracked at runtime:
- `current_connections` - Active HTTP/WebSocket connections
- `current_jobs` - Jobs currently being processed
- `cpu_usage` - CPU usage percentage
- `memory_usage` - Memory usage percentage

### forge_leaders

Tracks current leadership for each role.

```sql
CREATE TABLE IF NOT EXISTS forge_leaders (
    role VARCHAR(64) PRIMARY KEY,
    node_id UUID NOT NULL,
    acquired_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    lease_until TIMESTAMPTZ NOT NULL
);
```

The `role` column is the primary key, ensuring exactly one leader per role. The actual lock is held via PostgreSQL advisory locks; this table provides visibility into current leadership.

---

## Cluster Information Types

### ClusterInfo

Aggregate information about the cluster.

```rust
// crates/forge-core/src/cluster/traits.rs
pub struct ClusterInfo {
    pub name: String,
    pub total_nodes: usize,
    pub active_nodes: usize,
    pub draining_nodes: usize,
    pub dead_nodes: usize,
    pub scheduler_leader: Option<NodeId>,
}
```

---

## Key Patterns

### PostgreSQL as Single Source of Truth

The cluster uses PostgreSQL for all coordination:
- Node discovery via `forge_nodes` table
- Leader election via advisory locks + `forge_leaders` table
- Health tracking via heartbeat timestamps

This eliminates split-brain scenarios - if a node cannot reach PostgreSQL, it cannot participate in the cluster.

### Advisory Lock Semantics

PostgreSQL advisory locks have important properties:
- Released automatically when connection closes (handles crashes)
- `pg_try_advisory_lock()` is non-blocking (returns immediately)
- `pg_advisory_unlock()` releases explicitly
- Session-level locks persist until connection ends

### Lease-Based Leadership

While advisory locks provide mutual exclusion, the lease system provides:
- Visibility into who is leader (queryable via SQL)
- Health checking by standbys (verify lease validity)
- Graceful handoff (release lock, clear lease record)

---

## Implementation Notes

### What Was Implemented

1. **Core Types** - NodeId, NodeInfo, NodeStatus, NodeRole, LeaderRole all implemented
2. **Leader Election** - Using `pg_try_advisory_lock` with lease tracking
3. **Heartbeat Loop** - Configurable interval with dead node detection
4. **Graceful Shutdown** - Drain timeout with in-flight request tracking
5. **Node Registry** - Full CRUD operations for cluster membership

### Differences from Proposal

The proposal included some features not yet implemented:

| Proposed Feature | Implementation Status |
|-----------------|----------------------|
| gRPC mesh between nodes | Not implemented - nodes communicate via PostgreSQL |
| DNS/Kubernetes discovery | Not implemented - PostgreSQL-only discovery |
| Cluster CLI commands (`forge cluster status`, `forge node drain`) | Not implemented |
| Minimum nodes before accepting traffic | Not implemented |
| Auto-scaling integration | Not documented in implementation |

The current implementation focuses on PostgreSQL-based coordination without the inter-node gRPC mesh. Nodes discover each other through the database and do not establish direct connections.

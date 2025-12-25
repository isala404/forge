# Clustering

> *Self-organizing distributed systems*

---

## Overview

FORGE nodes automatically form a **self-organizing cluster**. There's no manual configuration of "this node talks to that node"—nodes discover each other and coordinate automatically.

Key properties:
- **Automatic discovery**: Nodes find each other
- **No single point of failure**: Any node can fail
- **Consistent state**: PostgreSQL as source of truth
- **Dynamic membership**: Nodes join and leave freely

---

## Cluster Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                           FORGE CLUSTER                                      │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │                      CLUSTER MESH                                    │   │
│   │                                                                      │   │
│   │     ┌─────────┐       ┌─────────┐       ┌─────────┐                 │   │
│   │     │ Node 1  │◄─────►│ Node 2  │◄─────►│ Node 3  │                 │   │
│   │     │         │       │         │       │         │                 │   │
│   │     │ G F W S │       │ G F W   │       │   W     │                 │   │
│   │     └────┬────┘       └────┬────┘       └────┬────┘                 │   │
│   │          │                 │                 │                       │   │
│   │          │                 │                 │                       │   │
│   │          └─────────────────┼─────────────────┘                       │   │
│   │                            │                                         │   │
│   │                      gRPC mesh                                       │   │
│   │                   (full connectivity)                                │   │
│   └─────────────────────────────────────────────────────────────────────┘   │
│                                                                              │
│   Legend: G=Gateway, F=Function, W=Worker, S=Scheduler(leader)              │
│                                                                              │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │                    COORDINATION LAYER                                │   │
│   │                                                                      │   │
│   │   ┌─────────────────┐  ┌─────────────────┐  ┌─────────────────┐    │   │
│   │   │  Node Registry  │  │ Leader Election │  │  Health Checks  │    │   │
│   │   │  (forge_nodes)  │  │ (forge_leaders) │  │   (heartbeat)   │    │   │
│   │   └─────────────────┘  └─────────────────┘  └─────────────────┘    │   │
│   │                                                                      │   │
│   │                     All backed by PostgreSQL                         │   │
│   └─────────────────────────────────────────────────────────────────────┘   │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Node Lifecycle

### Joining the Cluster

```
┌─────────┐
│  START  │
└────┬────┘
     │
     │  1. Load configuration
     ▼
┌─────────────────────────────────────────┐
│  Connect to PostgreSQL                   │
│  (verify connectivity)                   │
└────────────────┬────────────────────────┘
                 │
                 │  2. Register self
                 ▼
┌─────────────────────────────────────────┐
│  INSERT INTO forge_nodes                 │
│  (id, ip, port, roles, capabilities)    │
└────────────────┬────────────────────────┘
                 │
                 │  3. Discover peers
                 ▼
┌─────────────────────────────────────────┐
│  SELECT * FROM forge_nodes               │
│  WHERE status = 'active'                 │
│  AND id != self_id                       │
└────────────────┬────────────────────────┘
                 │
                 │  4. Connect to peers
                 ▼
┌─────────────────────────────────────────┐
│  Establish gRPC connections              │
│  to all discovered peers                 │
└────────────────┬────────────────────────┘
                 │
                 │  5. Start roles
                 ▼
┌─────────────────────────────────────────┐
│  Start enabled roles:                    │
│  - Gateway (if enabled)                  │
│  - Function executor (if enabled)        │
│  - Worker (if enabled)                   │
│  - Try scheduler election (if enabled)   │
└────────────────┬────────────────────────┘
                 │
                 │  6. Begin heartbeat
                 ▼
┌─────────────────────────────────────────┐
│  Update forge_nodes.last_heartbeat       │
│  every 5 seconds                         │
└────────────────┬────────────────────────┘
                 │
                 ▼
          ┌─────────────┐
          │   ACTIVE    │
          └─────────────┘
```

### Node States

| State | Description | Behavior |
|-------|-------------|----------|
| `joining` | Node is starting up | Not receiving traffic |
| `active` | Node is healthy | Receiving traffic |
| `draining` | Node is shutting down gracefully | Finishing current work, not accepting new |
| `dead` | Node hasn't sent heartbeat | Marked by other nodes |

### Graceful Shutdown

```rust
// When node receives SIGTERM:
async fn graceful_shutdown(&self) {
    // 1. Stop accepting new work
    self.set_status(NodeStatus::Draining).await;
    
    // 2. Wait for in-flight requests (with timeout)
    self.wait_for_completion(Duration::seconds(30)).await;
    
    // 3. Release any leader locks
    self.release_leadership().await;
    
    // 4. Disconnect from peers
    self.disconnect_peers().await;
    
    // 5. Update database
    self.db.execute("DELETE FROM forge_nodes WHERE id = $1", &[&self.id]).await;
}
```

---

## PostgreSQL as Coordination Backbone

All cluster state lives in PostgreSQL:

```sql
-- Node registry
CREATE TABLE forge_nodes (
    id UUID PRIMARY KEY,
    hostname VARCHAR(255) NOT NULL,
    ip_address INET NOT NULL,
    grpc_port INTEGER NOT NULL,
    http_port INTEGER NOT NULL,
    
    -- Capabilities
    roles TEXT[] NOT NULL DEFAULT ARRAY['gateway', 'function', 'worker', 'scheduler'],
    worker_capabilities TEXT[] DEFAULT ARRAY['general'],
    
    -- Status
    status VARCHAR(50) NOT NULL DEFAULT 'joining',
    last_heartbeat TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    
    -- Metadata
    version VARCHAR(50),
    started_at TIMESTAMPTZ DEFAULT NOW(),
    
    -- Load tracking
    current_connections INTEGER DEFAULT 0,
    current_jobs INTEGER DEFAULT 0,
    cpu_usage FLOAT DEFAULT 0,
    memory_usage FLOAT DEFAULT 0
);

-- Index for quick health checks
CREATE INDEX idx_forge_nodes_heartbeat ON forge_nodes(last_heartbeat);
CREATE INDEX idx_forge_nodes_status ON forge_nodes(status);
```

### Why PostgreSQL?

1. **Already required**: No new dependencies
2. **ACID guarantees**: Leader election is safe
3. **Battle-tested**: 30+ years of production use
4. **Simple**: No Raft/Paxos complexity to maintain

---

## Health Checking

### Heartbeat Loop

Every node runs a heartbeat loop:

```rust
async fn heartbeat_loop(&self) {
    let interval = Duration::seconds(5);
    
    loop {
        // Update our heartbeat
        sqlx::query("UPDATE forge_nodes SET last_heartbeat = NOW() WHERE id = $1")
            .bind(&self.id)
            .execute(&self.db)
            .await?;
        
        // Check for dead nodes
        let dead_threshold = Duration::seconds(15);
        sqlx::query("UPDATE forge_nodes SET status = 'dead' WHERE last_heartbeat < NOW() - $1 AND status = 'active'")
            .bind(&dead_threshold)
            .execute(&self.db)
            .await?;
        
        // Sleep until next heartbeat
        tokio::time::sleep(interval).await;
    }
}
```

### Dead Node Detection

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                      DEAD NODE DETECTION                                     │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  Time    Node 1         Node 2         Node 3         PostgreSQL            │
│  ────    ──────         ──────         ──────         ──────────            │
│                                                                              │
│  0:00    heartbeat →                                 last_heartbeat=0:00    │
│  0:05    heartbeat →                   heartbeat →   last_heartbeat=0:05    │
│  0:10    [CRASH]                       heartbeat →                          │
│  0:15                   heartbeat →    heartbeat →   Check: Node 1 last     │
│                         checks dead                  heartbeat was 0:05     │
│                         nodes                        (10s ago > threshold)  │
│                                                                              │
│  0:15                   UPDATE forge_nodes                                   │
│                         SET status = 'dead'                                  │
│                         WHERE id = node1_id                                  │
│                                                                              │
│  Node 1's jobs can now be reassigned to other workers                       │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

### What Happens When a Node Dies

1. **Jobs are reassigned**: Claimed jobs return to pending
2. **Subscriptions reconnect**: Clients connect to other nodes
3. **Leader re-election**: If leader died, new leader elected
4. **Load rebalances**: Traffic goes to remaining nodes

---

## Cluster Membership

### Viewing Cluster Status

```bash
# CLI
forge cluster status
```

```
FORGE Cluster: production
═══════════════════════════════════════════════════════════════════

Nodes (3 active, 0 draining, 0 dead):

  NODE ID     HOSTNAME        STATUS    ROLES              LOAD    UPTIME
  ─────────────────────────────────────────────────────────────────────────
  abc-123     forge-1         active    G F W S*           45%     3d 14h
  def-456     forge-2         active    G F W              62%     3d 14h
  ghi-789     forge-3         active    W (media)          28%     1d 2h

  * = scheduler leader

Connections: 1,247 active
Jobs: 23 pending, 8 running
Subscriptions: 3,891 active
```

### Programmatic Access

```rust
// From within a function
#[forge::query]
pub async fn get_cluster_health(ctx: &QueryContext) -> Result<ClusterHealth> {
    let nodes = ctx.cluster.nodes().await;
    let leader = ctx.cluster.scheduler_leader().await;
    
    Ok(ClusterHealth {
        total_nodes: nodes.len(),
        active_nodes: nodes.iter().filter(|n| n.status == "active").count(),
        scheduler_leader: leader.map(|n| n.hostname),
    })
}
```

---

## Network Partitions

### Split-Brain Prevention

FORGE uses PostgreSQL as the single source of truth, which prevents split-brain:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                      NETWORK PARTITION HANDLING                              │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  Scenario: Network splits cluster into two groups                            │
│                                                                              │
│  ┌─────────────────────┐          ┌─────────────────────┐                   │
│  │  Partition A        │    ✗     │  Partition B        │                   │
│  │  Node 1, Node 2     │ ───────  │  Node 3             │                   │
│  │                     │ network  │                     │                   │
│  │  Can reach          │ split    │  Cannot reach       │                   │
│  │  PostgreSQL ✓       │          │  PostgreSQL ✗       │                   │
│  └─────────────────────┘          └─────────────────────┘                   │
│                                                                              │
│  Result:                                                                     │
│  - Partition A continues operating normally                                  │
│  - Partition B cannot update heartbeat → marked dead                         │
│  - Partition B stops accepting work (can't verify auth, etc.)                │
│  - No split-brain: PostgreSQL is single source of truth                      │
│                                                                              │
│  When network heals:                                                         │
│  - Node 3 reconnects to PostgreSQL                                           │
│  - Node 3 re-registers as joining                                            │
│  - Node 3 rejoins cluster                                                    │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

### PostgreSQL Availability

If PostgreSQL becomes unavailable:

```rust
async fn handle_db_unavailable(&self) {
    // 1. Stop accepting new requests
    self.gateway.stop_accepting().await;
    
    // 2. Continue serving cached queries (if enabled)
    // 3. Queue jobs locally (with limit)
    
    // 4. Retry connection with backoff
    let mut backoff = Duration::seconds(1);
    loop {
        match self.db.ping().await {
            Ok(_) => {
                self.gateway.start_accepting().await;
                break;
            }
            Err(_) => {
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::seconds(30));
            }
        }
    }
}
```

---

## Scaling the Cluster

### Adding Nodes

```bash
# Just start a new node pointing to the same database
FORGE_DATABASE_URL=postgres://... forge serve

# It automatically:
# 1. Connects to PostgreSQL
# 2. Discovers existing nodes
# 3. Joins the mesh
# 4. Starts receiving traffic
```

### Removing Nodes

```bash
# Graceful shutdown (SIGTERM)
kill -TERM <pid>

# Or via CLI
forge node drain <node_id>

# Node will:
# 1. Stop accepting new work
# 2. Finish current work
# 3. Deregister from cluster
```

### Auto-Scaling

With Kubernetes HPA:

```yaml
apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata:
  name: forge-hpa
spec:
  scaleTargetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: forge
  minReplicas: 3
  maxReplicas: 20
  metrics:
  - type: Resource
    resource:
      name: cpu
      target:
        type: Utilization
        averageUtilization: 70
  - type: Pods
    pods:
      metric:
        name: forge_jobs_queue_depth
      target:
        type: AverageValue
        averageValue: 100
```

---

## Cluster Configuration

```toml
# forge.toml

[cluster]
# Cluster name (nodes must match to join)
name = "production"

# Discovery method
discovery = "postgres"  # or "dns", "kubernetes", "static"

# Health check intervals
heartbeat_interval = "5s"
dead_threshold = "15s"

# Connection settings
grpc_port = 9000
max_peer_connections = 100

[cluster.membership]
# Minimum nodes before accepting traffic
min_nodes = 1

# Maximum time to wait for minimum nodes
startup_timeout = "60s"
```

---

## Monitoring

### Metrics

| Metric | Description |
|--------|-------------|
| `forge_cluster_nodes_total` | Total registered nodes |
| `forge_cluster_nodes_active` | Currently active nodes |
| `forge_cluster_node_joins_total` | Node join events |
| `forge_cluster_node_leaves_total` | Node leave events |
| `forge_cluster_leader_elections_total` | Leader election events |

### Alerts

```toml
# forge.toml

[[alerts]]
name = "low_node_count"
condition = "forge_cluster_nodes_active < 2"
for = "5m"
severity = "critical"
notify = ["pagerduty"]

[[alerts]]
name = "node_flapping"
condition = "rate(forge_cluster_node_joins_total[5m]) > 10"
severity = "warning"
notify = ["slack:#ops"]
```

---

## Related Documentation

- [Discovery](DISCOVERY.md) — Node discovery mechanisms
- [Meshing](MESHING.md) — Inter-node communication
- [Leader Election](LEADER_ELECTION.md) — Scheduler leader
- [Workers](WORKERS.md) — Worker specialization

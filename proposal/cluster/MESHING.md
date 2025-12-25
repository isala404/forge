# Inter-Node Communication (Meshing)

> *How nodes talk to each other*

---

## Overview

FORGE nodes form a **full mesh network**—every node maintains a connection to every other node. This enables:

- Request forwarding (route to best executor)
- Subscription propagation (notify subscribers on any node)
- Job coordination (workers on any node)
- Cluster state sharing

---

## Mesh Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         FULL MESH TOPOLOGY                                   │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│                            ┌─────────┐                                       │
│                      ┌────►│ Node 1  │◄────┐                                │
│                      │     └────┬────┘     │                                │
│                      │          │          │                                │
│                      │    gRPC  │  gRPC    │                                │
│                      │          │          │                                │
│                 ┌────┴────┐     │     ┌────┴────┐                           │
│                 │ Node 2  │◄────┴────►│ Node 3  │                           │
│                 └────┬────┘           └────┬────┘                           │
│                      │                     │                                │
│                      │                     │                                │
│                      │    ┌─────────┐     │                                │
│                      └───►│ Node 4  │◄────┘                                │
│                           └─────────┘                                       │
│                                                                              │
│   Every node connected to every other node                                   │
│   N nodes = N×(N-1)/2 connections                                           │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## gRPC Protocol

All inter-node communication uses gRPC over HTTP/2:

```protobuf
// forge_internal.proto

syntax = "proto3";

package forge.internal;

service ForgeInternal {
    // Cluster coordination
    rpc Ping(PingRequest) returns (PingResponse);
    rpc GetClusterState(Empty) returns (ClusterState);
    rpc NotifyNodeJoin(NodeJoinNotification) returns (Empty);
    rpc NotifyNodeLeave(NodeLeaveNotification) returns (Empty);
    
    // Function execution
    rpc ExecuteFunction(FunctionRequest) returns (FunctionResponse);
    rpc StreamFunction(FunctionRequest) returns (stream FunctionChunk);
    
    // Subscription management
    rpc PropagateChange(ChangeNotification) returns (Empty);
    rpc BroadcastInvalidation(InvalidationRequest) returns (Empty);
    
    // Job coordination
    rpc NotifyJobAvailable(JobNotification) returns (Empty);
    rpc ReportJobProgress(JobProgressRequest) returns (Empty);
    rpc ReportJobComplete(JobCompletionRequest) returns (Empty);
    
    // Observability
    rpc PushMetrics(MetricsBatch) returns (Empty);
    rpc PushLogs(LogBatch) returns (Empty);
    rpc PushTraceSpans(SpanBatch) returns (Empty);
}

message FunctionRequest {
    string function_name = 1;
    bytes arguments = 2;  // JSON-encoded
    string trace_id = 3;
    string span_id = 4;
    optional string user_token = 5;
}

message FunctionResponse {
    bytes result = 1;  // JSON-encoded
    optional string error = 2;
    repeated SpanData child_spans = 3;
}
```

---

## Connection Management

### Connection Pool

Each node maintains a connection pool to every peer:

```rust
struct PeerConnection {
    node_id: Uuid,
    address: SocketAddr,
    channel: Channel,
    
    // Health tracking
    last_ping: Instant,
    consecutive_failures: u32,
    
    // Load info
    reported_load: f32,
}

struct MeshManager {
    peers: HashMap<Uuid, PeerConnection>,
    
    // Configuration
    max_connections_per_peer: usize,  // Default: 10
    ping_interval: Duration,           // Default: 5s
    reconnect_backoff: Duration,       // Default: 1s
}
```

### Connection Lifecycle

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                    PEER CONNECTION LIFECYCLE                                 │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│   ┌───────────────┐                                                          │
│   │  DISCOVERED   │ ◄─── Node found via discovery                            │
│   └───────┬───────┘                                                          │
│           │                                                                  │
│           │  Attempt gRPC connection                                         │
│           ▼                                                                  │
│   ┌───────────────┐                                                          │
│   │  CONNECTING   │                                                          │
│   └───────┬───────┘                                                          │
│           │                                                                  │
│      ┌────┴────┐                                                             │
│      │         │                                                             │
│      ▼         ▼                                                             │
│   Success   Failure                                                          │
│      │         │                                                             │
│      ▼         ▼                                                             │
│   ┌─────────┐  ┌─────────────────┐                                          │
│   │CONNECTED│  │ BACKOFF (1s,2s) │                                          │
│   └────┬────┘  └────────┬────────┘                                          │
│        │                │                                                    │
│        │                └──────► Retry connection                            │
│        │                                                                     │
│        │  Connection lost                                                    │
│        └──────────────────────────────────┐                                 │
│                                           │                                  │
│                                           ▼                                  │
│                                    ┌─────────────┐                          │
│                                    │RECONNECTING │                          │
│                                    └─────────────┘                          │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Request Forwarding

When a node receives a request it can't (or shouldn't) handle locally:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                      REQUEST FORWARDING                                      │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│   Client Request: Execute function "heavy_query"                             │
│                                                                              │
│   ┌─────────────┐                                                            │
│   │   Node 1    │ ◄─── Request arrives                                       │
│   │  (gateway)  │                                                            │
│   │   load: 95% │ ◄─── High load!                                           │
│   └──────┬──────┘                                                            │
│          │                                                                   │
│          │  Decision: Forward to less-loaded node                            │
│          │                                                                   │
│          │  Check peer loads:                                                │
│          │  - Node 2: 30%  ◄─── Best choice                                 │
│          │  - Node 3: 75%                                                    │
│          │                                                                   │
│          │  grpc: ExecuteFunction(heavy_query, args)                        │
│          ▼                                                                   │
│   ┌─────────────┐                                                            │
│   │   Node 2    │                                                            │
│   │  load: 30%  │                                                            │
│   └──────┬──────┘                                                            │
│          │                                                                   │
│          │  Execute function locally                                         │
│          │                                                                   │
│          │  Return result via gRPC                                           │
│          ▼                                                                   │
│   ┌─────────────┐                                                            │
│   │   Node 1    │                                                            │
│   │             │ ──► Return result to client                                │
│   └─────────────┘                                                            │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Forwarding Decision

```rust
fn should_forward_request(&self, request: &Request) -> Option<NodeId> {
    let self_load = self.get_current_load();
    
    // Don't forward if we're not too busy
    if self_load < 0.8 {
        return None;
    }
    
    // Find least-loaded peer with required capability
    let best_peer = self.peers.values()
        .filter(|p| p.has_role(Role::Function))
        .filter(|p| p.reported_load < self_load - 0.2)  // At least 20% less loaded
        .min_by(|a, b| a.reported_load.partial_cmp(&b.reported_load).unwrap());
    
    best_peer.map(|p| p.node_id)
}
```

---

## Change Propagation

When data changes, all nodes need to know:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                     CHANGE PROPAGATION                                       │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│   Mutation executes on Node 2                                                │
│                                                                              │
│   ┌─────────────┐                                                            │
│   │   Node 2    │                                                            │
│   │             │ ──► INSERT INTO projects...                                │
│   │             │ ──► COMMIT                                                 │
│   └──────┬──────┘                                                            │
│          │                                                                   │
│          │  PostgreSQL NOTIFY 'forge_changes'                                │
│          │                                                                   │
│          ▼                                                                   │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │                         PostgreSQL                                   │   │
│   │  NOTIFY payload: {"table": "projects", "op": "INSERT", "id": ...}   │   │
│   └─────────────────────────────────────────────────────────────────────┘   │
│          │                                                                   │
│          │  All nodes LISTEN on 'forge_changes'                             │
│          ▼                                                                   │
│   ┌─────────────┐    ┌─────────────┐    ┌─────────────┐                     │
│   │   Node 1    │    │   Node 2    │    │   Node 3    │                     │
│   │             │    │             │    │             │                     │
│   │ Check local │    │ Check local │    │ Check local │                     │
│   │ subscribers │    │ subscribers │    │ subscribers │                     │
│   │             │    │             │    │             │                     │
│   │ Client A ✓  │    │             │    │ Client B ✓  │                     │
│   │ re-run query│    │             │    │ re-run query│                     │
│   │ send delta  │    │             │    │ send delta  │                     │
│   └─────────────┘    └─────────────┘    └─────────────┘                     │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Trace Context Propagation

Distributed tracing spans cross nodes:

```rust
// When forwarding a request
async fn forward_request(&self, peer: &PeerConnection, request: FunctionRequest) -> Result<FunctionResponse> {
    // Inject current trace context
    let mut request = request;
    request.trace_id = current_trace_id();
    request.span_id = current_span_id();
    
    // Create child span for the forward
    let span = tracing::span!(Level::INFO, "forward_to_peer", peer_id = %peer.node_id);
    let _guard = span.enter();
    
    // Make the call
    let response = peer.client.execute_function(request).await?;
    
    // Merge child spans from peer into our trace
    for child_span in response.child_spans {
        self.tracer.record_span(child_span);
    }
    
    Ok(response)
}
```

---

## Security

### mTLS Between Nodes

```toml
# forge.toml

[cluster.security]
# Enable mutual TLS for inter-node communication
mtls_enabled = true

# Certificate paths (auto-generated if not specified)
cert_file = "/etc/forge/certs/node.crt"
key_file = "/etc/forge/certs/node.key"
ca_file = "/etc/forge/certs/ca.crt"

# Auto-rotate certificates
auto_rotate = true
rotate_before_expiry = "7d"
```

### Authentication

```rust
// Nodes authenticate to each other using cluster secret
impl ForgeInternalService {
    async fn authenticate(&self, request: &Request) -> Result<()> {
        let token = request.metadata().get("x-forge-cluster-token")
            .ok_or(Error::Unauthenticated)?;
        
        if token != self.cluster_secret {
            return Err(Error::Unauthenticated);
        }
        
        Ok(())
    }
}
```

---

## Performance

### Connection Pooling

```toml
# forge.toml

[cluster.mesh]
# Connections per peer
connections_per_peer = 10

# Keep-alive settings
keepalive_interval = "10s"
keepalive_timeout = "20s"

# Request timeout
request_timeout = "30s"
```

### Load Reporting

Nodes periodically share load information:

```rust
struct LoadReport {
    cpu_usage: f32,
    memory_usage: f32,
    active_connections: u32,
    active_jobs: u32,
    queue_depth: u32,
}

// Shared via gossip every second
```

---

## Monitoring

### Metrics

| Metric | Description |
|--------|-------------|
| `forge_mesh_peers_connected` | Currently connected peers |
| `forge_mesh_requests_forwarded_total` | Requests forwarded to peers |
| `forge_mesh_rpc_duration_seconds` | gRPC call latency |
| `forge_mesh_rpc_errors_total` | gRPC errors by type |

### Dashboard

The dashboard shows:
- Mesh topology visualization
- Connection health
- Request forwarding patterns
- Latency between nodes

---

## Related Documentation

- [Clustering](CLUSTERING.md) — Cluster overview
- [Discovery](DISCOVERY.md) — How nodes find each other
- [Tracing](../observability/TRACING.md) — Distributed tracing

# Node Discovery

> *How nodes find each other*

---

## Overview

When a FORGE node starts, it needs to find other nodes in the cluster. FORGE supports multiple discovery mechanisms for different deployment environments.

---

## Discovery Methods

### 1. PostgreSQL Discovery (Default)

The simplest and most reliable method—PostgreSQL is always available.

```toml
# forge.toml
[cluster]
discovery = "postgres"
```

**How it works:**

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                    POSTGRESQL DISCOVERY                                      │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│   New Node Starting                                                          │
│        │                                                                     │
│        │  1. Connect to PostgreSQL                                           │
│        ▼                                                                     │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │  SELECT * FROM forge_nodes WHERE status = 'active'                   │   │
│   └─────────────────────────────────────────────────────────────────────┘   │
│        │                                                                     │
│        │  Returns: [{id: "abc", ip: "10.0.0.1", grpc_port: 9000}, ...]      │
│        ▼                                                                     │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │  2. Register self in forge_nodes                                     │   │
│   │  INSERT INTO forge_nodes (id, ip, grpc_port, ...)                    │   │
│   └─────────────────────────────────────────────────────────────────────┘   │
│        │                                                                     │
│        │  3. Connect to discovered peers via gRPC                           │
│        ▼                                                                     │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │  grpc::connect("10.0.0.1:9000")                                      │   │
│   │  grpc::connect("10.0.0.2:9000")                                      │   │
│   │  ...                                                                 │   │
│   └─────────────────────────────────────────────────────────────────────┘   │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Advantages:**
- No additional infrastructure
- Works everywhere
- Always consistent (single source of truth)

**Disadvantages:**
- Slightly slower initial discovery (database query)
- Requires PostgreSQL connectivity

---

### 2. DNS Discovery

Best for Docker Compose and simple deployments.

```toml
# forge.toml
[cluster]
discovery = "dns"
dns_name = "forge"  # Service name
dns_port = 9000     # gRPC port
```

**How it works:**

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                      DNS DISCOVERY                                           │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│   docker-compose.yml:                                                        │
│   services:                                                                  │
│     forge:                                                                   │
│       image: forge                                                           │
│       deploy:                                                                │
│         replicas: 3                                                          │
│                                                                              │
│   Docker creates DNS entries:                                                │
│   forge → 10.0.0.2, 10.0.0.3, 10.0.0.4                                      │
│                                                                              │
│   ─────────────────────────────────────────────────────────────────────────  │
│                                                                              │
│   New Node Starting                                                          │
│        │                                                                     │
│        │  1. DNS lookup                                                      │
│        ▼                                                                     │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │  dns::lookup("forge") → [10.0.0.2, 10.0.0.3, 10.0.0.4]              │   │
│   └─────────────────────────────────────────────────────────────────────┘   │
│        │                                                                     │
│        │  2. Connect to each IP on gRPC port                                │
│        ▼                                                                     │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │  grpc::connect("10.0.0.2:9000")                                      │   │
│   │  grpc::connect("10.0.0.3:9000")                                      │   │
│   │  grpc::connect("10.0.0.4:9000")                                      │   │
│   └─────────────────────────────────────────────────────────────────────┘   │
│        │                                                                     │
│        │  3. Register in PostgreSQL (still required for state)              │
│        ▼                                                                     │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Advantages:**
- Fast discovery (local DNS)
- Works well with Docker Compose
- No database query for initial peer list

**Disadvantages:**
- Requires DNS service (Docker, Kubernetes, etc.)
- DNS TTL can cause stale entries

---

### 3. Kubernetes Discovery

Native integration with Kubernetes API.

```toml
# forge.toml
[cluster]
discovery = "kubernetes"
kubernetes_namespace = "default"
kubernetes_service = "forge-mesh"  # Headless service
```

**How it works:**

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                   KUBERNETES DISCOVERY                                       │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│   Kubernetes Resources:                                                      │
│                                                                              │
│   apiVersion: v1                                                             │
│   kind: Service                                                              │
│   metadata:                                                                  │
│     name: forge-mesh                                                         │
│   spec:                                                                      │
│     clusterIP: None  # Headless service                                      │
│     selector:                                                                │
│       app: forge                                                             │
│                                                                              │
│   ─────────────────────────────────────────────────────────────────────────  │
│                                                                              │
│   New Node Starting                                                          │
│        │                                                                     │
│        │  1. Query Kubernetes API                                            │
│        ▼                                                                     │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │  GET /api/v1/namespaces/default/endpoints/forge-mesh                 │   │
│   │                                                                      │   │
│   │  Response:                                                           │   │
│   │  {                                                                   │   │
│   │    "subsets": [{                                                     │   │
│   │      "addresses": [                                                  │   │
│   │        {"ip": "10.0.0.5", "hostname": "forge-0"},                   │   │
│   │        {"ip": "10.0.0.6", "hostname": "forge-1"},                   │   │
│   │        {"ip": "10.0.0.7", "hostname": "forge-2"}                    │   │
│   │      ],                                                              │   │
│   │      "ports": [{"port": 9000, "name": "grpc"}]                      │   │
│   │    }]                                                                │   │
│   │  }                                                                   │   │
│   └─────────────────────────────────────────────────────────────────────┘   │
│        │                                                                     │
│        │  2. Watch for changes                                               │
│        ▼                                                                     │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │  GET /api/v1/watch/namespaces/default/endpoints/forge-mesh           │   │
│   │  (streaming updates when pods start/stop)                            │   │
│   └─────────────────────────────────────────────────────────────────────┘   │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Advantages:**
- Real-time pod updates
- Native Kubernetes integration
- No DNS TTL issues

**Disadvantages:**
- Requires Kubernetes RBAC permissions
- Only works in Kubernetes

---

### 4. Static Discovery

For fixed infrastructure or testing.

```toml
# forge.toml
[cluster]
discovery = "static"
static_seeds = [
    "10.0.0.1:9000",
    "10.0.0.2:9000",
    "10.0.0.3:9000",
]
```

**How it works:**

```
New Node Starting
     │
     │  1. Try each seed in order
     ▼
┌─────────────────────────────────────────────────────────────────────────────┐
│  grpc::connect("10.0.0.1:9000") → Success! Get peer list from this node     │
│  grpc::connect("10.0.0.2:9000") → (skip, already found peers)               │
└─────────────────────────────────────────────────────────────────────────────┘
     │
     │  2. Connect to all peers in returned list
     ▼
```

**Advantages:**
- No external dependencies
- Predictable behavior
- Good for testing

**Disadvantages:**
- Manual configuration
- Doesn't handle dynamic scaling

---

## Hybrid Discovery

Combine methods for resilience:

```toml
# forge.toml
[cluster]
discovery = ["kubernetes", "postgres"]
# Try Kubernetes first, fall back to PostgreSQL
```

---

## Discovery Events

```rust
// Listen for cluster membership changes
impl ClusterEventHandler for MyHandler {
    async fn on_node_joined(&self, node: &NodeInfo) {
        println!("Node joined: {} at {}", node.id, node.address);
    }
    
    async fn on_node_left(&self, node: &NodeInfo) {
        println!("Node left: {}", node.id);
    }
    
    async fn on_node_updated(&self, node: &NodeInfo) {
        println!("Node updated: {} load={}%", node.id, node.load);
    }
}
```

---

## Troubleshooting

### Node Not Discovering Peers

```bash
# Check DNS resolution
forge debug dns forge

# Check PostgreSQL node registry
forge debug nodes

# Check Kubernetes endpoints
forge debug k8s-endpoints
```

### Common Issues

| Issue | Cause | Solution |
|-------|-------|----------|
| "No peers found" | First node, or discovery misconfigured | Check discovery config, verify network |
| "Connection refused" | Firewall blocking gRPC port | Open port 9000 between nodes |
| "DNS resolution failed" | DNS not configured | Check Docker/K8s service name |
| "Kubernetes API error" | Missing RBAC | Add endpoints read permission |

---

## Related Documentation

- [Clustering](CLUSTERING.md) — Cluster overview
- [Meshing](MESHING.md) — Inter-node communication
- [Kubernetes](../deployment/KUBERNETES.md) — K8s deployment
- [Docker](../deployment/DOCKER.md) — Docker deployment

# Capacity Planning

> *Right-size your deployment*

---

## Overview

This guide provides memory, CPU, and connection requirements for FORGE deployments. Use these guidelines to plan your infrastructure from development through production.

**Target scale:** FORGE is designed for typical SaaS workloads—up to 100K concurrent users comfortably, scalable to 1M with proper configuration.

---

## Quick Reference

### Per-Node Resource Requirements

| Component | Memory (base) | Memory (per unit) | CPU | Notes |
|-----------|---------------|-------------------|-----|-------|
| Gateway | 50 MB | +2 KB/connection | 0.1 core | WebSocket handling |
| Function executor | 100 MB | +1 MB/concurrent request | 0.2 core | Depends on your code |
| Worker | 50 MB | +5 MB/concurrent job | 0.5 core | Job processing |
| Subscription manager | 50 MB | +4 KB/subscription | 0.1 core | Read-set tracking |
| Scheduler (leader only) | 20 MB | - | 0.05 core | Cron scheduling |
| Base runtime | 100 MB | - | 0.1 core | Tokio, logging, etc. |

### PostgreSQL Requirements

| Concurrent users | Connections needed | Recommended RAM | Storage |
|-----------------|-------------------|-----------------|---------|
| 1K | 50-100 | 4 GB | 20 GB |
| 10K | 100-200 | 8 GB | 50 GB |
| 100K | 200-500 | 32 GB | 200 GB |
| 1M | 500-1000 (with PgBouncer) | 128 GB | 1 TB+ |

---

## Memory Breakdown

### WebSocket Connections

Each WebSocket connection consumes memory for:

```
Per connection:
├── TCP socket buffer:     ~8 KB (configurable)
├── TLS state (if HTTPS):  ~20 KB
├── Session state:         ~1 KB
├── Send/receive buffers:  ~4 KB
└── Total:                 ~10-35 KB per connection
```

**Example:** 10,000 concurrent connections ≈ 100-350 MB

### Subscriptions

Each active subscription tracks:

```
Per subscription:
├── Query definition:      ~200 bytes
├── Query arguments:       ~100 bytes (varies)
├── Read-set (table-level): ~50 bytes
├── Read-set (row-level):   ~100 bytes per tracked row
├── Last result hash:      ~32 bytes
└── Total (table-level):   ~400 bytes
└── Total (row-level):     ~400 bytes + (100 × rows tracked)
```

**Memory by tracking mode:**

| Mode | Memory per 1K subs | Best for |
|------|-------------------|----------|
| Table-level | ~400 KB | Simple queries, high fan-out |
| Row-level (10 rows) | ~1.4 MB | Complex queries, targeted updates |
| Row-level (100 rows) | ~10 MB | Wide result sets |

### Jobs

Each in-flight job consumes:

```
Per job:
├── Job metadata:          ~500 bytes
├── Input (serialized):    varies (typically 1-10 KB)
├── Execution context:     ~2 KB
└── Total:                 ~3-15 KB per job
```

**Example:** 100 concurrent jobs with 5KB inputs ≈ 0.8 MB

### Workflows

Workflow state in memory (active workflows):

```
Per workflow:
├── Workflow metadata:     ~1 KB
├── Step history:          ~500 bytes per step
├── Current input/output:  varies
└── Total:                 ~2-20 KB per workflow
```

---

## CPU Requirements

### Request Processing

| Operation | Typical CPU time | Notes |
|-----------|-----------------|-------|
| HTTP request parse | 5-20 μs | Depends on headers |
| Auth token validation | 10-50 μs | JWT verification |
| Simple query | 100-500 μs | Mostly DB wait time |
| Simple mutation | 200-1000 μs | DB write + notification |
| Subscription update check | 50-200 μs | Read-set comparison |

**Throughput estimate:** A single core can handle ~2,000-5,000 simple requests/second.

### Background Processing

| Operation | Typical CPU time | Notes |
|-----------|-----------------|-------|
| Job claim | 100-300 μs | DB round-trip |
| Simple job execution | 1-100 ms | Depends on job |
| Subscription invalidation | 10-50 μs per sub | Batch processing |
| Metrics aggregation | ~1 ms per batch | Background task |

---

## Sizing Guidelines

### Development (Local)

```
FORGE Node: 1
├── Memory: 256 MB
├── CPU: 1 core
└── Handles: ~100 concurrent users

PostgreSQL:
├── Memory: 1 GB
├── Storage: 10 GB
└── Connections: 20
```

### Small Production (1K-10K users)

```
FORGE Nodes: 2-3
├── Memory: 1 GB each
├── CPU: 2 cores each
└── Handles: 1K-5K concurrent users per node

PostgreSQL:
├── Memory: 8 GB
├── Storage: 50 GB
├── Connections: 100
└── Consider: Read replica for queries
```

### Medium Production (10K-100K users)

```
FORGE Nodes: 3-6
├── Memory: 2-4 GB each
├── CPU: 4 cores each
└── Handles: 10K-20K concurrent users per node

PostgreSQL:
├── Memory: 32 GB
├── Storage: 200 GB
├── Connections: 200 (via PgBouncer)
└── Required: Read replicas, connection pooling
```

### Large Production (100K-1M users)

```
FORGE Nodes: 6-20
├── Memory: 4-8 GB each
├── CPU: 8 cores each
└── Handles: 50K-100K concurrent users per node

PostgreSQL:
├── Memory: 128 GB
├── Storage: 1 TB+
├── Connections: 500+ (via PgBouncer)
└── Required: Read replicas, partitioning, possibly sharding
```

---

## Subscription Scaling

### Memory Pressure Management

Subscriptions are the primary memory consumer. Configure limits:

```toml
# forge.toml

[subscriptions]
# Maximum subscriptions per connection
max_per_connection = 50

# Maximum total subscriptions per node
max_per_node = 100000

# Maximum rows tracked per subscription (row-level mode)
max_tracked_rows = 100

# Memory limit for subscription data (triggers cleanup)
memory_limit = "500MB"

# When memory limit approached, switch to table-level tracking
degrade_to_table_level_at = "400MB"
```

### Subscription Limits by Deployment Size

| Deployment | Max subs/connection | Max subs/node | Tracking mode |
|------------|--------------------|--------------|--------------|
| Small | 50 | 10,000 | Row-level |
| Medium | 30 | 50,000 | Hybrid |
| Large | 20 | 100,000 | Table-level default |

### Monitoring Subscription Memory

```sql
-- Check subscription memory usage (via dashboard or SQL)
SELECT
    node_id,
    count(*) as subscription_count,
    pg_size_pretty(sum(memory_bytes)) as total_memory,
    avg(tracked_row_count) as avg_tracked_rows
FROM forge_subscription_stats
GROUP BY node_id;
```

---

## Connection Pool Sizing

### FORGE to PostgreSQL

```toml
# forge.toml

[database]
# Connections for user requests
pool_size = 50

# Separate pool for background jobs (recommended)
[jobs]
pool_size = 20

# Separate pool for observability (recommended)
[observability]
pool_size = 10
```

**Formula:**
```
connections_per_node = user_pool + job_pool + observability_pool + buffer
                     = 50 + 20 + 10 + 5
                     = 85 connections per node

total_pg_connections = connections_per_node × node_count
                     = 85 × 3 nodes
                     = 255 connections
```

### Using PgBouncer

For larger deployments, use PgBouncer to multiplex connections:

```
┌─────────────┐      ┌──────────────┐      ┌─────────────┐
│  FORGE (1)  │──┐   │              │      │             │
├─────────────┤  │   │   PgBouncer  │──────│  PostgreSQL │
│  FORGE (2)  │──┼──►│  (100 → 50)  │      │  (50 conns) │
├─────────────┤  │   │              │      │             │
│  FORGE (3)  │──┘   └──────────────┘      └─────────────┘
```

```ini
# pgbouncer.ini
[databases]
forge = host=postgres port=5432 dbname=forge

[pgbouncer]
pool_mode = transaction
max_client_conn = 500
default_pool_size = 50
min_pool_size = 10
```

---

## Job Queue Capacity

### Queue Depth Limits

```toml
# forge.toml

[jobs]
# Maximum pending jobs before backpressure
max_pending = 100000

# Maximum jobs per worker concurrently
max_concurrent_per_worker = 50

# When queue exceeds this, new dispatches are rejected
backpressure_threshold = 80000
```

### Job Processing Capacity

| Workers | Concurrent jobs/worker | Job duration (avg) | Throughput |
|---------|------------------------|-------------------|------------|
| 3 | 10 | 100ms | 300/sec |
| 3 | 50 | 100ms | 1,500/sec |
| 10 | 50 | 100ms | 5,000/sec |
| 10 | 100 | 50ms | 20,000/sec |

---

## Horizontal Scaling

### When to Add Nodes

| Signal | Threshold | Action |
|--------|-----------|--------|
| CPU usage | > 70% sustained | Add nodes |
| Memory usage | > 80% | Add nodes or reduce subscriptions |
| Request latency P99 | > 500ms | Add nodes |
| Job queue depth | > 10K sustained | Add worker nodes |
| WebSocket connections | > 50K/node | Add gateway nodes |

### Role-Specialized Scaling

For large deployments, specialize nodes:

```toml
# Gateway nodes (WebSocket heavy)
# forge.gateway.toml
[roles]
gateway = true
function = true
worker = false
scheduler = false

# Worker nodes (job processing)
# forge.worker.toml
[roles]
gateway = false
function = false
worker = true
scheduler = true  # One will become leader
```

---

## PostgreSQL Tuning

### Key Parameters

```sql
-- Memory
shared_buffers = '8GB'              -- 25% of RAM
effective_cache_size = '24GB'       -- 75% of RAM
work_mem = '256MB'                  -- For complex queries
maintenance_work_mem = '2GB'        -- For vacuum, index builds

-- Connections
max_connections = 200               -- Limit, use PgBouncer for more

-- WAL
wal_buffers = '64MB'
checkpoint_completion_target = 0.9
max_wal_size = '4GB'

-- Parallel queries
max_parallel_workers_per_gather = 4
max_parallel_workers = 8
```

### Indexing for Scale

```sql
-- Essential indexes (created by FORGE)
CREATE INDEX CONCURRENTLY idx_jobs_claimable
    ON forge_jobs(priority DESC, scheduled_at)
    WHERE status = 'pending';

CREATE INDEX CONCURRENTLY idx_subscriptions_tables
    ON forge_subscriptions USING GIN(read_tables);

-- Consider partial indexes for your data
CREATE INDEX CONCURRENTLY idx_projects_active
    ON projects(owner_id, created_at DESC)
    WHERE status = 'active';
```

---

## Monitoring Capacity

### Key Metrics to Watch

```toml
# forge.toml

[[alerts]]
name = "high_memory"
condition = "forge_node_memory_usage_bytes > 0.85 * forge_node_memory_limit_bytes"
severity = "warning"

[[alerts]]
name = "subscription_memory_pressure"
condition = "forge_subscription_memory_bytes > 400000000"  # 400MB
severity = "warning"

[[alerts]]
name = "high_connection_count"
condition = "forge_websocket_connections > 40000"
severity = "warning"

[[alerts]]
name = "job_queue_backing_up"
condition = "forge_jobs_pending > 50000"
for = "5m"
severity = "warning"
```

### Dashboard Queries

```sql
-- Current capacity usage
SELECT
    'Connections' as metric,
    count(*) as current,
    100000 as limit,
    round(count(*) * 100.0 / 100000, 1) as pct
FROM forge_sessions
UNION ALL
SELECT
    'Subscriptions',
    count(*),
    500000,
    round(count(*) * 100.0 / 500000, 1)
FROM forge_subscriptions
UNION ALL
SELECT
    'Pending Jobs',
    count(*),
    100000,
    round(count(*) * 100.0 / 100000, 1)
FROM forge_jobs WHERE status = 'pending';
```

---

## Cost Estimation

### Cloud Instance Recommendations

| Scale | AWS | GCP | Monthly cost* |
|-------|-----|-----|---------------|
| Dev | t3.small | e2-small | ~$15/mo |
| Small (1K users) | 2× t3.medium + RDS db.t3.medium | 2× e2-medium + Cloud SQL | ~$200/mo |
| Medium (10K users) | 3× t3.large + RDS db.r6g.large | 3× e2-standard-4 + Cloud SQL | ~$600/mo |
| Large (100K users) | 6× c6i.xlarge + RDS db.r6g.xlarge | 6× c3-standard-8 + Cloud SQL | ~$2,500/mo |

*Estimates, actual costs vary by region and usage.

---

## Related Documentation

- [Clustering](../cluster/CLUSTERING.md) — Multi-node setup
- [Observability](../observability/OBSERVABILITY.md) — Monitoring
- [PostgreSQL Schema](../database/POSTGRES_SCHEMA.md) — Database design

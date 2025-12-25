# Resilience & Error Recovery

> *What happens when things go wrong*

---

## Philosophy

FORGE takes a pragmatic approach to failure:

1. **Fail fast** — Don't hide errors, surface them quickly
2. **Degrade gracefully** — Some functionality beats no functionality
3. **Recover automatically** — When possible, without manual intervention
4. **Be honest** — Tell users when something is wrong

---

## PostgreSQL Unavailable

The most critical failure mode. PostgreSQL is the source of truth for everything.

### Detection

```rust
// FORGE maintains a health check connection
// Checked every 5 seconds (configurable)

[database]
health_check_interval = "5s"
health_check_timeout = "2s"
```

### Behavior During Outage

| Component | Behavior |
|-----------|----------|
| **Queries** | Return error immediately (no stale data) |
| **Mutations** | Return error immediately |
| **Subscriptions** | Mark as `stale`, keep last-known data visible |
| **Jobs** | Queue in memory (bounded), retry when DB returns |
| **Crons** | Skip execution, log missed run |
| **Gateway** | Health endpoint returns 503 |

### Memory Buffer for Jobs

During brief outages (<30s), jobs are buffered in memory:

```toml
# forge.toml

[worker.resilience]
# Buffer jobs in memory when DB is down
memory_buffer_enabled = true
memory_buffer_max_jobs = 1000
memory_buffer_max_age = "30s"

# After buffer is full or aged out, reject new jobs
buffer_full_behavior = "reject"  # or "drop_oldest"
```

**What this means:**

```
DB goes down at T+0
  │
  ├─► Jobs dispatched T+0 to T+30: Buffered in memory
  │
  ├─► Buffer fills at T+15 (1000 jobs): New jobs rejected with error
  │
  ├─► DB returns at T+25: Buffered jobs flushed to DB
  │
  └─► All buffered jobs execute normally
```

**What this does NOT mean:**

- Jobs are NOT persisted to disk (memory only)
- If the node crashes during outage, buffered jobs are lost
- This is for brief blips, not extended outages

### Client Behavior

The frontend client handles DB outages gracefully:

```svelte
<script>
  import { subscribe } from '$lib/forge';

  const projects = subscribe(get_projects, { userId });
</script>

{#if $projects.stale}
  <Banner type="warning">
    Connection issues. Showing cached data.
  </Banner>
{/if}

{#if $projects.error?.code === 'DATABASE_UNAVAILABLE'}
  <Banner type="error">
    Database is temporarily unavailable. Please try again.
  </Banner>
{/if}

{#each $projects.data ?? [] as project}
  <ProjectCard {project} />
{/each}
```

### Recovery

When PostgreSQL returns:

1. Health check succeeds
2. Connection pool refills
3. Buffered jobs flush to DB
4. Subscriptions re-sync (query current state, send deltas)
5. Health endpoint returns 200

No manual intervention needed.

---

## Node Failure

### Single Node

If you're running a single node and it crashes:

- **Downtime** until restart
- **No data loss** (PostgreSQL has the data)
- **Jobs resume** from last checkpoint when node restarts
- **Workflows resume** from last completed step

### Multi-Node Cluster

If one node in a cluster fails:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                         NODE FAILURE RECOVERY                                │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  Normal operation:                                                           │
│  ┌─────────┐    ┌─────────┐    ┌─────────┐                                  │
│  │ Node 1  │    │ Node 2  │    │ Node 3  │                                  │
│  │ Gateway │    │ Gateway │    │ Gateway │                                  │
│  │ Worker  │    │ Worker  │    │ Leader  │                                  │
│  └────┬────┘    └────┬────┘    └────┬────┘                                  │
│       └──────────────┴──────────────┘                                       │
│                      │                                                       │
│               ┌──────┴──────┐                                               │
│               │  PostgreSQL │                                               │
│               └─────────────┘                                               │
│                                                                              │
│  Node 2 crashes:                                                             │
│  ┌─────────┐         ✗          ┌─────────┐                                  │
│  │ Node 1  │                    │ Node 3  │                                  │
│  │ Gateway │◄───────────────────│ Gateway │                                  │
│  │ Worker  │  Takes over        │ Leader  │                                  │
│  └─────────┘  Node 2's jobs     └─────────┘                                  │
│                                                                              │
│  What happens:                                                               │
│  - Load balancer stops sending traffic to Node 2                            │
│  - Node 2's in-progress jobs timeout, become available for other workers    │
│  - No data loss (jobs are in PostgreSQL)                                    │
│  - Subscriptions on Node 2 disconnect, clients reconnect to Node 1 or 3    │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Job Handoff

Jobs claimed by a failed node are automatically reclaimed:

```sql
-- Jobs have a heartbeat timestamp
-- If heartbeat is stale, job becomes available

UPDATE forge_jobs
SET status = 'pending', claimed_by = NULL
WHERE status = 'processing'
  AND heartbeat_at < NOW() - INTERVAL '30 seconds';
```

Workers heartbeat every 10 seconds. After 30 seconds without heartbeat, jobs are released.

---

## Network Partitions

### Between Nodes

If nodes can't talk to each other but can reach PostgreSQL:

- **Leader election** continues to work (PostgreSQL is the arbiter)
- **Job distribution** works (via PostgreSQL)
- **Subscriptions** may see delayed updates (gRPC fallback to polling)

```toml
# forge.toml

[cluster]
# If gRPC to peer fails, poll PostgreSQL for changes
grpc_fallback_to_polling = true
polling_interval = "1s"
```

### Between Client and Server

- **WebSocket reconnects** automatically with exponential backoff
- **Subscriptions** marked `stale` until reconnected
- **Mutations** fail immediately (client should retry or show error)

---

## Graceful Shutdown

When a node receives SIGTERM:

```
SIGTERM received
    │
    ├─► Stop accepting new connections
    │
    ├─► Stop claiming new jobs
    │
    ├─► Wait for in-flight requests (up to 30s)
    │
    ├─► Wait for in-progress jobs (up to 60s)
    │       └─► Jobs not finished are released back to queue
    │
    ├─► Close WebSocket connections (clients will reconnect)
    │
    └─► Exit
```

```toml
# forge.toml

[shutdown]
request_drain_timeout = "30s"
job_drain_timeout = "60s"
force_after = "90s"
```

---

## Circuit Breakers

For external service calls, use circuit breakers to prevent cascade failures:

```rust
#[forge::action]
pub async fn sync_with_stripe(ctx: &ActionContext, user_id: Uuid) -> Result<()> {
    // Circuit breaker wraps external calls
    ctx.external("stripe")
        .timeout(Duration::seconds(10))
        .retries(3)
        .circuit_breaker(CircuitBreaker {
            failure_threshold: 5,      // Open after 5 failures
            reset_timeout: Duration::seconds(30),  // Try again after 30s
        })
        .call(|| stripe::Customer::retrieve(&user_id))
        .await?;

    Ok(())
}
```

Circuit states visible in dashboard: **Metrics** → **External Services**.

---

## Error Visibility

All errors are logged and visible in the dashboard:

### Dashboard Error Views

| View | Shows |
|------|-------|
| **Logs** | All errors with full stack traces |
| **Jobs** | Failed jobs with error messages, one-click retry |
| **Workflows** | Failed steps with error context |
| **Metrics** | Error rate graphs, alerts |

### Alerting

Configure alerts in `forge.toml`:

```toml
[observability.alerts]
# Alert if error rate exceeds threshold
error_rate_threshold = 0.05  # 5%
error_rate_window = "5m"

# Alert if job queue backs up
job_queue_threshold = 1000
job_queue_window = "10m"

# Where to send alerts
[observability.alerts.destinations]
webhook = "https://your-alerting-service.com/webhook"
```

---

## Testing Resilience

### Chaos Testing

Inject failures during testing:

```rust
#[tokio::test]
async fn test_database_outage_recovery() {
    let ctx = TestContext::new().await;

    // Simulate DB outage
    ctx.simulate_db_outage(Duration::seconds(5)).await;

    // Dispatch job during outage
    let job = ctx.dispatch(my_job, input).await;

    // Job should be buffered
    assert!(job.is_ok());

    // Wait for recovery
    tokio::time::sleep(Duration::seconds(6)).await;

    // Job should have executed
    assert!(ctx.job_completed(job.unwrap()).await);
}
```

### Manual Testing

Via dashboard:

- **Jobs** → **Simulate Failure** — Mark a job as failed
- **Workflows** → **Fail Step** — Inject step failure
- **Cluster** → **Disconnect Node** — Simulate node failure (dev only)

---

## What FORGE Does NOT Handle

Be realistic about failure modes FORGE can't solve:

| Scenario | What Happens | Your Responsibility |
|----------|--------------|---------------------|
| PostgreSQL data loss | Unrecoverable | Backups, replication |
| All nodes down | Complete outage | Multi-region, monitoring |
| Slow queries blocking pool | Degraded performance | Query optimization, pool isolation |
| Memory exhaustion | OOM kill | Resource limits, monitoring |
| Disk full | Writes fail | Monitoring, cleanup jobs |

---

## Configuration Summary

```toml
# forge.toml - resilience settings

[database]
health_check_interval = "5s"
health_check_timeout = "2s"
pool_size = 50
pool_timeout = "30s"

[worker.resilience]
memory_buffer_enabled = true
memory_buffer_max_jobs = 1000
memory_buffer_max_age = "30s"
job_heartbeat_interval = "10s"
job_stale_threshold = "30s"

[cluster]
grpc_fallback_to_polling = true
polling_interval = "1s"

[shutdown]
request_drain_timeout = "30s"
job_drain_timeout = "60s"
force_after = "90s"

[observability.alerts]
error_rate_threshold = 0.05
error_rate_window = "5m"
```

---

## Related Documentation

- [Clustering](../cluster/CLUSTERING.md) — Multi-node setup
- [Jobs](../core/JOBS.md) — Job queue details
- [Observability](../observability/OBSERVABILITY.md) — Monitoring

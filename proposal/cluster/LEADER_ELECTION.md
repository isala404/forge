# Leader Election

> *Exactly one scheduler, always*

---

## Overview

Some operations in FORGE require a **single coordinator**:

- Cron job triggering
- Job assignment to workers
- Dead letter queue processing
- Cluster-wide aggregations

FORGE uses **PostgreSQL advisory locks** for leader election—simple, reliable, and requires no additional infrastructure.

---

## How It Works

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                     LEADER ELECTION                                          │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│   Three nodes, all with scheduler role enabled:                              │
│                                                                              │
│   ┌─────────────┐    ┌─────────────┐    ┌─────────────┐                     │
│   │   Node 1    │    │   Node 2    │    │   Node 3    │                     │
│   │             │    │             │    │             │                     │
│   │  Try lock   │    │  Try lock   │    │  Try lock   │                     │
│   │     ↓       │    │     ↓       │    │     ↓       │                     │
│   └──────┬──────┘    └──────┬──────┘    └──────┬──────┘                     │
│          │                  │                  │                             │
│          │  SELECT pg_try_advisory_lock(12345)                              │
│          │                  │                  │                             │
│          ▼                  ▼                  ▼                             │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │                         PostgreSQL                                   │   │
│   │                                                                      │   │
│   │   Advisory Lock 12345:                                               │   │
│   │   - Node 1: TRUE (acquired) ◄── LEADER                              │   │
│   │   - Node 2: FALSE (not acquired)                                     │   │
│   │   - Node 3: FALSE (not acquired)                                     │   │
│   └─────────────────────────────────────────────────────────────────────┘   │
│          │                  │                  │                             │
│          ▼                  ▼                  ▼                             │
│   ┌─────────────┐    ┌─────────────┐    ┌─────────────┐                     │
│   │   Node 1    │    │   Node 2    │    │   Node 3    │                     │
│   │  ★ LEADER ★ │    │   standby   │    │   standby   │                     │
│   │             │    │  (waiting)  │    │  (waiting)  │                     │
│   │ - Run crons │    │             │    │             │                     │
│   │ - Assign    │    │             │    │             │                     │
│   │   jobs      │    │             │    │             │
│   └─────────────┘    └─────────────┘    └─────────────┘                     │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Implementation

### Acquiring Leadership

```rust
impl LeaderElection {
    const SCHEDULER_LOCK_ID: i64 = 0xFORGE_SCHEDULER;  // Unique lock ID
    
    async fn try_become_leader(&self) -> Result<bool> {
        // Try to acquire advisory lock (non-blocking)
        let result: (bool,) = sqlx::query_as(
            "SELECT pg_try_advisory_lock($1)"
        )
        .bind(Self::SCHEDULER_LOCK_ID)
        .fetch_one(&self.db)
        .await?;
        
        let acquired = result.0;
        
        if acquired {
            // Record leadership in database for visibility
            sqlx::query(
                "INSERT INTO forge_leaders (role, node_id, acquired_at, lease_until)
                 VALUES ('scheduler', $1, NOW(), NOW() + INTERVAL '1 minute')
                 ON CONFLICT (role) DO UPDATE SET
                   node_id = $1,
                   acquired_at = NOW(),
                   lease_until = NOW() + INTERVAL '1 minute'"
            )
            .bind(&self.node_id)
            .execute(&self.db)
            .await?;
            
            info!("Became scheduler leader");
        }
        
        Ok(acquired)
    }
    
    async fn maintain_leadership(&self) {
        // Refresh lease periodically
        loop {
            sqlx::query(
                "UPDATE forge_leaders 
                 SET lease_until = NOW() + INTERVAL '1 minute'
                 WHERE role = 'scheduler' AND node_id = $1"
            )
            .bind(&self.node_id)
            .execute(&self.db)
            .await?;
            
            tokio::time::sleep(Duration::seconds(30)).await;
        }
    }
}
```

### Standby Behavior

```rust
impl LeaderElection {
    async fn standby_loop(&self) {
        loop {
            // Wait before trying again
            tokio::time::sleep(Duration::seconds(5)).await;
            
            // Check if current leader is healthy
            let leader_healthy = self.check_leader_health().await;
            
            if !leader_healthy {
                // Try to become leader
                if self.try_become_leader().await? {
                    // We're the leader now
                    self.run_as_leader().await;
                }
            }
        }
    }
    
    async fn check_leader_health(&self) -> bool {
        let result: Option<(Timestamp,)> = sqlx::query_as(
            "SELECT lease_until FROM forge_leaders WHERE role = 'scheduler'"
        )
        .fetch_optional(&self.db)
        .await?;
        
        match result {
            Some((lease_until,)) => lease_until > Timestamp::now(),
            None => false,  // No leader, we should try
        }
    }
}
```

---

## Failure Handling

### Leader Crash

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                     LEADER FAILURE                                           │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│   Time 0:00 - Normal operation                                               │
│   ┌─────────────┐    ┌─────────────┐                                        │
│   │   Node 1    │    │   Node 2    │                                        │
│   │  ★ LEADER ★ │    │   standby   │                                        │
│   └─────────────┘    └─────────────┘                                        │
│                                                                              │
│   Time 0:15 - Node 1 crashes                                                 │
│   ┌─────────────┐    ┌─────────────┐                                        │
│   │   Node 1    │    │   Node 2    │                                        │
│   │   [DEAD]    │    │   standby   │                                        │
│   └─────────────┘    └─────────────┘                                        │
│                                                                              │
│   PostgreSQL automatically releases advisory lock when connection closes     │
│                                                                              │
│   Time 0:20 - Node 2 notices (lease expired OR lock available)               │
│   ┌─────────────┐    ┌─────────────┐                                        │
│   │   Node 1    │    │   Node 2    │                                        │
│   │   [DEAD]    │    │  tries lock │                                        │
│   └─────────────┘    │  SUCCESS!   │                                        │
│                      └─────────────┘                                        │
│                                                                              │
│   Time 0:21 - Node 2 is leader                                               │
│   ┌─────────────┐    ┌─────────────┐                                        │
│   │   Node 1    │    │   Node 2    │                                        │
│   │   [DEAD]    │    │  ★ LEADER ★ │                                        │
│   └─────────────┘    └─────────────┘                                        │
│                                                                              │
│   Failover time: ~5-20 seconds                                               │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Network Partition

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                     NETWORK PARTITION                                        │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│   Partition A              │            Partition B                          │
│   ┌─────────────┐         ✗│            ┌─────────────┐                     │
│   │   Node 1    │  network │            │   Node 2    │                     │
│   │  ★ LEADER ★ │   split  │            │   standby   │                     │
│   │             │          │            │             │                     │
│   │  Can reach  │          │            │ Cannot reach│                     │
│   │  PostgreSQL │          │            │ PostgreSQL  │                     │
│   └─────────────┘          │            └─────────────┘                     │
│                            │                                                 │
│   Result:                                                                    │
│   - Node 1 keeps leading (has DB connection, holds lock)                     │
│   - Node 2 cannot try election (no DB connection)                            │
│   - NO SPLIT BRAIN (PostgreSQL is single source of truth)                    │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Graceful Leadership Transfer

When a leader is shutting down gracefully:

```rust
async fn graceful_leadership_transfer(&self) {
    // 1. Stop accepting new scheduler work
    self.stop_scheduling().await;
    
    // 2. Wait for in-flight work to complete
    self.wait_for_completion(Duration::seconds(10)).await;
    
    // 3. Release the advisory lock
    sqlx::query("SELECT pg_advisory_unlock($1)")
        .bind(Self::SCHEDULER_LOCK_ID)
        .execute(&self.db)
        .await?;
    
    // 4. Clear leadership record
    sqlx::query("DELETE FROM forge_leaders WHERE node_id = $1")
        .bind(&self.node_id)
        .execute(&self.db)
        .await?;
    
    info!("Released scheduler leadership");
    
    // Another node will acquire within seconds
}
```

---

## Multiple Leader Roles

FORGE can have multiple leader roles:

```rust
enum LeaderRole {
    Scheduler,          // Job assignment, crons
    MetricsAggregator,  // Aggregate metrics from all nodes
    LogCompactor,       // Compact old logs
}

// Each role has its own advisory lock ID
impl LeaderRole {
    fn lock_id(&self) -> i64 {
        match self {
            Self::Scheduler => 0x464F524745_0001,  // FORGE_0001
            Self::MetricsAggregator => 0x464F524745_0002,
            Self::LogCompactor => 0x464F524745_0003,
        }
    }
}
```

---

## Observability

### Metrics

| Metric | Description |
|--------|-------------|
| `forge_leader_is_leader` | 1 if this node is leader, 0 otherwise |
| `forge_leader_elections_total` | Total election attempts |
| `forge_leader_tenure_seconds` | How long current leader has held role |
| `forge_leader_failovers_total` | Number of failovers |

### Dashboard

The dashboard shows:
- Current leader for each role
- Leadership history
- Failover timeline
- Election events

### Alerts

```toml
# forge.toml

[[alerts]]
name = "no_scheduler_leader"
condition = "sum(forge_leader_is_leader{role='scheduler'}) == 0"
for = "30s"
severity = "critical"
notify = ["pagerduty"]

[[alerts]]
name = "frequent_failovers"
condition = "rate(forge_leader_failovers_total[5m]) > 5"
severity = "warning"
notify = ["slack:#ops"]
```

---

## Configuration

```toml
# forge.toml

[cluster.leader_election]
# How often standbys check leader health
check_interval = "5s"

# Lease duration (leader must refresh before expiry)
lease_duration = "60s"

# Lease refresh interval
refresh_interval = "30s"

# Timeout for acquiring lock
lock_timeout = "10s"
```

---

## Why PostgreSQL Advisory Locks?

| Alternative | Why Not |
|-------------|---------|
| **Raft/Paxos** | Complex, requires 3+ nodes, separate protocol |
| **etcd/Consul** | Additional infrastructure |
| **Redis SETNX** | Redis not required by FORGE |
| **File locks** | Doesn't work across network |
| **ZooKeeper** | Heavy, complex |

PostgreSQL advisory locks:
- Already available (PostgreSQL is required)
- ACID guarantees
- Automatic release on disconnect
- Simple API
- Battle-tested

---

## Related Documentation

- [Clustering](CLUSTERING.md) — Cluster overview
- [Crons](../core/CRONS.md) — Scheduled tasks (require leader)
- [Jobs](../core/JOBS.md) — Job assignment (require leader)

# Job Queue

> *PostgreSQL-powered reliable job processing*

---

## Overview

FORGE uses PostgreSQL as a job queue via the **SKIP LOCKED** pattern. This provides:

- **Exactly-once delivery** (within retry bounds)
- **Ordered processing** (by priority, then time)
- **Distributed workers** (multiple workers, no double-processing)
- **Durability** (jobs survive restarts)
- **No additional infrastructure** (just PostgreSQL)

---

## The SKIP LOCKED Pattern

### The Problem

Multiple workers need to claim jobs without conflicts:

```
Worker A: SELECT * FROM jobs WHERE status = 'pending' LIMIT 1;
Worker B: SELECT * FROM jobs WHERE status = 'pending' LIMIT 1;

-- Both get the SAME job!
-- Double-processing occurs
```

### The Solution: FOR UPDATE SKIP LOCKED

```sql
-- Worker A
SELECT * FROM jobs 
WHERE status = 'pending' 
ORDER BY priority DESC, created_at 
LIMIT 1 
FOR UPDATE SKIP LOCKED;  -- Locks the row, returns it

-- Worker B (same query)
SELECT * FROM jobs 
WHERE status = 'pending' 
ORDER BY priority DESC, created_at 
LIMIT 1 
FOR UPDATE SKIP LOCKED;  -- SKIPS the locked row, gets next one

-- No conflicts!
```

---

## How It Works

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                      SKIP LOCKED IN ACTION                                   │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│   Jobs Table:                                                                │
│   ┌──────┬──────────┬──────────┬────────┐                                   │
│   │  id  │  status  │ priority │ locked │                                   │
│   ├──────┼──────────┼──────────┼────────┤                                   │
│   │  1   │ pending  │   high   │   -    │                                   │
│   │  2   │ pending  │  normal  │   -    │                                   │
│   │  3   │ pending  │   low    │   -    │                                   │
│   └──────┴──────────┴──────────┴────────┘                                   │
│                                                                              │
│   Step 1: Worker A queries                                                   │
│   ───────────────────────────                                               │
│   SELECT ... FOR UPDATE SKIP LOCKED                                          │
│                                                                              │
│   ┌──────┬──────────┬──────────┬────────┐                                   │
│   │  1   │ pending  │   high   │ 🔒 A   │ ◄── Worker A locks job 1          │
│   │  2   │ pending  │  normal  │   -    │                                   │
│   │  3   │ pending  │   low    │   -    │                                   │
│   └──────┴──────────┴──────────┴────────┘                                   │
│                                                                              │
│   Step 2: Worker B queries (same query)                                      │
│   ─────────────────────────────────────                                      │
│   SELECT ... FOR UPDATE SKIP LOCKED                                          │
│                                                                              │
│   ┌──────┬──────────┬──────────┬────────┐                                   │
│   │  1   │ pending  │   high   │ 🔒 A   │     (skipped - locked)            │
│   │  2   │ pending  │  normal  │ 🔒 B   │ ◄── Worker B gets job 2           │
│   │  3   │ pending  │   low    │   -    │                                   │
│   └──────┴──────────┴──────────┴────────┘                                   │
│                                                                              │
│   Step 3: Worker A updates and commits                                       │
│   ──────────────────────────────────────                                     │
│   UPDATE jobs SET status = 'running' WHERE id = 1;                          │
│   COMMIT;                                                                    │
│                                                                              │
│   ┌──────┬──────────┬──────────┬────────┐                                   │
│   │  1   │ running  │   high   │   -    │     (lock released)               │
│   │  2   │ pending  │  normal  │ 🔒 B   │                                   │
│   │  3   │ pending  │   low    │   -    │                                   │
│   └──────┴──────────┴──────────┴────────┘                                   │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Implementation

### Job Claiming

```rust
impl JobQueue {
    pub async fn claim_jobs(&self, worker_id: Uuid, capabilities: &[String], limit: i32) -> Result<Vec<Job>> {
        // Single atomic operation: SELECT + UPDATE
        let jobs = sqlx::query_as::<_, Job>(r#"
            WITH claimable AS (
                SELECT id
                FROM forge_jobs
                WHERE status = 'pending'
                  AND scheduled_at <= NOW()
                  AND (worker_capability = ANY($2) OR worker_capability IS NULL)
                ORDER BY priority DESC, scheduled_at ASC
                LIMIT $3
                FOR UPDATE SKIP LOCKED
            )
            UPDATE forge_jobs
            SET 
                status = 'claimed',
                worker_id = $1,
                claimed_at = NOW()
            WHERE id IN (SELECT id FROM claimable)
            RETURNING *
        "#)
        .bind(worker_id)
        .bind(capabilities)
        .bind(limit)
        .fetch_all(&self.pool)
        .await?;
        
        Ok(jobs)
    }
}
```

### Why UPDATE in a CTE?

The `WITH ... UPDATE` pattern is atomic:
1. Rows are locked by the SELECT
2. UPDATE happens immediately
3. Other workers can't see or claim these jobs

```rust
// This is WRONG - race condition between SELECT and UPDATE
async fn claim_wrong(&self) -> Result<Job> {
    let job = sqlx::query("SELECT ... FOR UPDATE SKIP LOCKED")
        .fetch_one(&self.pool).await?;
    
    // Another worker could claim this job here!
    
    sqlx::query("UPDATE ... SET status = 'claimed' WHERE id = $1")
        .execute(&self.pool).await?;
    
    Ok(job)
}

// This is RIGHT - atomic claim
async fn claim_right(&self) -> Result<Job> {
    let job = sqlx::query("WITH claimed AS (SELECT ... FOR UPDATE SKIP LOCKED) UPDATE ...")
        .fetch_one(&self.pool).await?;
    
    Ok(job)
}
```

---

## Job States

```
                                    ┌───────────────┐
                                    │   PENDING     │
                                    │               │
                           ┌────────┤ In queue,     │
                           │        │ waiting       │
                           │        └───────┬───────┘
                           │                │
               scheduled_at <= NOW()        │ Worker claims
               (delayed job)                │
                           │                ▼
                           │        ┌───────────────┐
                           │        │   CLAIMED     │
                           │        │               │
                           │        │ Assigned to   │
                           │        │ worker        │
                           │        └───────┬───────┘
                           │                │
                           │                │ Worker starts
                           │                ▼
                           │        ┌───────────────┐
                           │        │   RUNNING     │
                           │        │               │
                           │        │ Executing     │
                           │        └───────┬───────┘
                           │                │
                           │        ┌───────┴───────┐
                           │        │               │
                           │        ▼               ▼
                           │  ┌───────────┐  ┌───────────┐
                           │  │ COMPLETED │  │  FAILED   │
                           │  │           │  │           │
                           │  │ Success!  │  │ Error     │
                           │  └───────────┘  └─────┬─────┘
                           │                       │
                           │                       │ retries < max?
                           │                       │
                           │               ┌───────┴───────┐
                           │               │               │
                           │               ▼               ▼
                           │         ┌───────────┐  ┌─────────────┐
                           └─────────┤   RETRY   │  │ DEAD_LETTER │
                                     │           │  │             │
                                     │ Back to   │  │ Exhausted   │
                                     │ pending   │  │ retries     │
                                     └───────────┘  └─────────────┘
```

---

## Priority Ordering

Jobs are ordered by priority (DESC), then by time (ASC):

```sql
ORDER BY priority DESC, scheduled_at ASC
```

| Priority | Value | Use Case |
|----------|-------|----------|
| Critical | 100 | Payment processing |
| High | 75 | User-facing operations |
| Normal | 50 | Default |
| Low | 25 | Batch processing |
| Background | 0 | Analytics, cleanup |

```rust
#[forge::job]
#[priority = 100]  // Critical
pub async fn process_payment(ctx: &JobContext, input: PaymentInput) -> Result<()> {
    // Processes before all lower-priority jobs
}
```

---

## Delayed Jobs

Schedule jobs for the future:

```rust
// Dispatch with delay
ctx.dispatch_job_in(
    Duration::hours(24),
    send_reminder_email,
    ReminderInput { user_id }
).await?;

// Dispatch at specific time
ctx.dispatch_job_at(
    user.trial_ends_at - Duration::days(7),
    send_trial_ending_email,
    TrialInput { user_id }
).await?;
```

Implementation:

```sql
-- scheduled_at controls when job becomes claimable
INSERT INTO forge_jobs (job_type, input, scheduled_at)
VALUES ('send_reminder', '{}', NOW() + INTERVAL '24 hours');

-- Claim query filters by scheduled_at
SELECT * FROM forge_jobs
WHERE status = 'pending'
  AND scheduled_at <= NOW()  -- Only jobs scheduled for now or past
...
```

---

## Retry Logic

### Exponential Backoff

```rust
fn calculate_backoff(attempts: i32, base: Duration, max: Duration) -> Duration {
    let backoff = base * 2_i32.pow(attempts as u32);
    backoff.min(max)
}

// Attempts:  1     2      3       4        5
// Delay:     1s    2s     4s      8s       16s (capped at max)
```

### Retry Implementation

```rust
async fn handle_job_failure(&self, job: &Job, error: &Error) -> Result<()> {
    if job.attempts < job.max_retries {
        let backoff = self.calculate_backoff(job.attempts);
        
        sqlx::query(r#"
            UPDATE forge_jobs
            SET 
                status = 'pending',
                worker_id = NULL,
                claimed_at = NULL,
                attempts = attempts + 1,
                last_error = $2,
                scheduled_at = NOW() + $3  -- Delay retry
            WHERE id = $1
        "#)
        .bind(&job.id)
        .bind(&error.to_string())
        .bind(&backoff)
        .execute(&self.pool)
        .await?;
    } else {
        // Move to dead letter queue
        sqlx::query(r#"
            UPDATE forge_jobs
            SET 
                status = 'dead_letter',
                last_error = $2,
                failed_at = NOW()
            WHERE id = $1
        "#)
        .bind(&job.id)
        .bind(&error.to_string())
        .execute(&self.pool)
        .await?;
    }
    
    Ok(())
}
```

---

## Performance Optimization

### Index Design

```sql
-- Critical index for job claiming
-- Partial index: only pending jobs
CREATE INDEX idx_jobs_claimable ON forge_jobs (
    priority DESC,
    scheduled_at ASC
)
WHERE status = 'pending';

-- This index is used by:
-- SELECT ... WHERE status = 'pending' ORDER BY priority DESC, scheduled_at
```

### Batch Claiming

Claim multiple jobs in one query:

```rust
// Instead of claiming one at a time:
for _ in 0..10 {
    let job = claim_one_job().await?;  // 10 queries
}

// Claim in batch:
let jobs = claim_jobs(limit = 10).await?;  // 1 query
```

### Connection Pooling

```toml
# forge.toml

[database]
pool_size = 50  # Connections in pool
pool_timeout = "30s"
```

---

## NOTIFY for Instant Wake-up

Instead of polling, workers can wait for notifications:

```sql
-- Trigger when job is inserted
CREATE OR REPLACE FUNCTION notify_job_available() RETURNS TRIGGER AS $$
BEGIN
    PERFORM pg_notify('forge_jobs_available', NEW.worker_capability);
    RETURN NEW;
END;
$$ LANGUAGE plpgsql;

CREATE TRIGGER job_inserted
    AFTER INSERT ON forge_jobs
    FOR EACH ROW
    EXECUTE FUNCTION notify_job_available();
```

```rust
// Worker listens for notifications
async fn worker_loop(&self) {
    let mut listener = PgListener::connect(&self.database_url).await?;
    listener.listen("forge_jobs_available").await?;
    
    loop {
        tokio::select! {
            // Wake up on notification
            notification = listener.recv() => {
                if let Ok(n) = notification {
                    if self.capabilities.contains(&n.payload()) {
                        self.claim_and_process().await?;
                    }
                }
            }
            // Or poll periodically as fallback
            _ = tokio::time::sleep(Duration::seconds(5)) => {
                self.claim_and_process().await?;
            }
        }
    }
}
```

---

## Monitoring

### Queue Depth

```sql
-- Current queue depth by status
SELECT status, worker_capability, count(*)
FROM forge_jobs
GROUP BY status, worker_capability;

-- Pending by priority
SELECT priority, count(*)
FROM forge_jobs
WHERE status = 'pending'
GROUP BY priority
ORDER BY priority DESC;
```

### Throughput

```sql
-- Jobs completed per minute
SELECT 
    date_trunc('minute', completed_at) as minute,
    count(*) as completed
FROM forge_jobs
WHERE completed_at > NOW() - INTERVAL '1 hour'
GROUP BY 1
ORDER BY 1;
```

### Latency

```sql
-- Time from creation to completion
SELECT 
    job_type,
    avg(EXTRACT(EPOCH FROM (completed_at - created_at))) as avg_seconds,
    percentile_cont(0.95) WITHIN GROUP (ORDER BY EXTRACT(EPOCH FROM (completed_at - created_at))) as p95_seconds
FROM forge_jobs
WHERE status = 'completed'
  AND completed_at > NOW() - INTERVAL '1 hour'
GROUP BY job_type;
```

---

## Separate Jobs Database (Recommended for Production)

The job queue has high churn—jobs are constantly created, claimed, updated, and deleted. This creates significant write amplification and vacuum pressure that can impact your main application database.

### Configuration

```toml
# forge.toml

[database]
url = "postgres://user:pass@primary-db:5432/myapp"

[jobs]
# Point jobs to a separate database (optional, defaults to main database)
database_url = "${JOBS_DATABASE_URL}"

# Separate connection pool for job operations
pool_size = 20
pool_timeout = "10s"
```

**When to use a separate jobs database:**
- Job throughput > 100 jobs/second
- Large job payloads (> 10KB input/output)
- Strict latency requirements for user-facing queries
- You're seeing vacuum-related performance issues

**If not specified:** Jobs use the main application database. Fine for development and low-to-medium throughput production workloads.

---

## Vacuum Management (Avoiding "Vacuum Hell")

The `forge_jobs` table is high-churn by nature: rows are constantly inserted, updated, and deleted. Without proper vacuum tuning, you'll face:

- **Table bloat**: Dead tuples accumulate, table size grows unboundedly
- **Index bloat**: B-tree indexes become inefficient
- **Transaction ID wraparound**: Postgres's TXID counter can exhaust

### Recommended PostgreSQL Settings

```sql
-- Aggressive autovacuum settings for forge_jobs
ALTER TABLE forge_jobs SET (
    autovacuum_vacuum_scale_factor = 0.01,      -- Vacuum at 1% dead tuples (vs default 20%)
    autovacuum_vacuum_threshold = 1000,          -- Or at least 1000 dead tuples
    autovacuum_analyze_scale_factor = 0.005,    -- Analyze frequently
    autovacuum_vacuum_cost_delay = 2,           -- Vacuum faster (ms between chunks)
    autovacuum_vacuum_cost_limit = 1000         -- Allow more vacuum work per round
);

-- Same for metrics tables if using same database
ALTER TABLE forge_metrics SET (
    autovacuum_vacuum_scale_factor = 0.01,
    autovacuum_vacuum_threshold = 5000,
    autovacuum_vacuum_cost_delay = 2
);
```

### Table Partitioning for Easy Cleanup

Instead of deleting completed jobs (which creates dead tuples), partition by completion time and drop old partitions:

```sql
-- Partition completed jobs by day
CREATE TABLE forge_jobs_completed (
    LIKE forge_jobs INCLUDING ALL
) PARTITION BY RANGE (completed_at);

-- Create daily partitions
CREATE TABLE forge_jobs_completed_2024_01_15
    PARTITION OF forge_jobs_completed
    FOR VALUES FROM ('2024-01-15') TO ('2024-01-16');

-- Cleanup: Drop entire partition (instant, no vacuum needed)
DROP TABLE forge_jobs_completed_2024_01_01;
```

### Monitoring Vacuum Health

```sql
-- Check for vacuum issues
SELECT
    schemaname, relname,
    n_dead_tup,
    n_live_tup,
    round(n_dead_tup::numeric / NULLIF(n_live_tup, 0) * 100, 2) as dead_pct,
    last_vacuum,
    last_autovacuum
FROM pg_stat_user_tables
WHERE relname LIKE 'forge_%'
ORDER BY n_dead_tup DESC;

-- Check table bloat
SELECT
    tablename,
    pg_size_pretty(pg_total_relation_size(schemaname || '.' || tablename)) as total_size,
    pg_size_pretty(pg_relation_size(schemaname || '.' || tablename)) as table_size
FROM pg_tables
WHERE tablename LIKE 'forge_%';
```

---

## Comparison to Alternatives

| Feature | PostgreSQL SKIP LOCKED | Redis + Bull | RabbitMQ |
|---------|----------------------|--------------|----------|
| Additional infrastructure | None | Redis | RabbitMQ |
| Durability | ACID | Configurable | Configurable |
| Exactly-once | Yes (with idempotency) | At-least-once | At-least-once |
| Priority queues | Native | Supported | Supported |
| Delayed jobs | Native | Supported | Plugin |
| Visibility into queue | SQL queries | Redis CLI | Management UI |
| Scaling | Good | Excellent | Excellent |

For most applications, PostgreSQL is sufficient. Consider alternatives when:
- Queue depth > 1M jobs
- Need > 10,000 jobs/second throughput
- Complex routing (fan-out, pub/sub)

---

## Optional Redis Backend (Escape Hatch)

PostgreSQL handles most workloads well, but when you genuinely need higher throughput, FORGE supports Redis as an optional job queue backend.

### When to Consider Redis

| Scenario | PostgreSQL | Redis |
|----------|------------|-------|
| < 1,000 jobs/sec | Recommended | Overkill |
| 1,000 - 5,000 jobs/sec | Fine with tuning | Good |
| 5,000 - 50,000 jobs/sec | Struggling | Recommended |
| > 50,000 jobs/sec | Not suitable | Good (with cluster) |

**Rule of thumb:** Start with PostgreSQL. Only switch to Redis if you have measured evidence that PostgreSQL is the bottleneck.

### Configuration

```toml
# forge.toml

[jobs]
# Default: PostgreSQL (no additional config needed)
backend = "postgres"

# Switch to Redis for high-throughput
backend = "redis"
redis_url = "${REDIS_URL}"

# Optional: Redis-specific settings
[jobs.redis]
pool_size = 20
key_prefix = "forge:jobs:"
```

### Hybrid Mode

Use PostgreSQL for most jobs, Redis for high-volume specific job types:

```toml
# forge.toml

[jobs]
# Default backend
backend = "postgres"

# Override for specific job types
[jobs.routing]
# High-volume analytics events → Redis
"track_event" = "redis"
"process_webhook" = "redis"

# Everything else stays on PostgreSQL
# (workflows, emails, reports, etc.)

[jobs.redis]
url = "${REDIS_URL}"
```

```rust
// This job uses PostgreSQL (default)
#[forge::job]
pub async fn send_email(ctx: &JobContext, input: EmailInput) -> Result<()> {
    // ...
}

// This job uses Redis (configured in forge.toml)
#[forge::job]
pub async fn track_event(ctx: &JobContext, input: EventInput) -> Result<()> {
    // High-volume, can handle some loss
}
```

### Trade-offs: Redis vs PostgreSQL

| Aspect | PostgreSQL | Redis |
|--------|------------|-------|
| **Durability** | ACID (survives crashes) | Configurable (AOF/RDB) |
| **Exactly-once** | Yes (with transactions) | At-least-once |
| **Visibility** | SQL queries, joins | Redis CLI, limited queries |
| **Ops complexity** | Already have it | Another service to manage |
| **Job inspection** | Full SQL access | Limited introspection |
| **Dependencies** | None (required anyway) | Additional infrastructure |

### Redis Implementation

When Redis backend is enabled, FORGE uses Redis lists with reliable queue pattern:

```
                    ┌─────────────────────────────────────────────────┐
                    │                   REDIS                          │
                    │                                                  │
                    │   forge:jobs:pending     (sorted set by score)   │
                    │   forge:jobs:claimed:{worker_id} (processing)    │
                    │   forge:jobs:data:{job_id}  (job payload)        │
                    │                                                  │
                    └─────────────────────────────────────────────────┘

Worker claims job:
1. BZPOPMIN forge:jobs:pending (blocking pop lowest score)
2. SADD forge:jobs:claimed:{worker_id} {job_id}
3. GET forge:jobs:data:{job_id}
4. Process job...
5. SREM forge:jobs:claimed:{worker_id} {job_id}
6. DEL forge:jobs:data:{job_id}

If worker crashes:
- Other workers scan stale claimed sets
- Re-queue orphaned jobs
```

### Durability Configuration

```toml
# forge.toml

[jobs.redis]
# For jobs where data loss is unacceptable (payments, etc.)
# Don't use Redis for these — keep them on PostgreSQL

# For high-volume jobs where occasional loss is acceptable
durability = "best_effort"  # Use Redis defaults (fast)

# Or for important Redis jobs
durability = "fsync"  # Wait for AOF fsync (slower but durable)
```

### Fallback Behavior

If Redis is unavailable:

```toml
[jobs.redis]
# What to do when Redis is down
fallback = "postgres"  # Queue to PostgreSQL instead (default)
fallback = "reject"    # Fail fast, return error to caller
fallback = "memory"    # Queue in-memory (loses on restart)
```

### Monitoring Redis Queue

```bash
# CLI commands
forge jobs status --backend redis

# Output:
# Redis Queue Status
# ═══════════════════════════════════════
# Pending:     1,234
# Processing:  45
# Workers:     8 active
#
# Throughput (last 5m):
#   Dispatched: 12,340/min
#   Completed:  12,298/min
#   Failed:     42/min
```

### Migration Path

```
Day 1: Start with PostgreSQL
       ↓
       (measure: jobs/sec, queue depth, latency)
       ↓
Hit limits: > 1000 jobs/sec sustained, growing queue
       ↓
Add Redis for high-volume job types only:
       [jobs.routing]
       "high_volume_job" = "redis"
       ↓
       (most jobs still on PostgreSQL)
       ↓
If needed: Move more job types to Redis
```

### Don't Use Redis If:

1. **You're under 1,000 jobs/second** — PostgreSQL is fine
2. **Job durability is critical** — Use PostgreSQL
3. **You need complex job queries** — PostgreSQL has SQL
4. **You want fewer moving parts** — PostgreSQL is already required
5. **Your jobs are long-running** — Queue throughput isn't your bottleneck

---

## Related Documentation

- [Jobs](../core/JOBS.md) — Job definitions
- [Workers](../cluster/WORKERS.md) — Worker configuration
- [PostgreSQL Schema](POSTGRES_SCHEMA.md) — Table definitions

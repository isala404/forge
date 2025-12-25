# Cron Jobs

> *Scheduled tasks that run reliably*

---

## Overview

Cron jobs are **scheduled tasks** that run at specified intervals. FORGE crons are:

- **Exactly-once**: Each scheduled time runs exactly once
- **Distributed**: Only one node runs each cron (leader election)
- **Timezone-aware**: Schedule in any timezone
- **Persistent**: Survive restarts, catch up missed runs

---

## Defining Crons

### Basic Cron

```rust
// functions/crons/cleanup.rs

use forge::prelude::*;

#[forge::cron("0 0 * * *")]  // Every day at midnight UTC
pub async fn daily_cleanup(ctx: &CronContext) -> Result<()> {
    // Delete old temporary files
    ctx.mutate(cleanup_temp_files, CleanupInput {
        older_than: Duration::days(7),
    }).await?;
    
    // Clean up expired sessions
    ctx.mutate(cleanup_expired_sessions, ()).await?;
    
    Ok(())
}
```

### Cron Expression Format

FORGE uses standard cron syntax with seconds precision:

```
┌──────────── second (0-59) [optional]
│ ┌────────── minute (0-59)
│ │ ┌──────── hour (0-23)
│ │ │ ┌────── day of month (1-31)
│ │ │ │ ┌──── month (1-12)
│ │ │ │ │ ┌── day of week (0-6, Sun=0)
│ │ │ │ │ │
* * * * * *
```

| Expression | Meaning |
|------------|---------|
| `* * * * *` | Every minute |
| `*/5 * * * *` | Every 5 minutes |
| `0 * * * *` | Every hour |
| `0 0 * * *` | Every day at midnight |
| `0 0 * * 0` | Every Sunday at midnight |
| `0 9 * * 1-5` | Weekdays at 9 AM |
| `0 0 1 * *` | First day of each month |
| `30 4 1,15 * *` | 4:30 AM on 1st and 15th |

### Timezone Support

```rust
#[forge::cron("0 9 * * *")]
#[timezone = "America/New_York"]  // 9 AM Eastern Time
pub async fn morning_report(ctx: &CronContext) -> Result<()> {
    // Runs at 9 AM ET, adjusts for DST automatically
    ...
}

#[forge::cron("0 0 * * *")]
#[timezone = "UTC"]  // Explicit UTC (default)
pub async fn midnight_utc(ctx: &CronContext) -> Result<()> {
    ...
}
```

FORGE uses the IANA timezone database. Common timezones:
- `UTC` — Coordinated Universal Time
- `America/New_York` — US Eastern
- `America/Los_Angeles` — US Pacific
- `Europe/London` — UK
- `Asia/Tokyo` — Japan

---

## Cron Context

The cron context provides information about the scheduled run:

```rust
#[forge::cron("0 * * * *")]
pub async fn hourly_job(ctx: &CronContext) -> Result<()> {
    // Scheduled time (not actual execution time)
    let scheduled = ctx.scheduled_time;
    
    // Actual execution time
    let now = ctx.execution_time;
    
    // Delay (if any)
    let delay = now - scheduled;
    if delay > Duration::minutes(5) {
        ctx.log.warn("Cron running late", json!({ "delay_seconds": delay.as_secs() }));
    }
    
    // Run queries
    let stats = ctx.query(get_hourly_stats, GetStatsInput {
        hour: scheduled.hour(),
    }).await?;
    
    // Run mutations
    ctx.mutate(save_stats, SaveStatsInput { stats }).await?;
    
    // Dispatch jobs
    ctx.dispatch_job(process_hourly_data, ProcessInput {
        hour: scheduled,
    }).await?;
    
    Ok(())
}
```

---

## Handling Missed Runs

When a server restarts, FORGE can catch up on missed crons:

```rust
#[forge::cron("0 0 * * *")]
#[catch_up = true]  // Run missed executions
#[catch_up_limit = 7]  // At most 7 missed runs
pub async fn daily_billing(ctx: &CronContext) -> Result<()> {
    // If server was down for 3 days, this runs 3 times on startup
    let date = ctx.scheduled_time.date();
    
    ctx.dispatch_job(process_daily_billing, BillingInput { date }).await?;
    
    Ok(())
}

#[forge::cron("*/5 * * * *")]
#[catch_up = false]  // Skip missed runs (default)
pub async fn health_check(ctx: &CronContext) -> Result<()> {
    // If we miss some health checks, just continue with current
    ...
}
```

### Catch-Up Behavior

| Scenario | `catch_up = true` | `catch_up = false` |
|----------|-------------------|-------------------|
| Server down 1 hour (12 missed 5-min runs) | Runs 12 times quickly | Runs once at next interval |
| Server down 1 day (24 missed hourly runs) | Runs 24 times quickly | Runs once at next hour |
| Missed run during long execution | Queues missed run | Skips missed run |

---

## Execution Guarantees

### Exactly-Once Execution

FORGE ensures each scheduled time runs exactly once, even in a cluster:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                      CRON SCHEDULING (CLUSTER)                               │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│   Time: 00:00:00 UTC — Daily cron scheduled                                  │
│                                                                              │
│   ┌─────────────┐    ┌─────────────┐    ┌─────────────┐                     │
│   │   Node 1    │    │   Node 2    │    │   Node 3    │                     │
│   │  (leader)   │    │  (standby)  │    │  (standby)  │                     │
│   └──────┬──────┘    └─────────────┘    └─────────────┘                     │
│          │                                                                   │
│          │  1. Leader sees cron due                                          │
│          │                                                                   │
│          │  2. INSERT INTO forge_cron_runs (cron, scheduled_time)            │
│          │     ON CONFLICT DO NOTHING                                        │
│          │                                                                   │
│          │  3. If inserted (not conflict), execute cron                      │
│          │                                                                   │
│          ▼                                                                   │
│   ┌─────────────┐                                                            │
│   │  Cron runs  │ ◄── Only runs once per scheduled_time                      │
│   │  on Node 1  │                                                            │
│   └─────────────┘                                                            │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

### What If Leader Fails?

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                      LEADER FAILURE DURING CRON                              │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│   Time: 00:00:00 — Leader starts cron                                        │
│   Time: 00:00:15 — Leader crashes mid-execution                              │
│   Time: 00:00:20 — Node 2 becomes new leader                                 │
│   Time: 00:00:25 — New leader checks for incomplete crons                    │
│                                                                              │
│   ┌─────────────┐                                                            │
│   │   Node 2    │                                                            │
│   │ (new leader)│                                                            │
│   └──────┬──────┘                                                            │
│          │                                                                   │
│          │  SELECT * FROM forge_cron_runs                                    │
│          │  WHERE status = 'running'                                         │
│          │  AND started_at < NOW() - INTERVAL '5 minutes'                    │
│          │                                                                   │
│          │  Found: daily_cleanup at 00:00:00 (incomplete)                    │
│          │                                                                   │
│          │  Decision: Re-run (cron is idempotent)                            │
│          │                                                                   │
│          ▼                                                                   │
│   ┌─────────────┐                                                            │
│   │ Cron re-run │                                                            │
│   │ on Node 2   │                                                            │
│   └─────────────┘                                                            │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Cron Patterns

### Dispatch Jobs from Crons

For heavy work, dispatch jobs instead of running inline:

```rust
#[forge::cron("0 0 * * *")]
pub async fn daily_reports(ctx: &CronContext) -> Result<()> {
    // Get all tenants
    let tenants = ctx.query(get_all_tenants, ()).await?;
    
    // Dispatch a job for each tenant
    for tenant in tenants {
        ctx.dispatch_job(generate_tenant_report, TenantReportInput {
            tenant_id: tenant.id,
            date: ctx.scheduled_time.date(),
        }).await?;
    }
    
    Ok(())
}
```

### Staggered Processing

Avoid thundering herd:

```rust
#[forge::cron("0 0 * * *")]
pub async fn daily_sync(ctx: &CronContext) -> Result<()> {
    let tenants = ctx.query(get_all_tenants, ()).await?;
    
    for (i, tenant) in tenants.into_iter().enumerate() {
        // Stagger jobs by 1 minute each
        let delay = Duration::minutes(i as i64);
        
        ctx.dispatch_job_in(delay, sync_tenant, SyncInput {
            tenant_id: tenant.id,
        }).await?;
    }
    
    Ok(())
}
```

### Conditional Execution

```rust
#[forge::cron("0 * * * *")]
pub async fn conditional_cron(ctx: &CronContext) -> Result<()> {
    // Only run on weekdays
    let day = ctx.scheduled_time.weekday();
    if day == Weekday::Sat || day == Weekday::Sun {
        ctx.log.info("Skipping weekend");
        return Ok(());
    }
    
    // Only run during business hours (9 AM - 5 PM)
    let hour = ctx.scheduled_time.hour();
    if hour < 9 || hour >= 17 {
        ctx.log.info("Outside business hours");
        return Ok(());
    }
    
    do_business_hours_task().await
}
```

---

## Monitoring

### Built-in Metrics

| Metric | Description |
|--------|-------------|
| `forge_cron_runs_total` | Total runs by cron name |
| `forge_cron_duration_seconds` | Execution duration |
| `forge_cron_failures_total` | Failed runs |
| `forge_cron_delay_seconds` | Delay from scheduled time |
| `forge_cron_missed_total` | Missed runs (when catch_up=false) |

### Dashboard

The dashboard shows:
- Next scheduled run time for each cron
- Execution history
- Failure alerts
- Duration trends

### Alerts

```toml
# forge.toml

[[alerts]]
name = "cron_failure"
condition = "forge_cron_failures_total > 0"
for = "1m"
severity = "warning"
notify = ["slack:#ops"]

[[alerts]]
name = "cron_delayed"
condition = "forge_cron_delay_seconds > 300"
severity = "warning"
notify = ["slack:#ops"]
```

---

## Testing Crons

### Manual Trigger

```bash
# Trigger a cron manually
forge cron trigger daily_cleanup

# Trigger with specific scheduled time
forge cron trigger daily_cleanup --at "2024-01-15T00:00:00Z"
```

### Unit Testing

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use forge::testing::*;
    
    #[tokio::test]
    async fn test_daily_cleanup() {
        let ctx = TestCronContext::new()
            .with_scheduled_time("2024-01-15T00:00:00Z")
            .build();
        
        daily_cleanup(&ctx).await.unwrap();
        
        // Verify mutations were called
        assert!(ctx.mutations_called().contains("cleanup_temp_files"));
        assert!(ctx.mutations_called().contains("cleanup_expired_sessions"));
    }
}
```

---

## Best Practices

### 1. Make Crons Idempotent

```rust
// ❌ Not idempotent - creates duplicate reports
#[forge::cron("0 0 * * *")]
pub async fn bad_report(ctx: &CronContext) -> Result<()> {
    let report = generate_report().await?;
    ctx.mutate(create_report, report).await?;
}

// ✅ Idempotent - checks before creating
#[forge::cron("0 0 * * *")]
pub async fn good_report(ctx: &CronContext) -> Result<()> {
    let date = ctx.scheduled_time.date();
    
    // Check if report already exists
    let exists = ctx.query(report_exists, ReportExistsInput { date }).await?;
    if exists {
        return Ok(());
    }
    
    let report = generate_report().await?;
    ctx.mutate(create_report, report).await?;
    Ok(())
}
```

### 2. Keep Crons Fast

```rust
// ❌ Slow cron blocks scheduler
#[forge::cron("0 * * * *")]
pub async fn slow_cron(ctx: &CronContext) -> Result<()> {
    // Processing 10000 items inline...
    for item in get_all_items().await? {
        process_item(&item).await?;
    }
}

// ✅ Dispatch work to jobs
#[forge::cron("0 * * * *")]
pub async fn fast_cron(ctx: &CronContext) -> Result<()> {
    let items = get_all_items().await?;
    
    for chunk in items.chunks(100) {
        ctx.dispatch_job(process_items_batch, BatchInput {
            ids: chunk.iter().map(|i| i.id).collect(),
        }).await?;
    }
    
    Ok(())
}
```

### 3. Log Important Information

```rust
#[forge::cron("0 0 * * *")]
pub async fn important_cron(ctx: &CronContext) -> Result<()> {
    ctx.log.info("Starting daily process", json!({
        "scheduled_time": ctx.scheduled_time,
    }));
    
    let result = do_work().await?;
    
    ctx.log.info("Completed daily process", json!({
        "items_processed": result.count,
        "duration_ms": result.duration.as_millis(),
    }));
    
    Ok(())
}
```

---

## Related Documentation

- [Jobs](JOBS.md) — Background job processing
- [Leader Election](../cluster/LEADER_ELECTION.md) — How cron leader is selected
- [Workflows](WORKFLOWS.md) — Complex multi-step processes

# Crons

FORGE implements a scheduled task system using the `#[forge::cron]` macro. Crons are executed by a single leader node in the cluster, with exactly-once semantics guaranteed via a PostgreSQL UNIQUE constraint on `(cron_name, scheduled_time)`.

---

## Defining Crons

### Basic Cron

Use the `#[forge::cron]` macro with a cron expression to define scheduled tasks:

```rust
use forge::prelude::*;

#[forge::cron("0 0 * * *")]  // Daily at midnight UTC
pub async fn daily_cleanup(ctx: &CronContext) -> Result<()> {
    tracing::info!(run_id = %ctx.run_id, "Running daily cleanup");

    sqlx::query("DELETE FROM temp_files WHERE created_at < NOW() - INTERVAL '7 days'")
        .execute(ctx.db())
        .await?;

    Ok(())
}
```

The macro generates a struct `DailyCleanupCron` implementing the `ForgeCron` trait. Register it in your main function:

```rust
Forge::builder()
    .config(config)
    .cron::<functions::DailyCleanupCron>()
    .build()?
    .run()
    .await
```

### Cron Attributes

| Attribute | Type | Default | Description |
|-----------|------|---------|-------------|
| Schedule (macro argument) | string | Required | Cron expression (5 or 6 parts) |
| `timezone` | string | `"UTC"` | IANA timezone for schedule interpretation |
| `catch_up` | flag | `false` | Run missed executions on startup |
| `catch_up_limit` | integer | `10` | Maximum catch-up runs |
| `timeout` | duration | `"1h"` | Maximum execution time |

```rust
#[forge::cron("0 9 * * 1-5")]  // 9 AM on weekdays
#[timezone = "America/New_York"]
#[catch_up = 5]
#[timeout = "30m"]
pub async fn business_hours_report(ctx: &CronContext) -> Result<()> {
    // Runs at 9 AM Eastern, adjusts for DST automatically
    // If server was down, catches up to 5 missed runs
    // Times out after 30 minutes
    Ok(())
}
```

---

## Schedule Expression Format

FORGE uses standard cron syntax. Both 5-part and 6-part expressions are supported:

```
5-part:  * * * * *       (minute, hour, day, month, weekday)
6-part:  * * * * * *     (second, minute, hour, day, month, weekday)
```

When you provide a 5-part expression, FORGE automatically normalizes it to 6-part by prepending `0` for seconds.

| Expression | Normalized | Meaning |
|------------|------------|---------|
| `* * * * *` | `0 * * * * *` | Every minute at :00 |
| `*/5 * * * *` | `0 */5 * * * *` | Every 5 minutes at :00 |
| `0 0 * * *` | `0 0 0 * * *` | Daily at midnight |
| `0 9 * * 1-5` | `0 0 9 * * 1-5` | Weekdays at 9:00 AM |
| `30 */5 * * * *` | `30 */5 * * * *` | Every 5 minutes at :30 |

The schedule is parsed using the [cron](https://crates.io/crates/cron) crate.

---

## Timezone Support

FORGE uses [chrono-tz](https://crates.io/crates/chrono-tz) for timezone handling. All IANA timezone names are supported:

```rust
#[forge::cron("0 0 * * *")]
#[timezone = "UTC"]  // Default
pub async fn utc_midnight(ctx: &CronContext) -> Result<()> { Ok(()) }

#[forge::cron("0 9 * * *")]
#[timezone = "America/New_York"]  // 9 AM Eastern (respects DST)
pub async fn eastern_morning(ctx: &CronContext) -> Result<()> { Ok(()) }

#[forge::cron("0 0 * * *")]
#[timezone = "Asia/Tokyo"]  // Midnight JST
pub async fn tokyo_midnight(ctx: &CronContext) -> Result<()> { Ok(()) }
```

Common timezones:
- `UTC` - Coordinated Universal Time
- `America/New_York` - US Eastern
- `America/Los_Angeles` - US Pacific
- `America/Chicago` - US Central
- `Europe/London` - UK
- `Europe/Paris` - Central European
- `Asia/Tokyo` - Japan
- `Australia/Sydney` - Australia Eastern

The `CronSchedule::between_in_tz()` method handles timezone conversion, including daylight saving time transitions.

---

## CronContext

The context passed to cron handlers provides:

```rust
// crates/forge-core/src/cron/context.rs
pub struct CronContext {
    pub run_id: Uuid,                        // Unique ID for this execution
    pub cron_name: String,                   // Cron function name
    pub scheduled_time: DateTime<Utc>,       // When the cron was scheduled to run
    pub execution_time: DateTime<Utc>,       // When execution actually started
    pub timezone: String,                    // Configured timezone
    pub is_catch_up: bool,                   // True if this is a catch-up run
    pub auth: AuthContext,                   // Authentication context (unauthenticated by default)
    pub log: CronLog,                        // Structured logger with cron context
}
```

**Methods:**

| Method | Return Type | Description |
|--------|-------------|-------------|
| `db()` | `&PgPool` | Get the database connection pool |
| `http()` | `&reqwest::Client` | Get the HTTP client |
| `delay()` | `chrono::Duration` | Time between scheduled and actual execution |
| `is_late()` | `bool` | True if delay > 1 minute |
| `with_auth(auth)` | `Self` | Set authentication context |

**Example usage:**

```rust
#[forge::cron("*/5 * * * *")]
pub async fn check_health(ctx: &CronContext) -> Result<()> {
    if ctx.is_late() {
        ctx.log.warn("Cron running late", serde_json::json!({
            "delay_seconds": ctx.delay().num_seconds()
        }));
    }

    // Use database
    let count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(ctx.db())
        .await?;

    // Make HTTP requests
    let response = ctx.http()
        .get("https://api.example.com/health")
        .send()
        .await?;

    ctx.log.info("Health check complete", serde_json::json!({
        "user_count": count.0,
        "api_status": response.status().as_u16()
    }));

    Ok(())
}
```

### CronLog

Structured logging with cron context automatically included:

```rust
pub struct CronLog {
    cron_name: String,
}

impl CronLog {
    pub fn info(&self, message: &str, data: serde_json::Value);
    pub fn warn(&self, message: &str, data: serde_json::Value);
    pub fn error(&self, message: &str, data: serde_json::Value);
    pub fn debug(&self, message: &str, data: serde_json::Value);
}
```

Logs are emitted via [tracing](https://crates.io/crates/tracing) with the cron name automatically included as a field.

---

## ForgeCron Trait

The `#[forge::cron]` macro generates an implementation of:

```rust
// crates/forge-core/src/cron/traits.rs
pub trait ForgeCron: Send + Sync + 'static {
    fn info() -> CronInfo;
    fn execute(ctx: &CronContext) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
}
```

### CronInfo

Metadata about the cron, returned by `ForgeCron::info()`:

```rust
pub struct CronInfo {
    pub name: &'static str,              // Function name
    pub schedule: CronSchedule,          // Parsed schedule
    pub timezone: &'static str,          // Timezone string
    pub catch_up: bool,                  // Whether to catch up missed runs
    pub catch_up_limit: u32,             // Max catch-up runs (default: 10)
    pub timeout: std::time::Duration,    // Execution timeout (default: 1 hour)
}
```

---

## Leader-Only Execution

Crons run only on the scheduler leader node to ensure exactly-once execution.

### How It Works

1. **Leader Check**: The `CronRunner` only executes `tick()` when `config.is_leader` is true
2. **UNIQUE Constraint**: Each scheduled time is claimed by inserting into `forge_cron_runs`:

```sql
INSERT INTO forge_cron_runs (id, cron_name, scheduled_time, status, node_id, started_at)
VALUES ($1, $2, $3, 'running', $4, NOW())
ON CONFLICT (cron_name, scheduled_time) DO NOTHING
```

3. **Rows Affected Check**: If `rows_affected > 0`, this node claimed the run and executes it
4. **Status Updates**: After execution, status is updated to `completed` or `failed`

### Database Schema

```sql
CREATE TABLE IF NOT EXISTS forge_cron_runs (
    id UUID PRIMARY KEY,
    cron_name VARCHAR(255) NOT NULL,
    scheduled_time TIMESTAMPTZ NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'pending',
    node_id UUID,
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    error TEXT,
    UNIQUE(cron_name, scheduled_time)
);

CREATE INDEX IF NOT EXISTS idx_forge_cron_runs_name_time
    ON forge_cron_runs(cron_name, scheduled_time DESC);
```

The `UNIQUE(cron_name, scheduled_time)` constraint ensures that even if multiple nodes attempt to claim the same scheduled time, only one succeeds.

### CronStatus

```rust
pub enum CronStatus {
    Pending,    // Not yet started (not used in current implementation)
    Running,    // Currently executing
    Completed,  // Finished successfully
    Failed,     // Failed with error
}
```

---

## Catch-Up Runs

When `catch_up` is enabled, the cron runner processes missed executions after startup:

```rust
#[forge::cron("0 0 * * *")]
#[catch_up = 7]  // Catch up to 7 missed runs
pub async fn daily_billing(ctx: &CronContext) -> Result<()> {
    if ctx.is_catch_up {
        tracing::info!("Processing missed billing for {}", ctx.scheduled_time);
    }
    // Process billing for ctx.scheduled_time.date()
    Ok(())
}
```

**Catch-up behavior:**

1. Find the last completed run from `forge_cron_runs`
2. Calculate all scheduled times between last run and now
3. Execute up to `catch_up_limit` missed runs in order
4. Each catch-up run has `ctx.is_catch_up = true`

---

## CronRunner

The runtime component that schedules and executes crons.

```rust
// crates/forge-runtime/src/cron/scheduler.rs
pub struct CronRunner {
    registry: Arc<CronRegistry>,
    pool: sqlx::PgPool,
    http_client: reqwest::Client,
    config: CronRunnerConfig,
}
```

### CronRunnerConfig

```rust
pub struct CronRunnerConfig {
    pub poll_interval: Duration,  // How often to check for due crons (default: 1s)
    pub node_id: Uuid,            // This node's ID
    pub is_leader: bool,          // Only leaders run crons (default: true)
}
```

### Tick Loop

Every `poll_interval`, the runner:

1. Calculates a time window (`now - 2 * poll_interval` to `now`)
2. For each registered cron, finds scheduled times in that window using `between_in_tz()`
3. Attempts to claim each scheduled time via `try_claim()`
4. If claimed, executes the cron with timeout handling
5. Records metrics and traces for observability
6. Handles catch-up if enabled

### Observability

The runner records:

- `cron_runs_total` - Counter with labels `cron_name`, `is_catch_up`
- `cron_duration_seconds` - Gauge with label `cron_name`
- `cron_success_total` - Counter with label `cron_name`
- `cron_failures_total` - Counter with labels `cron_name`, `reason` (error/timeout)

Traces are recorded as spans with kind `Internal` and attributes:
- `cron.name`, `cron.run_id`, `cron.scheduled_time`, `cron.is_catch_up`, `cron.duration_ms`

---

## CronRegistry

Stores registered cron handlers for runtime lookup.

```rust
// crates/forge-runtime/src/cron/registry.rs
pub struct CronRegistry {
    crons: HashMap<String, CronEntry>,
}

impl CronRegistry {
    pub fn register<C: ForgeCron>(&mut self);
    pub fn get(&self, name: &str) -> Option<&CronEntry>;
    pub fn list(&self) -> Vec<&CronEntry>;
    pub fn len(&self) -> usize;
    pub fn is_empty(&self) -> bool;
    pub fn names(&self) -> Vec<&str>;
}
```

---

## Duration Format

The `timeout` attribute accepts duration strings:

| Format | Example | Meaning |
|--------|---------|---------|
| Milliseconds | `"500ms"` | 500 milliseconds |
| Seconds | `"30s"` | 30 seconds |
| Minutes | `"5m"` | 5 minutes |
| Hours | `"2h"` | 2 hours |
| Numeric | `"3600"` | 3600 seconds |

---

## Code Examples

### Minute-Level Stats Update

```rust
#[forge::cron("* * * * *")]  // Every minute
#[timezone = "UTC"]
pub async fn heartbeat_stats(ctx: &CronContext) -> Result<()> {
    let now = chrono::Utc::now();
    tracing::debug!(run_id = %ctx.run_id, "Running heartbeat stats");

    // Update last heartbeat timestamp
    sqlx::query(
        "INSERT INTO app_stats (id, stat_name, stat_value, updated_at)
         VALUES ('heartbeat', 'last_heartbeat', $1, NOW())
         ON CONFLICT (id) DO UPDATE SET stat_value = $1, updated_at = NOW()"
    )
    .bind(now.to_rfc3339())
    .execute(ctx.db())
    .await?;

    // Count users
    let user_count: (i64,) = sqlx::query_as("SELECT COUNT(*) FROM users")
        .fetch_one(ctx.db())
        .await?;

    sqlx::query(
        "INSERT INTO app_stats (id, stat_name, stat_value, updated_at)
         VALUES ('user_count', 'total_users', $1, NOW())
         ON CONFLICT (id) DO UPDATE SET stat_value = $1, updated_at = NOW()"
    )
    .bind(user_count.0.to_string())
    .execute(ctx.db())
    .await?;

    Ok(())
}
```

### Daily Cleanup with Catch-Up

```rust
#[forge::cron("0 3 * * *")]  // 3 AM daily
#[timezone = "UTC"]
#[catch_up = 7]
#[timeout = "30m"]
pub async fn nightly_cleanup(ctx: &CronContext) -> Result<()> {
    ctx.log.info("Starting cleanup", serde_json::json!({
        "is_catch_up": ctx.is_catch_up,
        "scheduled_time": ctx.scheduled_time.to_rfc3339()
    }));

    // Delete old records
    let deleted = sqlx::query("DELETE FROM audit_logs WHERE created_at < NOW() - INTERVAL '90 days'")
        .execute(ctx.db())
        .await?
        .rows_affected();

    ctx.log.info("Cleanup complete", serde_json::json!({
        "deleted_rows": deleted
    }));

    Ok(())
}
```

### Business Hours Cron

```rust
#[forge::cron("0 */15 9-17 * * 1-5")]  // Every 15 min, 9-5, Mon-Fri
#[timezone = "America/New_York"]
pub async fn business_hours_sync(ctx: &CronContext) -> Result<()> {
    // Only runs during Eastern business hours
    let response = ctx.http()
        .get("https://api.partner.com/sync")
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(forge_core::ForgeError::External(
            format!("Sync failed: {}", response.status())
        ));
    }

    Ok(())
}
```

---

## Differences from Proposal

| Proposed Feature | Implementation Status |
|-----------------|----------------------|
| `ctx.mutate()` / `ctx.query()` helpers | Not implemented - use `ctx.db()` directly with sqlx |
| `ctx.dispatch_job()` | Not implemented - dispatch jobs via MutationContext in functions |
| `ctx.dispatch_job_in(delay, ...)` | Not implemented |
| `#[catch_up = true]` boolean syntax | Implemented as `#[catch_up]` flag or `#[catch_up = N]` |
| `overlap = "skip"` attribute | Not implemented |
| Leader failure recovery with incomplete run detection | Not implemented - relies on UNIQUE constraint only |
| Dashboard trigger/pause/resume controls | Dashboard endpoints exist but CLI not implemented |
| Unit testing with `TestCronContext` | Not implemented |

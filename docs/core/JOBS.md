# Background Jobs

Durable background tasks with automatic retries, priority queues, and progress tracking.

## Overview

Jobs are background tasks that:
- Survive server restarts via PostgreSQL persistence
- Retry automatically on failure with configurable backoff
- Support scheduling for immediate or future execution
- Report progress in real-time via WebSocket
- Use SKIP LOCKED for safe distributed processing

## Defining Jobs

### Basic Job

```rust
use forge::prelude::*;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendEmailInput {
    pub user_id: Uuid,
    pub template: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SendEmailOutput {
    pub sent: bool,
}

#[forge::job]
pub async fn send_email(ctx: &JobContext, input: SendEmailInput) -> Result<SendEmailOutput> {
    let _ = ctx.progress(0, "Starting email send...");

    // Fetch user data
    let user: (String,) = sqlx::query_as("SELECT email FROM users WHERE id = $1")
        .bind(input.user_id)
        .fetch_one(ctx.db())
        .await?;

    let _ = ctx.progress(50, "Sending email...");

    // Send email via external service
    let response = ctx.http()
        .post("https://api.email.com/send")
        .json(&serde_json::json!({
            "to": user.0,
            "template": input.template,
        }))
        .send()
        .await?;

    let _ = ctx.progress(100, "Email sent successfully");

    Ok(SendEmailOutput { sent: response.status().is_success() })
}
```

The `#[forge::job]` macro generates a struct named `{FunctionName}Job` (e.g., `SendEmailJob`) that implements the `ForgeJob` trait.

### Job with Configuration

```rust
#[forge::job]
#[timeout = "30m"]
#[priority = "high"]
#[retry(max_attempts = 5, backoff = "exponential", max_backoff = "10m")]
#[worker_capability = "media"]
pub async fn transcode_video(ctx: &JobContext, input: VideoInput) -> Result<VideoOutput> {
    // Long-running video processing
    for i in 0..100 {
        let _ = ctx.progress(i, format!("Processing frame {}...", i));
        ctx.heartbeat().await?; // Keep job alive
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    Ok(VideoOutput { path: "/output/video.mp4".into() })
}
```

## Macro Attributes

| Attribute | Type | Default | Description |
|-----------|------|---------|-------------|
| `name` | `string` | function name | Override job type name |
| `timeout` | `duration` | `1h` | Maximum execution time |
| `priority` | `string` | `normal` | Default priority level |
| `max_attempts` | `u32` | `3` | Maximum retry attempts |
| `worker_capability` | `string` | none | Required worker capability |
| `idempotent` | flag | `false` | Enable idempotency checking |

Duration format: `30s`, `5m`, `2h`, `1000ms`

### Retry Configuration

Use the `#[retry(...)]` attribute for fine-grained control:

```rust
#[retry(
    max_attempts = 5,
    backoff = "exponential",  // fixed, linear, or exponential
    max_backoff = "5m"        // cap backoff duration
)]
```

| Backoff Strategy | Behavior |
|------------------|----------|
| `fixed` | Same delay each time (1s) |
| `linear` | Delay increases linearly (1s, 2s, 3s, ...) |
| `exponential` | Delay doubles each time (1s, 2s, 4s, 8s, ...) |

## JobContext

The `JobContext` provides access to job metadata, database, HTTP client, and progress reporting.

### Fields

| Field | Type | Description |
|-------|------|-------------|
| `job_id` | `Uuid` | Unique job identifier |
| `job_type` | `String` | Job type name |
| `attempt` | `u32` | Current attempt number (1-based) |
| `max_attempts` | `u32` | Maximum allowed attempts |
| `auth` | `AuthContext` | Authentication context |

### Methods

| Method | Signature | Description |
|--------|-----------|-------------|
| `db()` | `&PgPool` | Get database connection pool |
| `http()` | `&Client` | Get HTTP client for external calls |
| `progress()` | `fn(u8, impl Into<String>) -> Result<()>` | Report progress (0-100%) with message |
| `heartbeat()` | `async fn() -> Result<()>` | Update last_heartbeat to prevent stale detection |
| `is_retry()` | `fn() -> bool` | True if attempt > 1 |
| `is_last_attempt()` | `fn() -> bool` | True if attempt >= max_attempts |

### Progress Tracking

Progress updates are persisted to the database (`progress_percent`, `progress_message` columns) and broadcast via WebSocket for real-time UI updates.

```rust
#[forge::job]
pub async fn export_users(ctx: &JobContext, input: ExportInput) -> Result<ExportOutput> {
    let _ = ctx.progress(0, "Initializing...");

    let users: Vec<User> = sqlx::query_as("SELECT * FROM users")
        .fetch_all(ctx.db())
        .await?;

    let total = users.len();
    for (i, user) in users.iter().enumerate() {
        // Process user...
        let pct = ((i + 1) * 100 / total) as u8;
        let _ = ctx.progress(pct, format!("Processing {} of {}", i + 1, total));
    }

    let _ = ctx.progress(100, "Export complete");
    Ok(ExportOutput { count: total })
}
```

### Heartbeat for Long Jobs

For jobs that may exceed the stale job threshold (default 5 minutes), call `heartbeat()` periodically:

```rust
#[forge::job]
#[timeout = "2h"]
pub async fn long_running_job(ctx: &JobContext, _input: ()) -> Result<()> {
    for i in 0..1000 {
        // Do work...
        if i % 100 == 0 {
            ctx.heartbeat().await?; // Prevent stale detection
        }
    }
    Ok(())
}
```

## Job Priority Levels

Jobs are processed in priority order (highest first), then by scheduled time (oldest first).

| Priority | Value | Use Case |
|----------|-------|----------|
| `critical` | 100 | Payment processing, security alerts |
| `high` | 75 | User-facing notifications |
| `normal` | 50 | Default for most jobs |
| `low` | 25 | Analytics, reporting |
| `background` | 0 | Cleanup, maintenance |

```rust
#[forge::job]
#[priority = "critical"]
pub async fn process_payment(ctx: &JobContext, input: PaymentInput) -> Result<()> {
    // Processed before lower-priority jobs
}
```

## Job Status Lifecycle

```
PENDING  -->  CLAIMED  -->  RUNNING  -->  COMPLETED
   ^                            |
   |                            v
   +---- RETRY <---- FAILED ----+
                        |
                        v
                   DEAD_LETTER
```

| Status | Description |
|--------|-------------|
| `pending` | Waiting in queue to be claimed |
| `claimed` | Assigned to a worker, not yet started |
| `running` | Currently executing |
| `completed` | Finished successfully |
| `retry` | Failed, scheduled for retry |
| `failed` | Failed permanently (exhausted retries) |
| `dead_letter` | Moved to dead letter queue for manual review |

## Dispatching Jobs

### From Mutations

```rust
#[forge::mutation]
pub async fn register_user(ctx: &MutationContext, email: String) -> Result<User> {
    let user = create_user_in_db(ctx.db(), &email).await?;

    // Dispatch background job
    ctx.dispatch_job("send_welcome_email", SendEmailInput {
        user_id: user.id,
        template: "welcome".into(),
    }).await?;

    Ok(user)
}
```

### From Actions

```rust
#[forge::action]
pub async fn webhook_handler(ctx: &ActionContext, event: WebhookEvent) -> Result<()> {
    // Acknowledge quickly, process later
    ctx.dispatch_job("process_webhook", event).await?;
    Ok(())
}
```

### Via Dashboard API

```bash
# Dispatch a job
curl -X POST http://localhost:8080/_api/jobs/export_users/dispatch \
  -H "Content-Type: application/json" \
  -d '{"args": {"format": "csv"}}'
```

## Registering Jobs

Jobs must be registered with the runtime before they can be dispatched:

```rust
use forge::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    let config = ForgeConfig::from_file("forge.toml")?;
    let mut builder = Forge::builder();

    // Register jobs
    builder.job_registry_mut().register::<SendEmailJob>();
    builder.job_registry_mut().register::<TranscodeVideoJob>();
    builder.job_registry_mut().register::<ExportUsersJob>();

    builder.config(config).build()?.run().await
}
```

## SKIP LOCKED Pattern

FORGE uses PostgreSQL's `FOR UPDATE SKIP LOCKED` for safe concurrent job claiming:

```sql
WITH claimable AS (
    SELECT id FROM forge_jobs
    WHERE status = 'pending'
      AND scheduled_at <= NOW()
      AND (worker_capability = ANY($2) OR worker_capability IS NULL)
    ORDER BY priority DESC, scheduled_at ASC
    LIMIT $3
    FOR UPDATE SKIP LOCKED
)
UPDATE forge_jobs
SET status = 'claimed', worker_id = $1, claimed_at = NOW(), attempts = attempts + 1
WHERE id IN (SELECT id FROM claimable)
RETURNING *
```

This ensures:
- No two workers claim the same job
- No blocking between workers
- Jobs are processed in priority order
- Worker capabilities are respected

## Worker Configuration

Workers poll the queue and execute claimed jobs:

```rust
pub struct WorkerConfig {
    pub id: Option<Uuid>,           // Auto-generated if not set
    pub capabilities: Vec<String>,  // e.g., ["general", "media"]
    pub max_concurrent: usize,      // Default: 10
    pub poll_interval: Duration,    // Default: 100ms
    pub batch_size: i32,            // Default: 10
    pub stale_threshold: Duration,  // Default: 5 minutes
}
```

Workers with matching capabilities process jobs marked with `#[worker_capability = "..."]`.

## Idempotency

Prevent duplicate job execution with idempotency keys:

```rust
#[forge::job]
#[idempotent(key = "input.payment_id")]
pub async fn process_payment(ctx: &JobContext, input: PaymentInput) -> Result<()> {
    // Only executes once per unique payment_id
    stripe::capture(&input.payment_id).await
}
```

When a job with an existing idempotency key is dispatched, the existing job ID is returned instead of creating a duplicate.

## Database Schema

```sql
CREATE TABLE forge_jobs (
    id UUID PRIMARY KEY,
    job_type VARCHAR(255) NOT NULL,
    input JSONB NOT NULL DEFAULT '{}',
    output JSONB,
    status VARCHAR(32) NOT NULL DEFAULT 'pending',
    priority INTEGER NOT NULL DEFAULT 50,
    attempts INTEGER NOT NULL DEFAULT 0,
    max_attempts INTEGER NOT NULL DEFAULT 3,
    last_error TEXT,
    progress_percent INTEGER DEFAULT 0,
    progress_message TEXT,
    worker_capability VARCHAR(255),
    worker_id UUID,
    idempotency_key VARCHAR(255),
    scheduled_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    created_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    claimed_at TIMESTAMPTZ,
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    failed_at TIMESTAMPTZ,
    last_heartbeat TIMESTAMPTZ
);

CREATE INDEX idx_forge_jobs_status_scheduled
    ON forge_jobs(status, scheduled_at)
    WHERE status = 'pending';

CREATE INDEX idx_forge_jobs_idempotency
    ON forge_jobs(idempotency_key)
    WHERE idempotency_key IS NOT NULL;
```

## Frontend Integration

### Job Trackers

Use the generated tracker pattern for real-time job progress:

```typescript
import { createExportUsersJob } from '$lib/forge';
import { onDestroy } from 'svelte';

const job = createExportUsersJob();
onDestroy(job.cleanup);

async function startExport() {
    await job.start({ format: 'csv' });
}
```

```svelte
{#if $job}
    <div class="progress">
        <div class="bar" style="width: {$job.progress_percent}%"></div>
        <span>{$job.progress_message}</span>
    </div>
    {#if $job.status === 'completed'}
        <p>Export complete!</p>
    {:else if $job.status === 'failed'}
        <p class="error">{$job.last_error}</p>
    {/if}
{/if}
```

### WebSocket Subscriptions

Job updates are pushed via WebSocket when the job table changes. The tracker handles subscription lifecycle automatically.

## Related Documentation

- [Workflows](WORKFLOWS.md) - Multi-step durable processes
- [Crons](CRONS.md) - Scheduled recurring tasks
- [Workers](../cluster/WORKERS.md) - Worker pool configuration

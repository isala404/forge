# Background Jobs

> *Reliable async work processing*

---

## Overview

Jobs are **durable background tasks** that:
- Survive server restarts
- Automatically retry on failure
- Can be scheduled for the future
- Are distributed across workers

Unlike actions (which are synchronous), jobs run asynchronously and return immediately to the caller.

---

## Defining Jobs

### Basic Job

```rust
// functions/jobs/notifications.rs

use forge::prelude::*;

#[forge::job]
pub async fn send_welcome_email(
    ctx: &JobContext,
    input: SendWelcomeEmailInput,
) -> Result<()> {
    let user = ctx.query(get_user, input.user_id).await?;
    
    email::send(EmailParams {
        to: user.email,
        subject: "Welcome to our app!",
        template: "welcome",
        data: json!({ "name": user.name }),
    }).await?;
    
    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
pub struct SendWelcomeEmailInput {
    pub user_id: Uuid,
}
```

### Job with Retry Configuration

```rust
#[forge::job]
#[retry(
    max_attempts = 5,
    backoff = "exponential",      // 1s, 2s, 4s, 8s, 16s
    max_backoff = "5m",           // Cap at 5 minutes
    on = ["NetworkError", "RateLimitError"]  // Only retry these
)]
pub async fn sync_external_service(ctx: &JobContext, input: SyncInput) -> Result<()> {
    external_api::sync(&input.data).await?;
    Ok(())
}
```

### Job with Timeout

```rust
#[forge::job]
#[timeout(minutes = 30)]
pub async fn process_video(ctx: &JobContext, input: VideoInput) -> Result<VideoOutput> {
    // Long-running video processing
    // Will be killed if exceeds 30 minutes
    ...
}
```

### Job with Priority

```rust
#[forge::job]
#[priority = "critical"]  // critical > high > normal > low
pub async fn process_payment(ctx: &JobContext, input: PaymentInput) -> Result<()> {
    // This job gets processed before lower-priority jobs
    ...
}

#[forge::job]
#[priority = "low"]
pub async fn generate_report(ctx: &JobContext, input: ReportInput) -> Result<()> {
    // This can wait
    ...
}
```

---

## Dispatching Jobs

### From Mutations

```rust
#[forge::mutation]
pub async fn register_user(ctx: &MutationContext, input: RegisterInput) -> Result<User> {
    let user = ctx.db.insert(User { ... }).await?;
    
    // Dispatch job (runs after mutation commits)
    ctx.dispatch_job(send_welcome_email, SendWelcomeEmailInput {
        user_id: user.id,
    }).await?;
    
    Ok(user)
}
```

### With Delay

```rust
#[forge::mutation]
pub async fn create_trial(ctx: &MutationContext, input: TrialInput) -> Result<Trial> {
    let trial = ctx.db.insert(Trial { ... }).await?;
    
    // Send reminder 7 days before trial ends
    ctx.dispatch_job_at(
        trial.ends_at - Duration::days(7),
        send_trial_ending_reminder,
        TrialReminderInput { trial_id: trial.id },
    ).await?;
    
    // Send expiry notice when trial ends
    ctx.dispatch_job_at(
        trial.ends_at,
        send_trial_expired_notice,
        TrialExpiredInput { trial_id: trial.id },
    ).await?;
    
    Ok(trial)
}
```

### From Actions

```rust
#[forge::action]
pub async fn webhook_handler(ctx: &ActionContext, event: WebhookEvent) -> Result<()> {
    // Acknowledge quickly, process later
    ctx.dispatch_job(process_webhook, event).await?;
    Ok(())
}
```

---

## Job Context

Jobs have a rich context for interacting with the system:

```rust
#[forge::job]
pub async fn complex_job(ctx: &JobContext, input: ComplexInput) -> Result<Output> {
    // Progress reporting
    ctx.progress(0, "Starting...").await?;
    
    // Query data
    let data = ctx.query(get_data, input.data_id).await?;
    
    ctx.progress(25, "Data loaded").await?;
    
    // Mutate data
    ctx.mutate(update_status, UpdateStatusInput {
        id: input.id,
        status: Status::Processing,
    }).await?;
    
    ctx.progress(50, "Processing...").await?;
    
    // Call external service
    let result = external_api::process(&data).await?;
    
    ctx.progress(75, "Saving results...").await?;
    
    // Save results
    ctx.mutate(save_results, SaveResultsInput {
        id: input.id,
        result: result.clone(),
    }).await?;
    
    ctx.progress(100, "Complete!").await?;
    
    Ok(Output { result })
}
```

### Context Methods

| Method | Description |
|--------|-------------|
| `ctx.query(...)` | Execute a query |
| `ctx.mutate(...)` | Execute a mutation |
| `ctx.progress(pct, msg)` | Report progress (0-100) |
| `ctx.heartbeat()` | Keep job alive (for very long jobs) |
| `ctx.dispatch_job(...)` | Dispatch another job |
| `ctx.log.info/warn/error(...)` | Structured logging |

---

## Worker Capabilities

Jobs can require specific worker capabilities:

```rust
// Requires a worker with "media" capability
#[forge::job]
#[worker_capability = "media"]
#[resources(cpu = 4, memory = "8Gi")]
pub async fn transcode_video(ctx: &JobContext, input: TranscodeInput) -> Result<TranscodeOutput> {
    ffmpeg::transcode(&input.source_path, &input.output_path, input.options).await
}

// Requires a GPU worker
#[forge::job]
#[worker_capability = "ml"]
#[resources(gpu = 1, memory = "16Gi")]
pub async fn generate_embeddings(ctx: &JobContext, input: EmbeddingInput) -> Result<Vec<f32>> {
    ml_model::generate_embeddings(&input.text).await
}
```

Workers declare their capabilities in configuration:

```toml
# Worker node configuration
[node]
roles = ["worker"]
worker_capabilities = ["media"]  # or ["ml"], ["general"]
```

→ See [Workers](../cluster/WORKERS.md) for worker pool configuration.

---

## Job Lifecycle

```
┌─────────┐
│ PENDING │ ◄─── Job dispatched
└────┬────┘
     │
     │  Worker claims job (SKIP LOCKED)
     ▼
┌─────────┐
│ CLAIMED │ ◄─── Assigned to specific worker
└────┬────┘
     │
     │  Worker starts execution
     ▼
┌─────────┐
│ RUNNING │ ◄─── Heartbeat updates status
└────┬────┘
     │
     ├─── Success ───────────────────┐
     │                               │
     │                               ▼
     │                        ┌───────────┐
     │                        │ COMPLETED │
     │                        └───────────┘
     │
     ├─── Failure (retries left) ────┐
     │                               │
     │                               ▼
     │                        ┌─────────┐
     │                        │  RETRY  │ ──► Wait backoff ──► PENDING
     │                        └─────────┘
     │
     └─── Failure (no retries) ──────┐
                                     │
                                     ▼
                              ┌─────────┐
                              │ FAILED  │
                              └────┬────┘
                                   │
                                   │  After configured time
                                   ▼
                              ┌─────────────┐
                              │ DEAD LETTER │
                              └─────────────┘
```

---

## Dead Letter Queue

Failed jobs that exhaust retries go to the dead letter queue:

```rust
#[forge::job]
#[dead_letter(retain = "7d")]  // Keep in DLQ for 7 days
pub async fn critical_job(ctx: &JobContext, input: CriticalInput) -> Result<()> {
    // If this fails after all retries, it goes to DLQ
    ...
}
```

### Monitoring DLQ

```sql
-- View dead letter jobs
SELECT * FROM forge_jobs 
WHERE status = 'dead_letter'
ORDER BY failed_at DESC;
```

### Retrying Dead Letter Jobs

From the dashboard or CLI:

```bash
# Retry a specific dead letter job
forge jobs retry <job_id>

# Retry all dead letter jobs of a type
forge jobs retry-all --type send_email

# Move to permanent failure (give up)
forge jobs discard <job_id>
```

### Programmatic DLQ Handling

```rust
#[forge::cron("0 * * * *")]  // Every hour
pub async fn process_dead_letters(ctx: &CronContext) -> Result<()> {
    let dead_jobs = ctx.jobs.get_dead_letter_jobs(100).await?;
    
    for job in dead_jobs {
        // Decide what to do
        if job.job_type == "send_email" && job.attempts < 10 {
            // Retry with more attempts
            ctx.jobs.retry(job.id).await?;
        } else if job.job_type == "sync_external" {
            // Alert ops team
            ctx.dispatch_job(alert_ops, AlertInput {
                message: format!("Job {} in DLQ", job.id),
            }).await?;
        }
    }
    
    Ok(())
}
```

---

## Preventing Double Execution

### Idempotency Keys

```rust
#[forge::job]
#[idempotent(key = "input.payment_id")]  // Dedupe by payment ID
pub async fn process_payment(ctx: &JobContext, input: PaymentInput) -> Result<()> {
    // Even if this job is accidentally dispatched twice,
    // it will only execute once per payment_id
    stripe::capture_payment(&input.payment_id).await
}
```

### Custom Idempotency

```rust
#[forge::job]
pub async fn idempotent_job(ctx: &JobContext, input: Input) -> Result<()> {
    // Check if already processed
    let idempotency_key = format!("job:{}:{}", input.type, input.id);
    
    if ctx.db.query::<IdempotencyRecord>()
        .filter(|r| r.key == idempotency_key)
        .exists()
        .await?
    {
        ctx.log.info("Job already processed, skipping");
        return Ok(());
    }
    
    // Process job
    do_work(&input).await?;
    
    // Record completion
    ctx.mutate(record_idempotency, RecordInput {
        key: idempotency_key,
        processed_at: Timestamp::now(),
    }).await?;
    
    Ok(())
}
```

---

## Job Batching

For processing many items efficiently:

```rust
#[forge::job]
pub async fn process_batch(ctx: &JobContext, input: BatchInput) -> Result<BatchOutput> {
    let items = ctx.query(get_items, GetItemsInput {
        batch_id: input.batch_id,
    }).await?;
    
    let mut results = Vec::new();
    let total = items.len();
    
    for (i, item) in items.into_iter().enumerate() {
        // Process each item
        let result = process_item(&item).await?;
        results.push(result);
        
        // Report progress
        ctx.progress((i + 1) * 100 / total, format!("Processed {}/{}", i + 1, total)).await?;
    }
    
    // Save results
    ctx.mutate(save_batch_results, SaveBatchInput {
        batch_id: input.batch_id,
        results: results.clone(),
    }).await?;
    
    Ok(BatchOutput { results })
}
```

### Fan-Out / Fan-In

```rust
#[forge::job]
pub async fn export_all_users(ctx: &JobContext, input: ExportInput) -> Result<ExportOutput> {
    let user_count = ctx.query(get_user_count, ()).await?;
    let chunk_size = 1000;
    let chunks = (user_count + chunk_size - 1) / chunk_size;
    
    // Fan out: dispatch child jobs for each chunk
    let child_jobs = (0..chunks)
        .map(|i| {
            ctx.dispatch_child(export_user_chunk, ExportChunkInput {
                offset: i * chunk_size,
                limit: chunk_size,
            })
        })
        .collect::<Vec<_>>();
    
    let child_ids = futures::future::try_join_all(child_jobs).await?;
    
    // Fan in: wait for all children
    let results = ctx.wait_for_jobs(child_ids).await?;
    
    // Combine results
    let final_file = combine_export_chunks(results).await?;
    
    Ok(ExportOutput { file_url: final_file.url })
}
```

---

## Monitoring Jobs

### Metrics

FORGE automatically tracks:

| Metric | Description |
|--------|-------------|
| `forge_jobs_dispatched_total` | Jobs dispatched by type |
| `forge_jobs_completed_total` | Jobs completed by type |
| `forge_jobs_failed_total` | Jobs failed by type |
| `forge_jobs_duration_seconds` | Job execution duration |
| `forge_jobs_queue_depth` | Current queue depth |
| `forge_jobs_worker_utilization` | Worker busy percentage |

### Dashboard

The built-in dashboard shows:
- Real-time job queue depth
- Job throughput graphs
- Failed job list
- Dead letter queue
- Worker status

### Alerts

```toml
# forge.toml

[[alerts]]
name = "high_job_queue"
condition = "forge_jobs_queue_depth > 1000"
for = "5m"
severity = "warning"
notify = ["slack:#ops"]

[[alerts]]
name = "job_failures"
condition = "rate(forge_jobs_failed_total[5m]) > 10"
severity = "critical"
notify = ["pagerduty"]
```

---

## Best Practices

### 1. Keep Jobs Focused

```rust
// ❌ Job does too much
#[forge::job]
pub async fn process_order(ctx: &JobContext, input: OrderInput) -> Result<()> {
    charge_payment(...).await?;
    update_inventory(...).await?;
    send_confirmation_email(...).await?;
    notify_warehouse(...).await?;
    update_analytics(...).await?;
}

// ✅ Dispatch separate jobs
#[forge::job]
pub async fn process_order(ctx: &JobContext, input: OrderInput) -> Result<()> {
    charge_payment(&input).await?;
    
    ctx.dispatch_job(update_inventory, input.items.clone()).await?;
    ctx.dispatch_job(send_confirmation_email, input.email.clone()).await?;
    ctx.dispatch_job(notify_warehouse, input.clone()).await?;
    ctx.dispatch_job(update_analytics, input.clone()).await?;
    
    Ok(())
}
```

### 2. Make Jobs Idempotent

```rust
// ❌ Not idempotent - might charge twice
#[forge::job]
pub async fn charge_payment(ctx: &JobContext, input: ChargeInput) -> Result<()> {
    stripe::charge(&input).await?;
}

// ✅ Idempotent - safe to retry
#[forge::job]
#[idempotent(key = "input.order_id")]
pub async fn charge_payment(ctx: &JobContext, input: ChargeInput) -> Result<()> {
    stripe::charge(&input).await?;
}
```

### 3. Use Appropriate Timeouts

```rust
// ❌ No timeout - could run forever
#[forge::job]
pub async fn risky_job(ctx: &JobContext, input: Input) -> Result<()> {
    call_unreliable_api().await?;
}

// ✅ Bounded execution time
#[forge::job]
#[timeout(minutes = 5)]
pub async fn safe_job(ctx: &JobContext, input: Input) -> Result<()> {
    call_unreliable_api().await?;
}
```

---

## Related Documentation

- [Job Queue](../database/JOB_QUEUE.md) — PostgreSQL implementation
- [Workers](../cluster/WORKERS.md) — Worker configuration
- [Crons](CRONS.md) — Scheduled jobs
- [Workflows](WORKFLOWS.md) — Multi-step processes

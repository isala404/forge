# Workers

> *Specialized processing pools*

---

## Overview

Workers are nodes (or roles within nodes) that process background jobs. FORGE supports **worker specialization**—different workers for different workloads:

- **General workers**: API calls, emails, data transformations
- **Media workers**: Video transcoding, image processing
- **ML workers**: GPU inference, embeddings
- **Document workers**: PDF generation, Excel export

---

## Worker Architecture

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                      WORKER ARCHITECTURE                                     │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│   Scheduler (Leader)                                                         │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │  Job Router                                                          │   │
│   │                                                                      │   │
│   │  Job: transcode_video          Job: send_email                      │   │
│   │  capability: "media"           capability: "general"                │   │
│   │        │                              │                              │   │
│   │        ▼                              ▼                              │   │
│   │  ┌──────────────────┐         ┌──────────────────┐                  │   │
│   │  │ Find workers with │         │ Find workers with │                 │   │
│   │  │ capability=media  │         │ capability=general│                 │   │
│   │  └─────────┬────────┘         └─────────┬────────┘                  │   │
│   │            │                            │                            │   │
│   └────────────┼────────────────────────────┼────────────────────────────┘   │
│                │                            │                                │
│                ▼                            ▼                                │
│   ┌─────────────────────┐       ┌─────────────────────┐                     │
│   │  Media Worker Pool  │       │  General Worker Pool│                     │
│   │                     │       │                     │                     │
│   │  ┌───────┐ ┌──────┐│       │  ┌───────┐ ┌──────┐│                     │
│   │  │ W1    │ │ W2   ││       │  │ W3    │ │ W4   ││                     │
│   │  │ FFmpeg│ │FFmpeg││       │  │       │ │      ││                     │
│   │  │ 4 CPU │ │4 CPU ││       │  │ 2 CPU │ │ 2 CPU││                     │
│   │  └───────┘ └──────┘│       │  └───────┘ └──────┘│                     │
│   └─────────────────────┘       └─────────────────────┘                     │
│                                                                              │
│   ┌─────────────────────┐                                                    │
│   │   ML Worker Pool    │                                                    │
│   │                     │                                                    │
│   │  ┌───────┐ ┌──────┐│                                                    │
│   │  │ W5    │ │ W6   ││                                                    │
│   │  │ GPU   │ │ GPU  ││                                                    │
│   │  │ PyTorch│ │TensorRT│                                                  │
│   │  └───────┘ └──────┘│                                                    │
│   └─────────────────────┘                                                    │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Worker Configuration

### Basic Configuration

```toml
# forge.toml - General purpose node

[node]
roles = ["worker"]
worker_capabilities = ["general"]

[worker]
max_concurrent_jobs = 50
job_timeout = "1h"
poll_interval = "100ms"
```

### Specialized Media Worker

```toml
# forge.toml - Media processing node

[node]
roles = ["worker"]
worker_capabilities = ["media", "general"]  # Can do both

[worker]
max_concurrent_jobs = 10  # Fewer, but heavier
job_timeout = "2h"        # Video can take a while

[worker.resources]
cpu_limit = 8
memory_limit = "16Gi"
```

### GPU/ML Worker

```toml
# forge.toml - ML inference node

[node]
roles = ["worker"]
worker_capabilities = ["ml"]

[worker]
max_concurrent_jobs = 4  # GPU bound
job_timeout = "30m"

[worker.resources]
gpu = 1
memory_limit = "32Gi"
```

---

## Job Routing

### Declaring Worker Requirements

```rust
// Jobs declare what capability they need

#[forge::job]
#[worker_capability = "general"]  // Default
pub async fn send_email(ctx: &JobContext, input: EmailInput) -> Result<()> {
    email::send(&input).await
}

#[forge::job]
#[worker_capability = "media"]
#[resources(cpu = 4, memory = "8Gi")]
pub async fn transcode_video(ctx: &JobContext, input: TranscodeInput) -> Result<TranscodeOutput> {
    ffmpeg::transcode(&input).await
}

#[forge::job]
#[worker_capability = "ml"]
#[resources(gpu = 1, memory = "16Gi")]
pub async fn generate_embeddings(ctx: &JobContext, input: EmbeddingsInput) -> Result<Vec<f32>> {
    model::embed(&input.text).await
}
```

### Routing Algorithm

```rust
impl Scheduler {
    async fn route_job(&self, job: &Job) -> Result<NodeId> {
        // 1. Get required capability
        let required_cap = job.metadata.worker_capability
            .unwrap_or("general".to_string());
        
        // 2. Get resource requirements
        let resources = job.metadata.resources.unwrap_or_default();
        
        // 3. Find eligible workers
        let workers: Vec<_> = self.cluster.nodes()
            .filter(|n| n.roles.contains(&Role::Worker))
            .filter(|n| n.worker_capabilities.contains(&required_cap))
            .filter(|n| n.status == NodeStatus::Active)
            .filter(|n| n.can_satisfy_resources(&resources))
            .collect();
        
        if workers.is_empty() {
            return Err(Error::NoEligibleWorker {
                capability: required_cap,
                resources,
            });
        }
        
        // 4. Select best worker (least loaded)
        let best = workers.into_iter()
            .min_by_key(|n| (n.current_jobs * 100) / n.max_concurrent_jobs)
            .unwrap();
        
        Ok(best.id)
    }
}
```

---

## Job Claiming

Workers use **SKIP LOCKED** for efficient job claiming:

```rust
impl Worker {
    async fn claim_jobs(&self) -> Result<Vec<Job>> {
        // Claim up to N jobs that match our capabilities
        let jobs = sqlx::query_as::<_, Job>(r#"
            UPDATE forge_jobs
            SET 
                status = 'claimed',
                worker_id = $1,
                claimed_at = NOW()
            WHERE id IN (
                SELECT id FROM forge_jobs
                WHERE status = 'pending'
                AND (worker_capability = ANY($2) OR worker_capability IS NULL)
                ORDER BY priority DESC, created_at ASC
                LIMIT $3
                FOR UPDATE SKIP LOCKED
            )
            RETURNING *
        "#)
        .bind(&self.node_id)
        .bind(&self.capabilities)
        .bind(self.max_concurrent - self.current_jobs)
        .fetch_all(&self.db)
        .await?;
        
        Ok(jobs)
    }
}
```

---

## Worker Lifecycle

### Startup

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                     WORKER STARTUP                                           │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│   1. Register with cluster                                                   │
│      - Declare capabilities: ["media", "general"]                           │
│      - Declare resources: {cpu: 8, memory: "16Gi"}                          │
│                                                                              │
│   2. Start job polling loop                                                  │
│      - Poll interval: 100ms                                                  │
│      - Batch size: max_concurrent - current                                  │
│                                                                              │
│   3. Listen for NOTIFY                                                       │
│      - Channel: forge_jobs_available                                         │
│      - Wake up immediately when jobs arrive                                  │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Processing Loop

```rust
impl Worker {
    async fn run(&self) {
        loop {
            // Wait for jobs (polling + NOTIFY)
            let jobs = tokio::select! {
                _ = self.job_notify.recv() => {
                    self.claim_jobs().await?
                }
                _ = tokio::time::sleep(self.poll_interval) => {
                    self.claim_jobs().await?
                }
            };
            
            // Process claimed jobs concurrently
            for job in jobs {
                let worker = self.clone();
                tokio::spawn(async move {
                    worker.process_job(job).await;
                });
            }
        }
    }
    
    async fn process_job(&self, job: Job) {
        // Update status
        self.update_job_status(&job.id, "running").await;
        
        // Execute with timeout
        let result = tokio::time::timeout(
            job.timeout,
            self.execute_job(&job)
        ).await;
        
        match result {
            Ok(Ok(output)) => {
                self.complete_job(&job.id, output).await;
            }
            Ok(Err(e)) => {
                self.fail_job(&job.id, e).await;
            }
            Err(_) => {
                self.timeout_job(&job.id).await;
            }
        }
    }
}
```

### Graceful Shutdown

```rust
async fn graceful_shutdown(&self) {
    // 1. Stop claiming new jobs
    self.stop_claiming();
    
    // 2. Wait for current jobs (with timeout)
    let timeout = Duration::seconds(30);
    let _ = tokio::time::timeout(
        timeout,
        self.wait_for_current_jobs()
    ).await;
    
    // 3. Release uncompleted jobs back to queue
    sqlx::query(r#"
        UPDATE forge_jobs 
        SET status = 'pending', worker_id = NULL, claimed_at = NULL
        WHERE worker_id = $1 AND status IN ('claimed', 'running')
    "#)
    .bind(&self.node_id)
    .execute(&self.db)
    .await?;
    
    info!("Worker shutdown complete");
}
```

---

## Failure Handling

### Job Failure and Retry

```rust
async fn fail_job(&self, job_id: &Uuid, error: Error) {
    let job = self.get_job(job_id).await?;
    
    if job.attempts < job.max_retries {
        // Schedule retry with backoff
        let backoff = self.calculate_backoff(job.attempts);
        
        sqlx::query(r#"
            UPDATE forge_jobs
            SET 
                status = 'pending',
                worker_id = NULL,
                attempts = attempts + 1,
                last_error = $2,
                scheduled_at = NOW() + $3
            WHERE id = $1
        "#)
        .bind(job_id)
        .bind(&error.to_string())
        .bind(&backoff)
        .execute(&self.db)
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
        .bind(job_id)
        .bind(&error.to_string())
        .execute(&self.db)
        .await?;
    }
}
```

### Worker Failure (Stale Jobs)

The scheduler periodically cleans up stale jobs:

```rust
impl Scheduler {
    async fn cleanup_stale_jobs(&self) {
        // Jobs claimed but not completed within timeout
        let stale_threshold = Duration::minutes(5);
        
        let stale_count = sqlx::query(r#"
            UPDATE forge_jobs
            SET 
                status = 'pending',
                worker_id = NULL,
                claimed_at = NULL
            WHERE status IN ('claimed', 'running')
            AND claimed_at < NOW() - $1
        "#)
        .bind(&stale_threshold)
        .execute(&self.db)
        .await?
        .rows_affected();
        
        if stale_count > 0 {
            warn!("Released {} stale jobs", stale_count);
        }
    }
}
```

---

## Scaling Workers

### Kubernetes Auto-Scaling

```yaml
# Scale based on queue depth
apiVersion: autoscaling/v2
kind: HorizontalPodAutoscaler
metadata:
  name: forge-workers
spec:
  scaleTargetRef:
    apiVersion: apps/v1
    kind: Deployment
    name: forge-workers
  minReplicas: 2
  maxReplicas: 50
  metrics:
  - type: External
    external:
      metric:
        name: forge_jobs_pending
        selector:
          matchLabels:
            capability: general
      target:
        type: AverageValue
        averageValue: 10  # 10 pending jobs per worker
```

### Separate Deployments by Capability

```yaml
# General workers
apiVersion: apps/v1
kind: Deployment
metadata:
  name: forge-workers-general
spec:
  replicas: 5
  template:
    spec:
      containers:
      - name: forge
        env:
        - name: FORGE_WORKER_CAPABILITIES
          value: "general"
---
# Media workers (more resources)
apiVersion: apps/v1
kind: Deployment
metadata:
  name: forge-workers-media
spec:
  replicas: 2
  template:
    spec:
      containers:
      - name: forge
        env:
        - name: FORGE_WORKER_CAPABILITIES
          value: "media"
        resources:
          requests:
            cpu: "4"
            memory: "8Gi"
---
# ML workers (GPU)
apiVersion: apps/v1
kind: Deployment
metadata:
  name: forge-workers-ml
spec:
  replicas: 2
  template:
    spec:
      containers:
      - name: forge
        env:
        - name: FORGE_WORKER_CAPABILITIES
          value: "ml"
        resources:
          limits:
            nvidia.com/gpu: 1
```

---

## Monitoring

### Metrics

| Metric | Description |
|--------|-------------|
| `forge_worker_jobs_claimed_total` | Jobs claimed by capability |
| `forge_worker_jobs_completed_total` | Jobs completed |
| `forge_worker_jobs_failed_total` | Jobs failed |
| `forge_worker_job_duration_seconds` | Job execution time |
| `forge_worker_utilization` | Worker busy percentage |
| `forge_worker_queue_depth` | Pending jobs by capability |

### Dashboard

The dashboard shows:
- Worker pool status
- Job throughput by capability
- Queue depth graphs
- Failed job list
- Worker utilization

---

## Self-DDoS Prevention

Prevent workers from overwhelming external services:

```rust
#[forge::job]
#[worker_capability = "general"]
#[rate_limit(
    key = "stripe_api",      // Shared limit
    requests = 100,          // 100 requests
    per = "second"           // per second
)]
pub async fn sync_stripe(ctx: &JobContext, input: SyncInput) -> Result<()> {
    // Rate limit applied automatically
    stripe::sync(&input).await
}
```

### Global Rate Limiting

```toml
# forge.toml

[worker.rate_limits]
# Limit all workers combined
stripe_api = { requests = 100, per = "second" }
openai_api = { requests = 60, per = "minute" }
sendgrid = { requests = 100, per = "second" }
```

---

## Related Documentation

- [Jobs](../core/JOBS.md) — Job definitions
- [Job Queue](../database/JOB_QUEUE.md) — PostgreSQL queue
- [Kubernetes](../deployment/KUBERNETES.md) — K8s deployment

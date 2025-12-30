# Workflows

Multi-step durable processes with compensation (saga pattern).

---

## Overview

Workflows are durable, multi-step processes that:

- Survive server restarts via database persistence
- Track individual step status and results
- Support compensation (rollback) on failure
- Execute in the background asynchronously
- Have configurable timeouts and versioning

Use workflows when orchestrating multiple operations that must complete together, especially when external services are involved and rollback logic is needed.

---

## Defining Workflows

Use the `#[forge::workflow]` proc macro to define a workflow:

```rust
use forge::prelude::*;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize)]
pub struct OnboardingInput {
    pub user_id: String,
    pub email: String,
}

#[derive(Debug, Serialize)]
pub struct OnboardingOutput {
    pub verified: bool,
    pub token: String,
}

#[forge::workflow]
#[version = 1]
#[timeout = "24h"]
pub async fn user_onboarding(
    ctx: &WorkflowContext,
    input: OnboardingInput,
) -> Result<OnboardingOutput> {
    // Workflow implementation
    Ok(OnboardingOutput {
        verified: true,
        token: "abc123".to_string(),
    })
}
```

### Macro Attributes

| Attribute | Type | Default | Description |
|-----------|------|---------|-------------|
| `version` | `u32` | `1` | Workflow version for schema evolution |
| `timeout` | `String` | `"24h"` | Maximum execution time (e.g., `"30s"`, `"5m"`, `"1h"`, `"7d"`) |
| `deprecated` | flag | `false` | Mark workflow as deprecated |

The macro generates a struct (e.g., `UserOnboardingWorkflow`) implementing the `ForgeWorkflow` trait.

---

## WorkflowContext

The `WorkflowContext` provides access to workflow state, database, and step management.

### Fields

```rust
pub struct WorkflowContext {
    pub run_id: Uuid,           // Unique workflow run identifier
    pub workflow_name: String,  // Registered workflow name
    pub version: u32,           // Workflow version
    pub started_at: DateTime<Utc>,
    pub auth: AuthContext,      // Authentication context
    // ... private fields
}
```

### Methods

| Method | Description |
|--------|-------------|
| `run_id` | Unique identifier for this workflow execution |
| `workflow_name` | Name of the workflow being executed |
| `version` | Workflow version number |
| `workflow_time()` | Deterministic time (consistent across replays) |
| `db()` | Get database pool reference |
| `http()` | Get HTTP client reference |
| `elapsed()` | Time since workflow started |

### Step Management Methods

| Method | Description |
|--------|-------------|
| `step(name, fn)` | Create a fluent step runner |
| `is_step_completed(name)` | Check if step already completed |
| `get_step_result::<T>(name)` | Get cached result of completed step |
| `record_step_start(name)` | Mark step as running |
| `record_step_complete(name, result)` | Mark step as completed with result |
| `record_step_failure(name, error)` | Mark step as failed |
| `run_compensation()` | Execute all compensation handlers in reverse |

---

## Step Execution APIs

FORGE provides two APIs for defining workflow steps: the fluent API for common cases and the low-level API for full control.

### Fluent Step API

The fluent API provides a chainable builder pattern:

```rust
ctx.step("step_name", || async { ... })
    .timeout(Duration::from_secs(30))
    .compensate(|result| async move { ... })
    .optional()
    .run()
    .await?
```

#### StepRunner Methods

| Method | Description |
|--------|-------------|
| `.timeout(duration)` | Set step timeout |
| `.compensate(fn)` | Register compensation handler (rollback on later failure) |
| `.optional()` | Failure won't trigger workflow compensation |
| `.run()` | Execute the step and return result |

#### Fluent API Example

```rust
use std::time::Duration;

#[forge::workflow]
pub async fn payment_workflow(
    ctx: &WorkflowContext,
    input: PaymentInput,
) -> Result<PaymentOutput> {
    // Step 1: Reserve inventory (with compensation)
    let reservation = ctx.step("reserve_inventory", || async {
        reserve_items(&input.items).await
    })
    .compensate(|res| async move {
        release_reservation(&res.reservation_id).await
    })
    .run()
    .await?;

    // Step 2: Charge card (with timeout and compensation)
    let charge = ctx.step("charge_card", || async {
        charge_credit_card(&input.card_token).await
    })
    .timeout(Duration::from_secs(30))
    .compensate(|charge| async move {
        refund_charge(&charge.charge_id).await
    })
    .run()
    .await?;

    // Step 3: Send confirmation (optional - failure won't trigger compensation)
    ctx.step("send_confirmation", || async {
        send_order_email(&input.email).await
    })
    .optional()
    .run()
    .await?;

    Ok(PaymentOutput {
        order_id: reservation.order_id,
        charge_id: charge.charge_id,
    })
}
```

### Low-Level Step API

For workflows requiring custom logic (manual retry, conditional steps, complex branching), use the low-level API:

```rust
#[forge::workflow]
pub async fn verification_workflow(
    ctx: &WorkflowContext,
    input: VerificationInput,
) -> Result<VerificationOutput> {
    // Step 1: Generate token (with resume support)
    let token = if ctx.is_step_completed("generate_token") {
        // Workflow was resumed - use cached result
        ctx.get_step_result::<String>("generate_token")
            .unwrap_or_else(|| format!("verify_{}", Uuid::new_v4()))
    } else {
        ctx.record_step_start("generate_token");

        let token = format!("verify_{}", Uuid::new_v4());

        ctx.record_step_complete("generate_token", serde_json::json!(token));
        token
    };

    // Step 2: Store token (with manual retry)
    if !ctx.is_step_completed("store_token") {
        ctx.record_step_start("store_token");

        let mut attempts = 0;
        loop {
            match store_token_in_db(ctx.db(), &token).await {
                Ok(_) => break,
                Err(e) if attempts < 3 => {
                    attempts += 1;
                    tokio::time::sleep(Duration::from_secs(1)).await;
                }
                Err(e) => {
                    ctx.record_step_failure("store_token", e.to_string());
                    return Err(e);
                }
            }
        }

        ctx.record_step_complete("store_token", serde_json::json!({"stored": true}));
    }

    // Step 3: Send email
    if !ctx.is_step_completed("send_email") {
        ctx.record_step_start("send_email");

        send_verification_email(&input.email, &token).await?;

        ctx.record_step_complete("send_email", serde_json::json!({"sent": true}));
    }

    Ok(VerificationOutput { token, verified: false })
}
```

### When to Use Each API

| Use Case | Recommended API |
|----------|-----------------|
| Simple sequential steps | Fluent API |
| Steps with compensation | Fluent API |
| Steps with timeout | Fluent API |
| Optional/non-critical steps | Fluent API |
| Manual retry logic | Low-level API |
| Complex branching | Low-level API |
| Custom step state handling | Low-level API |
| Mixed checkpoint/non-checkpoint logic | Low-level API |

---

## Step Status and Persistence

Each step is persisted to the `forge_workflow_steps` table:

### StepStatus Enum

```rust
pub enum StepStatus {
    Pending,     // Not started
    Running,     // Currently executing
    Completed,   // Finished successfully
    Failed,      // Execution failed
    Compensated, // Rollback completed
    Skipped,     // Skipped (e.g., conditional step not taken)
}
```

### Database Schema

```sql
CREATE TABLE forge_workflow_steps (
    id UUID PRIMARY KEY,
    workflow_run_id UUID NOT NULL REFERENCES forge_workflow_runs(id),
    step_name VARCHAR(255) NOT NULL,
    status VARCHAR(32) NOT NULL DEFAULT 'pending',
    input JSONB,
    result JSONB,
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ,
    error TEXT,
    UNIQUE(workflow_run_id, step_name)
);
```

Steps are persisted asynchronously via `tokio::spawn` to keep the workflow execution non-blocking.

---

## Compensation (Saga Pattern)

Compensation enables rollback when a step fails. Handlers are registered during successful step execution and run in reverse order on failure.

### How Compensation Works

1. Each step can register a compensation handler
2. Handlers are stored in-memory in `WorkflowContext`
3. On failure, `run_compensation()` executes handlers in reverse order
4. Step status is updated to `Compensated` after successful rollback

```
Step 1: create_user    -> Success -> register compensation(delete_user)
Step 2: setup_stripe   -> Success -> register compensation(delete_customer)
Step 3: provision_db   -> FAILURE!

Compensation triggered (reverse order):
  Compensate step 2: delete_customer() -> Success
  Compensate step 1: delete_user()     -> Success

Result: Clean rollback
```

### Triggering Compensation

Compensation is triggered via `WorkflowExecutor::cancel()`:

```rust
// From dashboard or admin endpoint
workflow_executor.cancel(run_id).await?;
```

The cancel operation:
1. Sets workflow status to `Compensating`
2. Retrieves completed steps with their results
3. Runs compensation handlers in reverse order
4. Sets workflow status to `Compensated`

### Compensation Handler Signature

```rust
// Handler receives the step's result value
.compensate(|step_result: T| async move {
    // Rollback logic
    Ok(())
})
```

---

## Workflow Versioning

Workflows support versioning for schema evolution:

```rust
#[forge::workflow]
#[version = 2]
pub async fn user_onboarding(
    ctx: &WorkflowContext,
    input: OnboardingInputV2,
) -> Result<OnboardingOutputV2> {
    // V2 implementation
}
```

### WorkflowInfo

The macro generates `WorkflowInfo` metadata:

```rust
pub struct WorkflowInfo {
    pub name: &'static str,
    pub version: u32,
    pub timeout: Duration,
    pub deprecated: bool,
}
```

### Version Lookup

```rust
// Get workflow by name (latest version)
registry.get("user_onboarding")?;

// Get specific version
registry.get_version("user_onboarding", 1)?;
```

### Deprecation

Mark old workflow versions as deprecated:

```rust
#[forge::workflow]
#[version = 1]
#[deprecated]
pub async fn user_onboarding_v1(...) { ... }
```

---

## Workflow Lifecycle

### WorkflowStatus Enum

```rust
pub enum WorkflowStatus {
    Created,      // Workflow created, not started
    Running,      // Actively executing steps
    Waiting,      // Waiting for external event (not yet implemented)
    Completed,    // All steps finished successfully
    Compensating, // Running compensation handlers
    Compensated,  // Compensation completed
    Failed,       // Failed (compensation also failed or not available)
}
```

### Lifecycle Diagram

```
         ┌─────────┐
         │ Created │
         └────┬────┘
              │ start()
              ▼
         ┌─────────┐
         │ Running │◄───────────────────────┐
         └────┬────┘                        │
              │                             │
    ┌─────────┼─────────────┐               │ resume()
    │         │             │               │
    ▼         ▼             ▼               │
┌───────┐ ┌──────┐    ┌───────────┐         │
│Success│ │Failed│    │  Waiting  │─────────┘
└───┬───┘ └───┬──┘    └───────────┘
    │         │
    ▼         │ cancel()
┌─────────┐   ▼
│Completed│ ┌────────────┐
└─────────┘ │Compensating│
            └─────┬──────┘
                  │
        ┌─────────┴─────────┐
        ▼                   ▼
┌───────────┐          ┌────────┐
│Compensated│          │ Failed │
└───────────┘          └────────┘
```

---

## Registering and Starting Workflows

### Registration

Register workflows in `main.rs`:

```rust
use forge::prelude::*;

mod functions;

#[tokio::main]
async fn main() -> Result<()> {
    let mut builder = Forge::builder();

    // Register workflow
    builder.workflow_registry_mut().register::<functions::UserOnboardingWorkflow>();

    builder
        .config(ForgeConfig::from_env()?)
        .build()?
        .run()
        .await
}
```

### Starting from Functions

Start workflows from mutations or actions:

```rust
#[forge::mutation]
pub async fn register_user(
    ctx: &MutationContext,
    input: RegisterInput,
) -> Result<WorkflowHandle> {
    let run_id = ctx.start_workflow(
        "user_onboarding",
        OnboardingInput {
            user_id: input.user_id,
            email: input.email,
        },
    ).await?;

    Ok(WorkflowHandle { run_id })
}
```

### Starting via Dashboard API

```http
POST /_api/workflows/user_onboarding/start
Content-Type: application/json

{
  "input": {
    "user_id": "123",
    "email": "user@example.com"
  }
}
```

---

## Workflow State Persistence

### WorkflowRecord

```rust
pub struct WorkflowRecord {
    pub id: Uuid,
    pub workflow_name: String,
    pub version: u32,
    pub input: serde_json::Value,
    pub output: Option<serde_json::Value>,
    pub status: WorkflowStatus,
    pub current_step: Option<String>,
    pub step_results: serde_json::Value,
    pub started_at: DateTime<Utc>,
    pub completed_at: Option<DateTime<Utc>>,
    pub error: Option<String>,
    pub trace_id: Option<String>,
}
```

### Database Schema

```sql
CREATE TABLE forge_workflow_runs (
    id UUID PRIMARY KEY,
    workflow_name VARCHAR(255) NOT NULL,
    version VARCHAR(64),
    input JSONB NOT NULL DEFAULT '{}',
    output JSONB,
    status VARCHAR(32) NOT NULL DEFAULT 'created',
    current_step VARCHAR(255),
    step_results JSONB DEFAULT '{}',
    started_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    completed_at TIMESTAMPTZ,
    error TEXT,
    trace_id VARCHAR(64)
);
```

---

## Real-Time Subscriptions

Workflows support WebSocket subscriptions for real-time updates:

```typescript
// Frontend: Subscribe to workflow updates
const workflow = subscribeWorkflow(runId);

// Reactive updates
$: if ($workflow) {
    console.log($workflow.status);
    console.log($workflow.steps);
}
```

PostgreSQL NOTIFY triggers are enabled on workflow tables:

```sql
SELECT forge_enable_reactivity('forge_workflow_runs');
SELECT forge_enable_reactivity('forge_workflow_steps');
```

---

## WorkflowExecutor Methods

| Method | Description |
|--------|-------------|
| `start(name, input)` | Start workflow, returns `run_id` immediately |
| `resume(run_id)` | Resume a paused/running workflow |
| `status(run_id)` | Get current workflow record |
| `cancel(run_id)` | Cancel workflow and run compensation |

### Executor Example

```rust
let executor = WorkflowExecutor::new(
    Arc::new(registry),
    pool.clone(),
    http_client,
);

// Start workflow (returns immediately)
let run_id = executor.start("user_onboarding", input).await?;

// Check status
let record = executor.status(run_id).await?;
println!("Status: {:?}", record.status);

// Cancel and compensate
executor.cancel(run_id).await?;
```

---

## Best Practices

### Keep Steps Idempotent

Use idempotency keys for external calls:

```rust
ctx.step("charge_card", || async {
    stripe::PaymentIntent::create(&payment)
        .idempotency_key(&ctx.run_id.to_string())
        .await
})
.compensate(...)
.run()
.await?;
```

### Design Thoughtful Compensation

Consider all possible states:

```rust
.compensate(|order| async move {
    match order.status {
        OrderStatus::Draft => delete_order(order.id).await,
        OrderStatus::Processing => cancel_order(order.id).await,
        OrderStatus::Shipped => {
            // Can't undo - log for manual intervention
            tracing::error!("Cannot compensate shipped order {}", order.id);
            Ok(())
        }
    }
})
```

### Use Deterministic Time

```rust
// WRONG: Non-deterministic
let now = Utc::now();

// CORRECT: Deterministic across replays
let now = ctx.workflow_time();
```

### Handle Workflow Resume

Always check `is_step_completed()` when using low-level API:

```rust
if ctx.is_step_completed("step_name") {
    // Use cached result
    ctx.get_step_result::<T>("step_name")?
} else {
    // Execute step
}
```

---

## Related Documentation

- [Jobs](JOBS.md) - Simple background tasks
- [Functions](FUNCTIONS.md) - Queries, mutations, actions
- [Crons](CRONS.md) - Scheduled tasks

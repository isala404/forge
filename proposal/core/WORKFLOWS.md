# Workflows

> *Multi-step processes with compensation*

---

## Overview

Workflows are **durable, multi-step processes** that:

- Survive server restarts
- Handle failures gracefully
- Support compensation (rollback)
- Track progress and state
- Can run for hours, days, or longer

Use workflows when you need to orchestrate multiple steps that must complete together, especially when external services are involved.

---

## The Problem Workflows Solve

Consider user onboarding:

```rust
// ❌ This is fragile
#[forge::action]
pub async fn onboard_user(ctx: &ActionContext, input: OnboardInput) -> Result<User> {
    let user = ctx.mutate(create_user, input.clone()).await?;
    
    let stripe = stripe::Customer::create(&user).await?;  // What if this fails?
    ctx.mutate(save_stripe_id, (user.id, stripe.id)).await?;
    
    provision_resources(&user).await?;  // User exists but no resources!
    
    send_welcome_email(&user).await?;  // Not critical, but still...
    
    Ok(user)
}
```

If Stripe fails, the user exists but has no payment setup. If provisioning fails, the user might be in an inconsistent state.

---

## Defining Workflows

```rust
// functions/workflows/onboarding.rs

use forge::prelude::*;

#[forge::workflow]
pub async fn user_onboarding(
    ctx: &WorkflowContext,
    input: OnboardingInput,
) -> Result<OnboardingResult> {
    // Step 1: Create user
    let user = ctx.step("create_user")
        .run(|| ctx.mutate(create_user, CreateUserInput {
            email: input.email.clone(),
            name: input.name.clone(),
        }))
        .compensate(|user| ctx.mutate(delete_user, user.id))
        .await?;
    
    // Step 2: Setup Stripe
    let stripe_customer = ctx.step("setup_stripe")
        .run(|| stripe::Customer::create(&user))
        .compensate(|customer| stripe::Customer::delete(&customer.id))
        .await?;
    
    ctx.mutate(save_stripe_customer, SaveStripeInput {
        user_id: user.id,
        stripe_customer_id: stripe_customer.id,
    }).await?;
    
    // Step 3: Provision resources
    let resources = ctx.step("provision_resources")
        .run(|| provision_user_resources(&user))
        .compensate(|res| deprovision_resources(&res))
        .await?;
    
    // Step 4: Send welcome email (optional, don't compensate others if this fails)
    ctx.step("send_welcome")
        .run(|| send_welcome_email(&user.email))
        .optional()  // Failure here doesn't trigger compensation
        .await;
    
    Ok(OnboardingResult {
        user,
        stripe_customer_id: stripe_customer.id,
        resources,
    })
}
```

---

## How Compensation Works

When a step fails, FORGE runs compensation for all previous steps in reverse order:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                      WORKFLOW COMPENSATION                                   │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  Step 1: create_user ────► Success ────┐                                     │
│                                        │                                     │
│  Step 2: setup_stripe ───► Success ────┤                                     │
│                                        │                                     │
│  Step 3: provision ──────► FAILURE! ◄──┘                                     │
│                                                                              │
│  ─────────────────────────────────────────────────────────────────────────  │
│                                                                              │
│  Compensation triggered (reverse order):                                     │
│                                                                              │
│  Compensate step 2: stripe::Customer::delete() ────► Success                 │
│                                                                              │
│  Compensate step 1: delete_user() ─────────────────► Success                 │
│                                                                              │
│  Result: Clean rollback, no orphaned data                                    │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Workflow State Persistence

Workflows are durable—their state is saved to PostgreSQL after each step:

```rust
#[forge::workflow]
pub async fn long_running_workflow(ctx: &WorkflowContext, input: Input) -> Result<Output> {
    // State saved after each step
    let step1 = ctx.step("step1").run(...).await?;  // Checkpoint
    let step2 = ctx.step("step2").run(...).await?;  // Checkpoint
    
    // If server restarts here, workflow resumes from step3
    
    let step3 = ctx.step("step3").run(...).await?;  // Checkpoint
    
    Ok(Output { ... })
}
```

### State Storage

```sql
-- Workflow execution state
CREATE TABLE forge_workflow_runs (
    id UUID PRIMARY KEY,
    workflow_name VARCHAR(255) NOT NULL,
    input JSONB NOT NULL,
    status VARCHAR(50) NOT NULL,  -- running, completed, failed, compensating
    current_step VARCHAR(255),
    step_results JSONB DEFAULT '{}',
    started_at TIMESTAMPTZ NOT NULL,
    completed_at TIMESTAMPTZ,
    error TEXT
);

-- Individual step state
CREATE TABLE forge_workflow_steps (
    id UUID PRIMARY KEY,
    workflow_run_id UUID REFERENCES forge_workflow_runs(id),
    step_name VARCHAR(255) NOT NULL,
    status VARCHAR(50) NOT NULL,  -- pending, running, completed, failed, compensated
    result JSONB,
    error TEXT,
    started_at TIMESTAMPTZ,
    completed_at TIMESTAMPTZ
);
```

### State Format & Robustness

Workflow state is stored as JSONB in PostgreSQL. This is simple and queryable, but requires care:

**Schema versioning:**

Each workflow state includes a format version:

```sql
-- Workflow state includes version metadata
{
  "_format_version": 2,
  "_workflow_version": 1,
  "user_id": "abc-123",
  "step_results": { ... }
}
```

**Forward-compatible changes (safe):**
- Adding new optional fields to step results
- Adding new steps at the end
- Changing step retry counts

**Breaking changes (require migration):**
- Removing fields that later steps depend on
- Renaming step names
- Changing step order

**Handling breaking changes:**

When you need to make a breaking change to a workflow:

```rust
// 1. Check for in-flight workflows in dashboard
//    Dashboard → Workflows → filter by name

// 2. If count is low, wait for them to complete

// 3. If count is high or urgent, migrate via dashboard:
//    Dashboard → Workflows → Migrate → select target version
```

**State backup:**

Before major changes, export workflow state via dashboard:

Dashboard → Workflows → Export → JSON

This creates a snapshot you can restore if migration fails.

**Corrupt state recovery:**

If a workflow has corrupt state (rare):

```rust
// Via dashboard: Workflows → [workflow] → Reset Step
// This re-runs the step from scratch

// Or skip the problematic step entirely:
// Dashboard → Workflows → [workflow] → Skip Step → provide manual result
```

---

## Advanced Patterns

### Parallel Steps

```rust
#[forge::workflow]
pub async fn parallel_workflow(ctx: &WorkflowContext, input: Input) -> Result<Output> {
    // Run steps in parallel
    let (result1, result2, result3) = ctx.parallel()
        .step("fetch_from_api1", || fetch_api1(&input))
        .step("fetch_from_api2", || fetch_api2(&input))
        .step("fetch_from_api3", || fetch_api3(&input))
        .await?;
    
    // All three complete before continuing
    let combined = combine_results(result1, result2, result3);
    
    ctx.step("save_results")
        .run(|| ctx.mutate(save_combined, combined))
        .await?;
    
    Ok(Output { ... })
}
```

### Conditional Steps

```rust
#[forge::workflow]
pub async fn conditional_workflow(ctx: &WorkflowContext, input: Input) -> Result<Output> {
    let user = ctx.step("get_user")
        .run(|| ctx.query(get_user, input.user_id))
        .await?;
    
    // Conditionally run premium setup
    if user.plan == Plan::Premium {
        ctx.step("setup_premium")
            .run(|| setup_premium_features(&user))
            .compensate(|_| teardown_premium_features(&user))
            .await?;
    }
    
    // Always run basic setup
    ctx.step("setup_basic")
        .run(|| setup_basic_features(&user))
        .await?;
    
    Ok(Output { ... })
}
```

### Waiting for External Events

```rust
#[forge::workflow]
pub async fn approval_workflow(ctx: &WorkflowContext, input: ApprovalInput) -> Result<ApprovalResult> {
    // Create approval request
    let request = ctx.step("create_request")
        .run(|| ctx.mutate(create_approval_request, input.clone()))
        .await?;
    
    // Notify approver
    ctx.step("notify_approver")
        .run(|| send_approval_email(&input.approver_email, &request))
        .await?;
    
    // Wait for approval (external event) with timeout
    let approval = ctx.wait_for_event("approval")
        .filter(|e| e.request_id == request.id)
        .timeout(Duration::days(7))
        .await?;
    
    match approval {
        Some(event) if event.approved => {
            ctx.step("process_approved")
                .run(|| process_approval(&request))
                .await?;
            Ok(ApprovalResult::Approved)
        }
        Some(event) => {
            ctx.step("process_rejected")
                .run(|| process_rejection(&request, &event.reason))
                .await?;
            Ok(ApprovalResult::Rejected(event.reason))
        }
        None => {
            ctx.step("process_timeout")
                .run(|| process_timeout(&request))
                .await?;
            Ok(ApprovalResult::Timeout)
        }
    }
}

// External event sender (from webhook or mutation)
#[forge::mutation]
pub async fn submit_approval(ctx: &MutationContext, input: SubmitApprovalInput) -> Result<()> {
    ctx.emit_workflow_event("approval", ApprovalEvent {
        request_id: input.request_id,
        approved: input.approved,
        reason: input.reason,
    });
    Ok(())
}
```

### Timeouts and Retries

```rust
#[forge::workflow]
pub async fn robust_workflow(ctx: &WorkflowContext, input: Input) -> Result<Output> {
    // Step with timeout
    let data = ctx.step("fetch_external")
        .run(|| fetch_external_data(&input))
        .timeout(Duration::minutes(5))
        .await?;
    
    // Step with retries
    let result = ctx.step("unreliable_step")
        .run(|| call_unreliable_service(&data))
        .retry(3, Duration::seconds(10))  // 3 retries, 10s between
        .await?;
    
    // Step with both
    let final_result = ctx.step("critical_step")
        .run(|| critical_operation(&result))
        .timeout(Duration::minutes(10))
        .retry(5, Duration::seconds(30))
        .await?;
    
    Ok(final_result)
}
```

---

## Workflow Lifecycle

```
┌─────────┐
│ CREATED │ ◄─── Workflow dispatched
└────┬────┘
     │
     │  First step starts
     ▼
┌─────────┐
│ RUNNING │ ◄─── Steps executing, checkpoints saved
└────┬────┘
     │
     ├─── All steps complete ─────────────────────┐
     │                                            │
     │                                            ▼
     │                                     ┌───────────┐
     │                                     │ COMPLETED │
     │                                     └───────────┘
     │
     ├─── Step fails, compensation works ─────────┐
     │                                            │
     │                                            ▼
     │                                     ┌───────────┐
     │                                     │COMPENSATED│
     │                                     └───────────┘
     │
     ├─── Step fails, compensation fails ─────────┐
     │                                            │
     │                                            ▼
     │                                     ┌───────────────┐
     │                                     │FAILED (manual)│
     │                                     └───────────────┘
     │
     └─── Waiting for external event ─────────────┐
                                                  │
                                                  ▼
                                           ┌───────────┐
                                           │  WAITING  │
                                           └─────┬─────┘
                                                 │
                                                 ▼
                                           Continue...
```

---

## Invoking Workflows

### From Mutations

```rust
#[forge::mutation]
pub async fn register_user(ctx: &MutationContext, input: RegisterInput) -> Result<WorkflowHandle> {
    // Start workflow, return immediately
    let handle = ctx.dispatch_workflow(user_onboarding, OnboardingInput {
        email: input.email,
        name: input.name,
    }).await?;
    
    Ok(handle)  // Client can poll for status
}
```

### From Actions

```rust
#[forge::action]
pub async fn start_and_wait(ctx: &ActionContext, input: Input) -> Result<WorkflowResult> {
    // Start workflow and wait for completion
    let result = ctx.dispatch_workflow(my_workflow, input)
        .await?
        .wait()
        .await?;
    
    Ok(result)
}
```

### Monitoring Workflow Status

```svelte
<script>
  import { workflowStatus } from '$lib/forge';
  
  let workflowId = '...';
  const status = workflowStatus(workflowId);
</script>

{#if $status.loading}
  <Loading />
{:else if $status.data.status === 'running'}
  <Progress step={$status.data.currentStep} />
{:else if $status.data.status === 'completed'}
  <Success result={$status.data.result} />
{:else if $status.data.status === 'failed'}
  <Error message={$status.data.error} />
{/if}
```

---

## Manual Intervention

Sometimes workflows need human intervention:

### Retry Failed Workflow

```bash
# Via CLI
forge workflow retry <workflow_id>

# With modified input
forge workflow retry <workflow_id> --input '{"retryPayment": true}'
```

### Skip a Step

```bash
# Skip the failed step and continue
forge workflow skip-step <workflow_id> <step_name>

# Skip with manual result
forge workflow skip-step <workflow_id> payment --result '{"manual": true}'
```

### Cancel Workflow

```bash
# Cancel and run compensation
forge workflow cancel <workflow_id>

# Force cancel without compensation
forge workflow cancel <workflow_id> --force
```

---

## Testing Workflows

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use forge::testing::*;
    
    #[tokio::test]
    async fn test_onboarding_success() {
        let ctx = TestWorkflowContext::new()
            .mock_mutation(create_user, || Ok(User { id: "123", ... }))
            .mock_external(stripe::Customer::create, || Ok(StripeCustomer { ... }))
            .build();
        
        let result = user_onboarding(&ctx, OnboardingInput { ... }).await;
        
        assert!(result.is_ok());
        assert!(ctx.step_completed("create_user"));
        assert!(ctx.step_completed("setup_stripe"));
    }
    
    #[tokio::test]
    async fn test_onboarding_stripe_failure_compensates() {
        let ctx = TestWorkflowContext::new()
            .mock_mutation(create_user, || Ok(User { id: "123", ... }))
            .mock_external(stripe::Customer::create, || Err(StripeError::RateLimit))
            .build();
        
        let result = user_onboarding(&ctx, OnboardingInput { ... }).await;
        
        assert!(result.is_err());
        assert!(ctx.compensation_ran("create_user"));  // User was deleted
    }
}
```

---

## Workflow Versioning

**The Problem:** When you change a workflow definition while workflows are "sleeping" (waiting for events or between steps), the serialized state in the database may not match your new code.

Example: Workflow v1 expects `step_3` to receive `InputA`, but v2 changes it to expect `InputB`. The 5,000 workflows sleeping in step 2 will fail when they resume.

### Versioning Strategy

FORGE uses explicit workflow versions to handle this:

```rust
#[forge::workflow]
#[version = 2]  // Increment when making breaking changes
pub async fn user_onboarding(
    ctx: &WorkflowContext,
    input: OnboardingInput,
) -> Result<OnboardingResult> {
    // V2 implementation
}

// Keep old version for in-flight workflows
#[forge::workflow]
#[version = 1]
#[deprecated]
pub async fn user_onboarding_v1(
    ctx: &WorkflowContext,
    input: OnboardingInputV1,
) -> Result<OnboardingResultV1> {
    // V1 implementation - keep until all v1 workflows complete
}
```

### Version Migration

```rust
// Migration function to upgrade sleeping workflows
#[forge::workflow_migration]
pub async fn migrate_onboarding_v1_to_v2(
    old_state: OnboardingStateV1,
) -> Result<OnboardingStateV2> {
    OnboardingStateV2 {
        user_id: old_state.user_id,
        email: old_state.email,
        // Map old fields to new structure
        new_required_field: default_value(),
    }
}
```

### Configuration

```toml
# forge.toml
[workflows]
# How to handle version mismatches
version_mismatch = "migrate"  # migrate, fail, or continue

# Keep old versions for N days after last execution
deprecated_version_retention = "30d"
```

### Monitoring Version Drift

```bash
# Check for workflows on old versions
forge workflow list --version-drift

# Output:
# Workflow            Version  Count  Oldest
# user_onboarding     v1       523    2024-01-10
# order_processing    v2       12     2024-01-14
```

### Handling Long-Running Workflows

Workflows that run for days or weeks need special consideration:

**The challenge:** You can't wait 30 days for all approval workflows to complete before deploying a breaking change.

**Strategy 1: Side-by-side versions (recommended)**

Run both versions simultaneously:

```rust
// workflows/onboarding_v1.rs - Keep for in-flight workflows
#[forge::workflow]
#[version = 1]
pub async fn user_onboarding(ctx: &WorkflowContext, input: OnboardingInputV1) -> Result<OnboardingResultV1> {
    // Original implementation
}

// workflows/onboarding_v2.rs - New workflows use this
#[forge::workflow]
#[version = 2]
pub async fn user_onboarding(ctx: &WorkflowContext, input: OnboardingInputV2) -> Result<OnboardingResultV2> {
    // New implementation
}
```

```toml
# forge.toml
[workflows.versioning]
# New workflow starts use latest version
default_version = "latest"

# Keep serving old versions for existing workflows
serve_deprecated = true

# Alert when old versions linger
alert_on_old_versions_after = "7d"
```

**Strategy 2: Compatible changes only**

For small changes, design for backward compatibility:

```rust
#[derive(Deserialize)]
pub struct OnboardingInput {
    pub email: String,
    pub name: String,

    // New field with default - old state can deserialize
    #[serde(default)]
    pub referral_code: Option<String>,
}
```

**Strategy 3: Force migration for urgent changes**

When you must migrate immediately:

```bash
# Preview which workflows will be affected
forge workflow migrate preview user_onboarding --from-version 1 --to-version 2

# Output:
# Workflows to migrate: 523
# Current steps:
#   - waiting at step "approval": 412
#   - waiting at step "payment": 89
#   - running step "provision": 22
#
# Migration will:
#   - Transform state using migrate_onboarding_v1_to_v2
#   - Resume workflows with v2 code
#
# Estimated disruption: None (if migration function is correct)

# Execute migration
forge workflow migrate execute user_onboarding --from-version 1 --to-version 2

# Or migrate incrementally (safest)
forge workflow migrate execute user_onboarding --from-version 1 --to-version 2 --batch-size 50 --delay 5s
```

### Migration Rollback

If a migration causes issues:

**Before migration (automatic):**

```toml
# forge.toml
[workflows.migration]
# Automatically snapshot state before migration
auto_snapshot = true

# Keep snapshots for N days
snapshot_retention = "7d"
```

**Rollback to snapshot:**

```bash
# List available snapshots
forge workflow snapshots list user_onboarding

# Output:
# Snapshot ID                           Created            Workflows
# snap_20240115_120000_v1_to_v2         2024-01-15 12:00   523

# Rollback
forge workflow snapshots restore snap_20240115_120000_v1_to_v2

# This will:
# 1. Stop affected workflows
# 2. Restore state from snapshot
# 3. Resume with old version code
```

**Partial rollback (specific workflows):**

```bash
# Rollback only failed workflows
forge workflow rollback user_onboarding --status failed --to-version 1

# Rollback specific workflow IDs
forge workflow rollback --ids wf_abc123,wf_def456 --to-version 1
```

### Version Deprecation Lifecycle

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                    VERSION LIFECYCLE                                         │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  Version Status          Behavior                                            │
│  ──────────────          ────────                                            │
│                                                                              │
│  "active" (default)      - New workflows use this version                   │
│                          - Existing workflows continue                       │
│                                                                              │
│  "deprecated"            - No new workflows on this version                  │
│                          - Existing workflows continue to completion         │
│                          - Compile warning if referenced                     │
│                                                                              │
│  "migrating"             - Active migration in progress                      │
│                          - Both versions temporarily active                  │
│                          - Dashboard shows progress                          │
│                                                                              │
│  "retired"               - No workflows can use this version                 │
│                          - Code can be deleted after retention period        │
│                          - Attempting to resume fails with error             │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

```rust
// Mark version status in code
#[forge::workflow]
#[version = 1]
#[status = "deprecated"]  // or "retired"
pub async fn user_onboarding_v1(...) { ... }
```

### Safe Deployment Pattern

When deploying workflow changes:

```bash
# 1. Deploy with new version (doesn't affect running workflows)
#    New code includes both v1 and v2

# 2. Verify new version works for NEW workflows
forge workflow stats user_onboarding --version 2 --last 1h
# Check success rate, completion time, errors

# 3. If satisfied, start migrating old workflows (optional)
forge workflow migrate execute user_onboarding --from 1 --to 2 --batch-size 100

# 4. Monitor migration
forge workflow migrate status user_onboarding

# 5. Once all v1 workflows complete, mark as retired
#    (Or wait for deprecated_version_retention to expire)

# 6. Later deployment: remove v1 code
```

---

## Determinism Requirements

**The Problem:** Workflow replay requires deterministic execution. If a workflow step produces different results on replay, the workflow state becomes corrupted.

### Non-Deterministic Patterns to Avoid

```rust
// ❌ WRONG: HashMap iteration order is non-deterministic
#[forge::workflow]
pub async fn process_items(ctx: &WorkflowContext, items: HashMap<String, Item>) -> Result<()> {
    for (key, item) in items.iter() {  // Order varies between runs!
        ctx.step(&format!("process_{}", key))
            .run(|| process_item(item))
            .await?;
    }
    Ok(())
}

// ✅ CORRECT: Use BTreeMap or sort keys
#[forge::workflow]
pub async fn process_items(ctx: &WorkflowContext, items: BTreeMap<String, Item>) -> Result<()> {
    for (key, item) in items.iter() {  // Order is deterministic
        ctx.step(&format!("process_{}", key))
            .run(|| process_item(item))
            .await?;
    }
    Ok(())
}

// ❌ WRONG: Random values change on replay
#[forge::workflow]
pub async fn assign_random(ctx: &WorkflowContext) -> Result<String> {
    let id = uuid::Uuid::new_v4().to_string();  // Different every time!
    ctx.step("save").run(|| save_id(&id)).await?;
    Ok(id)
}

// ✅ CORRECT: Generate random values inside steps (recorded)
#[forge::workflow]
pub async fn assign_random(ctx: &WorkflowContext) -> Result<String> {
    let id = ctx.step("generate_id")
        .run(|| uuid::Uuid::new_v4().to_string())
        .await?;  // Result is recorded and replayed
    ctx.step("save").run(|| save_id(&id)).await?;
    Ok(id)
}

// ❌ WRONG: Current time changes
#[forge::workflow]
pub async fn time_based(ctx: &WorkflowContext) -> Result<()> {
    let now = Utc::now();  // Different on replay!
    if now.hour() > 12 {
        // Logic depends on when workflow runs vs replays
    }
    Ok(())
}

// ✅ CORRECT: Use workflow time
#[forge::workflow]
pub async fn time_based(ctx: &WorkflowContext) -> Result<()> {
    let now = ctx.workflow_time();  // Consistent across replays
    if now.hour() > 12 {
        // Deterministic
    }
    Ok(())
}
```

### Determinism Linter

FORGE includes a compile-time linter that warns about common non-determinism issues:

```bash
# Run determinism check
forge workflow lint

# Output:
# WARNING: order_workflow.rs:45 - HashMap iteration in workflow context
# WARNING: user_workflow.rs:23 - Uuid::new_v4() outside of step
# ERROR: payment_workflow.rs:67 - std::time::Instant::now() in workflow
```

### What's Safe in Workflows

| Operation | Safe? | Notes |
|-----------|-------|-------|
| `ctx.step(...).run(...)` | ✅ | Results are recorded |
| `ctx.query(...)` | ✅ | Read from recorded state |
| `ctx.mutate(...)` | ✅ | Side effects are recorded |
| `ctx.workflow_time()` | ✅ | Deterministic time |
| `HashMap` iteration | ❌ | Use `BTreeMap` or sort |
| `Uuid::new_v4()` | ❌ | Move inside a step |
| `Utc::now()` | ❌ | Use `ctx.workflow_time()` |
| `rand::random()` | ❌ | Move inside a step |
| Async race conditions | ❌ | Use `ctx.parallel()` |

---

## Best Practices

### 1. Keep Steps Idempotent

```rust
// ❌ Not idempotent
ctx.step("charge_card")
    .run(|| stripe::PaymentIntent::create(&payment_details))
    .await?;

// ✅ Idempotent with idempotency key
ctx.step("charge_card")
    .run(|| stripe::PaymentIntent::create(&payment_details)
        .idempotency_key(&workflow_id))
    .await?;
```

### 2. Design Good Compensation

```rust
// ❌ Compensation might not work
ctx.step("create_order")
    .run(|| create_order(&input))
    .compensate(|order| delete_order(order.id))  // What if order already shipped?
    .await?;

// ✅ State-aware compensation
ctx.step("create_order")
    .run(|| create_order(&input))
    .compensate(|order| {
        if order.status == OrderStatus::Draft {
            delete_order(order.id)
        } else {
            cancel_order(order.id)  // Different action for shipped orders
        }
    })
    .await?;
```

### 3. Use Workflows for the Right Things

**Good uses:**
- Multi-service transactions (user + Stripe + resources)
- Long-running processes (approvals, batch processing)
- Processes needing human intervention
- Complex business logic with rollback needs

**Don't use for:**
- Simple CRUD operations
- Single-service transactions (use mutations)
- Real-time operations

---

## Related Documentation

- [Jobs](JOBS.md) — Simple background tasks
- [Functions](FUNCTIONS.md) — Queries, mutations, actions
- [Crons](CRONS.md) — Scheduled workflows

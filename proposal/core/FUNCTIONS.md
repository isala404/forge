# Functions

> *The building blocks of your application*

---

## Overview

FORGE applications are built from three types of functions:

| Type | Purpose | Can Read DB | Can Write DB | Can Call External APIs | Transactional |
|------|---------|-------------|--------------|------------------------|---------------|
| **Query** | Read data | ✅ | ❌ | ❌ | Read-only |
| **Mutation** | Write data | ✅ | ✅ | ❌ | ✅ Full ACID |
| **Action** | Side effects | Via queries | Via mutations | ✅ | ❌ |

---

## Queries

Queries are **read-only** functions that can be **cached** and **subscribed to**.

### Basic Query

```rust
// functions/queries/projects.rs

use forge::prelude::*;

#[forge::query]
pub async fn get_projects(
    ctx: &QueryContext,
    owner_id: Uuid,
) -> Result<Vec<Project>> {
    ctx.db
        .query::<Project>()
        .filter(|p| p.owner_id == owner_id)
        .filter(|p| p.status != ProjectStatus::Archived)
        .order_by(|p| p.created_at.desc())
        .fetch_all()
        .await
}
```

### Query with Arguments

Arguments are automatically validated:

```rust
#[forge::query]
pub async fn get_project(
    ctx: &QueryContext,
    #[arg] project_id: Uuid,        // Required
    #[arg(optional)] include_tasks: bool,  // Optional, defaults to false
) -> Result<ProjectWithTasks> {
    let project = ctx.db.get::<Project>(project_id).await?
        .ok_or(Error::NotFound)?;
    
    let tasks = if include_tasks {
        ctx.db.query::<Task>()
            .filter(|t| t.project_id == project_id)
            .fetch_all()
            .await?
    } else {
        vec![]
    };
    
    Ok(ProjectWithTasks { project, tasks })
}
```

### Query with Pagination

```rust
#[forge::query]
pub async fn list_projects(
    ctx: &QueryContext,
    #[arg] page: Page,  // { limit: u32, cursor: Option<String> }
) -> Result<Paginated<Project>> {
    ctx.db
        .query::<Project>()
        .filter(|p| p.owner_id == ctx.auth.user_id()?)
        .order_by(|p| p.created_at.desc())
        .paginate(page)
        .await
}
```

### Query Caching

Queries are automatically cached based on:
- Function name
- Arguments
- User context (if authenticated)

```rust
#[forge::query]
#[cache(ttl = "5m")]  // Cache for 5 minutes
pub async fn get_expensive_report(
    ctx: &QueryContext,
    report_id: Uuid,
) -> Result<Report> {
    // This expensive computation is cached
    ...
}

#[forge::query]
#[cache(ttl = "0")]  // Disable caching
pub async fn get_realtime_data(ctx: &QueryContext) -> Result<Data> {
    // Always fresh
    ...
}
```

### Query Rules

1. **Deterministic**: Same inputs → same outputs
2. **No side effects**: Cannot modify database
3. **No external calls**: Cannot call APIs (use Actions)
4. **Fast**: Should complete in < 100ms typically

---

## Mutations

Mutations **write data** and run in a **transaction**.

### Basic Mutation

```rust
// functions/mutations/projects.rs

use forge::prelude::*;

#[forge::mutation]
pub async fn create_project(
    ctx: &MutationContext,
    input: CreateProjectInput,
) -> Result<Project> {
    // Validate input
    input.validate()?;
    
    // Check permissions
    let user = ctx.auth.require_user()?;
    
    // Create project
    let project = ctx.db.insert(Project {
        id: Uuid::new_v4(),
        owner_id: user.id,
        name: input.name,
        status: ProjectStatus::Draft,
        created_at: Timestamp::now(),
        updated_at: Timestamp::now(),
    }).await?;
    
    // Emit event (for subscriptions)
    ctx.emit(ProjectCreatedEvent { project_id: project.id });
    
    Ok(project)
}
```

### Input Validation

```rust
#[derive(Debug, Deserialize, Validate)]
pub struct CreateProjectInput {
    #[validate(length(min = 1, max = 100))]
    pub name: String,
    
    #[validate(length(max = 1000))]
    pub description: Option<String>,
    
    #[validate(custom = "validate_slug")]
    pub slug: String,
}

fn validate_slug(slug: &str) -> Result<(), ValidationError> {
    if !slug.chars().all(|c| c.is_alphanumeric() || c == '-') {
        return Err(ValidationError::new("slug must be alphanumeric with hyphens"));
    }
    Ok(())
}
```

### Mutation with Complex Logic

```rust
#[forge::mutation]
pub async fn transfer_ownership(
    ctx: &MutationContext,
    project_id: Uuid,
    new_owner_id: Uuid,
) -> Result<Project> {
    // All of this runs in ONE transaction
    
    // 1. Get project (locks the row)
    let mut project = ctx.db.get_for_update::<Project>(project_id).await?
        .ok_or(Error::NotFound)?;
    
    // 2. Check current user is owner
    let user = ctx.auth.require_user()?;
    if project.owner_id != user.id {
        return Err(Error::Forbidden);
    }
    
    // 3. Verify new owner exists
    let new_owner = ctx.db.get::<User>(new_owner_id).await?
        .ok_or(Error::InvalidInput("New owner not found"))?;
    
    // 4. Update ownership
    project.owner_id = new_owner_id;
    project.updated_at = Timestamp::now();
    ctx.db.update(&project).await?;
    
    // 5. Create audit log
    ctx.db.insert(AuditLog {
        id: Uuid::new_v4(),
        action: "transfer_ownership",
        entity_type: "project",
        entity_id: project_id,
        actor_id: user.id,
        details: json!({
            "previous_owner": user.id,
            "new_owner": new_owner_id,
        }),
        created_at: Timestamp::now(),
    }).await?;
    
    // 6. Send notification (via job, not blocking)
    ctx.dispatch_job(notify_ownership_transfer, NotifyTransfer {
        project_id,
        previous_owner_id: user.id,
        new_owner_id,
    }).await?;
    
    Ok(project)
}
```

### Transaction Guarantees

Mutations provide:

| Guarantee | Meaning |
|-----------|---------|
| **Atomicity** | All changes commit or none do |
| **Consistency** | Constraints are enforced |
| **Isolation** | Serializable (strongest level) |
| **Durability** | Committed data survives crashes |

```rust
#[forge::mutation]
pub async fn risky_operation(ctx: &MutationContext) -> Result<()> {
    ctx.db.update(...).await?;  // Change 1
    ctx.db.update(...).await?;  // Change 2
    
    if something_wrong() {
        return Err(Error::SomethingWrong);
        // Both changes are rolled back!
    }
    
    ctx.db.update(...).await?;  // Change 3
    
    Ok(())  // All three changes commit together
}
```

### Conflict Handling

With serializable isolation, conflicts are detected:

```rust
#[forge::mutation]
#[retry(on = "SerializationError", max_attempts = 3)]
pub async fn increment_counter(ctx: &MutationContext, id: Uuid) -> Result<i32> {
    let mut counter = ctx.db.get::<Counter>(id).await?;
    counter.value += 1;
    ctx.db.update(&counter).await?;
    Ok(counter.value)
}
```

If two transactions try to increment simultaneously:
1. One succeeds
2. The other gets `SerializationError`
3. FORGE retries automatically (up to 3 times)

---

## Actions

Actions can call **external services** but are **not transactional**.

### Basic Action

```rust
// functions/actions/stripe.rs

use forge::prelude::*;

#[forge::action]
pub async fn sync_with_stripe(
    ctx: &ActionContext,
    user_id: Uuid,
) -> Result<SyncResult> {
    // 1. Read from database (via query)
    let user = ctx.query(get_user, user_id).await?;
    
    // 2. Call external API (Stripe)
    let customer = stripe::Customer::retrieve(&user.stripe_customer_id).await?;
    
    // 3. Update database (via mutation)
    ctx.mutate(update_user_subscription, UpdateSubscription {
        user_id,
        plan: customer.subscription.plan,
        status: customer.subscription.status,
        current_period_end: customer.subscription.current_period_end,
    }).await?;
    
    Ok(SyncResult { synced: true })
}
```

### Action Timeouts

```rust
#[forge::action]
#[timeout(seconds = 60)]  // Fail if takes > 60s
pub async fn call_slow_api(ctx: &ActionContext) -> Result<Response> {
    // API call that might be slow
    ...
}
```

### Rate-Limited Actions

```rust
#[forge::action]
#[rate_limit(
    key = "openai",           // Shared limit across all calls
    requests = 60,            // 60 requests
    per = "minute",           // per minute
    strategy = "sliding"      // Sliding window
)]
pub async fn call_openai(ctx: &ActionContext, prompt: String) -> Result<String> {
    openai::complete(prompt).await
}

// Per-user rate limiting
#[forge::action]
#[rate_limit(
    key = |ctx| format!("user:{}", ctx.auth.user_id()),
    requests = 10,
    per = "hour"
)]
pub async fn user_ai_request(ctx: &ActionContext, prompt: String) -> Result<String> {
    ...
}
```

### Retry on Failure

```rust
#[forge::action]
#[retry(
    max_attempts = 3,
    backoff = "exponential",  // 1s, 2s, 4s
    on = ["NetworkError", "TimeoutError"]  // Only retry these
)]
pub async fn send_webhook(ctx: &ActionContext, url: String, payload: Value) -> Result<()> {
    reqwest::Client::new()
        .post(&url)
        .json(&payload)
        .timeout(Duration::seconds(10))
        .send()
        .await?;
    Ok(())
}
```

### Action Composition

Actions can orchestrate queries and mutations:

```rust
#[forge::action]
pub async fn process_order(ctx: &ActionContext, order_id: Uuid) -> Result<OrderResult> {
    // Step 1: Get order (query)
    let order = ctx.query(get_order, order_id).await?;
    
    // Step 2: Charge payment (external)
    let payment = stripe::PaymentIntent::create(&order.payment_details).await?;
    
    // Step 3: Update order status (mutation)
    ctx.mutate(update_order_status, UpdateOrderStatus {
        order_id,
        status: OrderStatus::Paid,
        payment_id: payment.id,
    }).await?;
    
    // Step 4: Send confirmation (external, but don't fail if it fails)
    if let Err(e) = send_email(&order.customer_email, "Order confirmed").await {
        ctx.log.warn("Failed to send confirmation email", json!({ "error": e.to_string() }));
    }
    
    // Step 5: Dispatch fulfillment job
    ctx.dispatch_job(fulfill_order, FulfillOrder { order_id }).await?;
    
    Ok(OrderResult { payment_id: payment.id })
}
```

---

## Context Objects

Each function type has a context with different capabilities:

### QueryContext

```rust
pub struct QueryContext {
    // Database access (read-only)
    pub db: QueryDb,
    
    // Authentication info
    pub auth: AuthContext,
    
    // Logging
    pub log: Logger,
    
    // Request metadata
    pub request: RequestMetadata,
}

impl QueryContext {
    // Get authenticated user (errors if not logged in)
    pub fn require_user(&self) -> Result<User>;
    
    // Get authenticated user (None if not logged in)
    pub fn user(&self) -> Option<User>;
    
    // Check if user has permission
    pub fn has_permission(&self, permission: &str) -> bool;
}
```

### MutationContext

```rust
pub struct MutationContext {
    // Database access (read + write)
    pub db: MutationDb,
    
    // Authentication
    pub auth: AuthContext,
    
    // Logging
    pub log: Logger,
    
    // Event emission
    pub events: EventEmitter,
    
    // Job scheduling
    pub jobs: JobScheduler,
    
    // Request metadata
    pub request: RequestMetadata,
}

impl MutationContext {
    // Dispatch a background job
    pub async fn dispatch_job<J: Job>(&self, job: J, input: J::Input) -> Result<JobId>;
    
    // Dispatch job with delay
    pub async fn dispatch_job_in<J: Job>(&self, delay: Duration, job: J, input: J::Input) -> Result<JobId>;
    
    // Emit an event for subscriptions
    pub fn emit<E: Event>(&self, event: E);
}
```

### ActionContext

```rust
pub struct ActionContext {
    // Authentication
    pub auth: AuthContext,
    
    // Logging
    pub log: Logger,
    
    // Scheduling
    pub scheduler: Scheduler,
    
    // Request metadata
    pub request: RequestMetadata,
}

impl ActionContext {
    // Call a query
    pub async fn query<Q: Query>(&self, query: Q, args: Q::Args) -> Result<Q::Output>;
    
    // Call a mutation
    pub async fn mutate<M: Mutation>(&self, mutation: M, args: M::Args) -> Result<M::Output>;
    
    // Dispatch a job
    pub async fn dispatch_job<J: Job>(&self, job: J, input: J::Input) -> Result<JobId>;
}
```

---

## Calling Functions from Frontend

### Svelte Integration

```svelte
<script>
  import { query, mutate, action } from '$lib/forge';
  import { get_projects, create_project, sync_with_stripe } from '$lib/forge/api';
  
  // Reactive query (auto-updates)
  const projects = query(get_projects, { ownerId: $currentUser.id });
  
  // Mutation
  async function handleCreate() {
    const result = await mutate(create_project, { name: 'New Project' });
    // $projects automatically updates!
  }
  
  // Action
  async function handleSync() {
    await action(sync_with_stripe, { userId: $currentUser.id });
  }
</script>

{#if $projects.loading}
  <Loading />
{:else if $projects.error}
  <Error message={$projects.error} />
{:else}
  {#each $projects.data as project}
    <ProjectCard {project} />
  {/each}
{/if}

<button on:click={handleCreate}>Create Project</button>
<button on:click={handleSync}>Sync with Stripe</button>
```

### Type Safety

All function calls are type-checked:

```typescript
// ✅ Correct
await mutate(create_project, { name: 'My Project' });

// ❌ TypeScript error: missing required field
await mutate(create_project, {});

// ❌ TypeScript error: wrong type
await mutate(create_project, { name: 123 });
```

---

## Error Handling

### Typed Errors

```rust
#[derive(Debug, thiserror::Error)]
pub enum ProjectError {
    #[error("Project not found")]
    NotFound,
    
    #[error("You don't have permission to access this project")]
    Forbidden,
    
    #[error("A project with this name already exists")]
    DuplicateName,
    
    #[error("Invalid input: {0}")]
    InvalidInput(String),
}

#[forge::mutation]
pub async fn create_project(ctx: &MutationContext, input: CreateProjectInput) -> Result<Project, ProjectError> {
    // Check for duplicate
    let existing = ctx.db.query::<Project>()
        .filter(|p| p.name == input.name && p.owner_id == ctx.auth.user_id()?)
        .fetch_optional()
        .await?;
    
    if existing.is_some() {
        return Err(ProjectError::DuplicateName);
    }
    
    // ... create project
}
```

### Frontend Error Handling

```svelte
<script>
  async function handleCreate() {
    try {
      await mutate(create_project, { name: projectName });
    } catch (error) {
      if (error.code === 'DuplicateName') {
        showToast('A project with this name already exists');
      } else {
        showToast('Something went wrong');
      }
    }
  }
</script>
```

---

## Best Practices

### 1. Keep Functions Small

```rust
// ❌ Too much in one function
#[forge::mutation]
pub async fn do_everything(ctx: &MutationContext, input: Input) -> Result<Output> {
    // 200 lines of logic...
}

// ✅ Compose smaller functions
#[forge::mutation]
pub async fn process_order(ctx: &MutationContext, input: OrderInput) -> Result<Order> {
    validate_inventory(ctx, &input).await?;
    let order = create_order(ctx, &input).await?;
    update_inventory(ctx, &order).await?;
    notify_warehouse(ctx, &order).await?;
    Ok(order)
}
```

### 2. Use Actions for External Calls

```rust
// ❌ Never call external APIs in mutations
#[forge::mutation]
pub async fn bad_mutation(ctx: &MutationContext) -> Result<()> {
    stripe::Customer::create(...).await?;  // Don't do this!
}

// ✅ Use actions, dispatch from mutations if needed
#[forge::mutation]
pub async fn good_mutation(ctx: &MutationContext) -> Result<()> {
    let order = ctx.db.insert(...).await?;
    ctx.dispatch_job(process_payment, order.id).await?;
    Ok(())
}
```

### 3. Validate Early

```rust
#[forge::mutation]
pub async fn create_project(ctx: &MutationContext, input: CreateProjectInput) -> Result<Project> {
    // Validate FIRST
    input.validate()?;
    
    // Then do database work
    ...
}
```

---

## Related Documentation

- [Schema](SCHEMA.md) — Data models used in functions
- [Jobs](JOBS.md) — Background job processing
- [Reactivity](REACTIVITY.md) — Real-time subscriptions
- [RPC Client](../frontend/RPC_CLIENT.md) — Calling functions from Svelte

# Functions

> The building blocks of FORGE applications

---

## Overview

FORGE applications are built from three types of functions:

| Type         | Purpose      | Can Read DB | Can Write DB | External APIs | Transactional |
|--------------|--------------|-------------|--------------|---------------|---------------|
| **Query**    | Read data    | Yes         | No           | No            | Read-only     |
| **Mutation** | Write data   | Yes         | Yes          | No            | Full ACID     |
| **Action**   | Side effects | Yes         | Yes          | Yes           | No            |

Each function type has:
- A corresponding trait (`ForgeQuery`, `ForgeMutation`, `ForgeAction`)
- A proc macro (`#[forge::query]`, `#[forge::mutation]`, `#[forge::action]`)
- A context object (`QueryContext`, `MutationContext`, `ActionContext`)

---

## Function Traits

All three function traits follow a similar pattern with associated types for input arguments and output.

### ForgeQuery

```rust
// crates/forge-core/src/function/traits.rs

pub trait ForgeQuery: Send + Sync + 'static {
    /// The input arguments type (deserialized from JSON).
    type Args: DeserializeOwned + Serialize + Send + Sync;
    /// The output type (serialized to JSON).
    type Output: Serialize + Send;

    /// Function metadata.
    fn info() -> FunctionInfo;

    /// Execute the query.
    fn execute(
        ctx: &QueryContext,
        args: Self::Args,
    ) -> Pin<Box<dyn Future<Output = Result<Self::Output>> + Send + '_>>;
}
```

Queries are:
- Read-only (cannot modify database)
- Cacheable based on function name and arguments
- Subscribable for real-time updates
- Deterministic (same inputs produce same outputs)

### ForgeMutation

```rust
pub trait ForgeMutation: Send + Sync + 'static {
    type Args: DeserializeOwned + Serialize + Send + Sync;
    type Output: Serialize + Send;

    fn info() -> FunctionInfo;

    fn execute(
        ctx: &MutationContext,
        args: Self::Args,
    ) -> Pin<Box<dyn Future<Output = Result<Self::Output>> + Send + '_>>;
}
```

Mutations are:
- Transactional (all changes commit or none do)
- Can read and write to the database
- Should NOT call external APIs (use Actions instead)
- Can dispatch jobs and start workflows

### ForgeAction

```rust
pub trait ForgeAction: Send + Sync + 'static {
    type Args: DeserializeOwned + Serialize + Send + Sync;
    type Output: Serialize + Send;

    fn info() -> FunctionInfo;

    fn execute(
        ctx: &ActionContext,
        args: Self::Args,
    ) -> Pin<Box<dyn Future<Output = Result<Self::Output>> + Send + '_>>;
}
```

Actions are:
- NOT transactional by default
- Can call external APIs via HTTP client
- Can dispatch jobs and start workflows
- May have configurable timeouts

---

## FunctionInfo

Each function provides metadata via `FunctionInfo`:

```rust
pub struct FunctionInfo {
    /// Function name (used for routing).
    pub name: &'static str,
    /// Human-readable description.
    pub description: Option<&'static str>,
    /// Kind of function (Query, Mutation, Action).
    pub kind: FunctionKind,
    /// Whether authentication is required.
    pub requires_auth: bool,
    /// Required role (if any).
    pub required_role: Option<&'static str>,
    /// Whether this function is public (no auth).
    pub is_public: bool,
    /// Cache TTL in seconds (for queries).
    pub cache_ttl: Option<u64>,
    /// Timeout in seconds.
    pub timeout: Option<u64>,
}
```

---

## Context Objects

### QueryContext

```rust
pub struct QueryContext {
    /// Authentication context (public field).
    pub auth: AuthContext,
    /// Request metadata (public field).
    pub request: RequestMetadata,
    // Private: db_pool
}

impl QueryContext {
    /// Get a reference to the database pool.
    pub fn db(&self) -> &sqlx::PgPool;

    /// Get the authenticated user ID or return an Unauthorized error.
    pub fn require_user_id(&self) -> Result<Uuid>;
}
```

### MutationContext

```rust
pub struct MutationContext {
    /// Authentication context (public field).
    pub auth: AuthContext,
    /// Request metadata (public field).
    pub request: RequestMetadata,
    // Private: db_pool, job_dispatch, workflow_dispatch
}

impl MutationContext {
    /// Get a reference to the database pool.
    pub fn db(&self) -> &sqlx::PgPool;

    /// Get the authenticated user ID or return an Unauthorized error.
    pub fn require_user_id(&self) -> Result<Uuid>;

    /// Dispatch a background job by name.
    /// Returns the UUID of the dispatched job.
    pub async fn dispatch_job<T: Serialize>(
        &self,
        job_type: &str,
        args: T,
    ) -> Result<Uuid>;

    /// Start a workflow by name.
    /// Returns the UUID of the started workflow run.
    pub async fn start_workflow<T: Serialize>(
        &self,
        workflow_name: &str,
        input: T,
    ) -> Result<Uuid>;
}
```

### ActionContext

```rust
pub struct ActionContext {
    /// Authentication context (public field).
    pub auth: AuthContext,
    /// Request metadata (public field).
    pub request: RequestMetadata,
    // Private: db_pool, http_client, job_dispatch, workflow_dispatch
}

impl ActionContext {
    /// Get a reference to the database pool.
    pub fn db(&self) -> &sqlx::PgPool;

    /// Get a reference to the HTTP client for external API calls.
    pub fn http(&self) -> &reqwest::Client;

    /// Get the authenticated user ID or return an Unauthorized error.
    pub fn require_user_id(&self) -> Result<Uuid>;

    /// Dispatch a background job by name.
    pub async fn dispatch_job<T: Serialize>(
        &self,
        job_type: &str,
        args: T,
    ) -> Result<Uuid>;

    /// Start a workflow by name.
    pub async fn start_workflow<T: Serialize>(
        &self,
        workflow_name: &str,
        input: T,
    ) -> Result<Uuid>;
}
```

### AuthContext

Available on all context types via the `auth` field:

```rust
pub struct AuthContext {
    user_id: Option<Uuid>,
    roles: Vec<String>,
    claims: HashMap<String, serde_json::Value>,
    authenticated: bool,
}

impl AuthContext {
    /// Create an unauthenticated context.
    pub fn unauthenticated() -> Self;

    /// Create an authenticated context.
    pub fn authenticated(
        user_id: Uuid,
        roles: Vec<String>,
        claims: HashMap<String, serde_json::Value>,
    ) -> Self;

    /// Check if the user is authenticated.
    pub fn is_authenticated(&self) -> bool;

    /// Get the user ID if authenticated.
    pub fn user_id(&self) -> Option<Uuid>;

    /// Get the user ID, returning an error if not authenticated.
    pub fn require_user_id(&self) -> Result<Uuid>;

    /// Check if the user has a specific role.
    pub fn has_role(&self, role: &str) -> bool;

    /// Require a specific role, returning an error if not present.
    pub fn require_role(&self, role: &str) -> Result<()>;

    /// Get a custom claim value from JWT.
    pub fn claim(&self, key: &str) -> Option<&serde_json::Value>;

    /// Get all roles.
    pub fn roles(&self) -> &[String];
}
```

### RequestMetadata

Available on all context types via the `request` field:

```rust
pub struct RequestMetadata {
    /// Unique request ID for tracing.
    pub request_id: Uuid,
    /// Trace ID for distributed tracing.
    pub trace_id: String,
    /// Client IP address.
    pub client_ip: Option<String>,
    /// User agent string.
    pub user_agent: Option<String>,
    /// Request timestamp.
    pub timestamp: chrono::DateTime<chrono::Utc>,
}
```

---

## Proc Macros

The proc macros transform async functions into trait implementations.

### #[forge::query]

```rust
#[forge::query]
pub async fn get_users(ctx: &QueryContext) -> Result<Vec<User>> {
    sqlx::query_as::<_, User>("SELECT * FROM users ORDER BY created_at DESC")
        .fetch_all(ctx.db())
        .await
        .map_err(Into::into)
}
```

**Generated code:**
- Creates `GetUsersQuery` struct implementing `ForgeQuery`
- For functions with no extra arguments, `type Args = ()`
- The original function is preserved for direct calls

#### Query with Arguments

```rust
#[forge::query]
pub async fn get_user(ctx: &QueryContext, id: Uuid) -> Result<Option<User>> {
    sqlx::query_as::<_, User>("SELECT * FROM users WHERE id = $1")
        .bind(id)
        .fetch_optional(ctx.db())
        .await
        .map_err(Into::into)
}
```

**Generated code:**
- Creates `GetUserQueryArgs` struct with `pub id: Uuid`
- Creates `GetUserQuery` struct with `type Args = GetUserQueryArgs`

#### Query Attributes

```rust
#[forge::query(cache = "5m")]              // Cache for 5 minutes
#[forge::query(cache = "30s")]             // Cache for 30 seconds
#[forge::query(public)]                    // No authentication required
#[forge::query(require_auth)]              // Authentication required
#[forge::query(timeout = 60)]              // 60 second timeout
```

### #[forge::mutation]

```rust
#[forge::mutation]
pub async fn create_user(
    ctx: &MutationContext,
    email: String,
    name: String,
) -> Result<User> {
    let id = Uuid::new_v4();
    let now = Utc::now();

    let user = sqlx::query_as::<_, User>(
        "INSERT INTO users (id, email, name, created_at, updated_at) \
         VALUES ($1, $2, $3, $4, $5) RETURNING *",
    )
    .bind(id)
    .bind(&email)
    .bind(&name)
    .bind(now)
    .bind(now)
    .fetch_one(ctx.db())
    .await?;

    Ok(user)
}
```

**Generated code:**
- Creates `CreateUserMutationArgs` struct with `pub email: String, pub name: String`
- Creates `CreateUserMutation` struct implementing `ForgeMutation`

#### Mutation Attributes

```rust
#[forge::mutation(require_auth)]           // Authentication required
#[forge::mutation(require_role("admin"))]  // Specific role required
#[forge::mutation(timeout = 30)]           // 30 second timeout
```

### #[forge::action]

```rust
#[forge::action]
pub async fn sync_external_api(
    ctx: &ActionContext,
    user_id: Uuid,
) -> Result<SyncResult> {
    // Make external HTTP request
    let response = ctx.http()
        .get("https://api.example.com/sync")
        .send()
        .await?;

    // Process response...
    Ok(SyncResult { success: true })
}
```

**Generated code:**
- Creates `SyncExternalApiActionArgs` struct
- Creates `SyncExternalApiAction` struct implementing `ForgeAction`

#### Action Attributes

```rust
#[forge::action(require_auth)]             // Authentication required
#[forge::action(require_role("admin"))]    // Specific role required
#[forge::action(timeout = 120)]            // 120 second timeout
```

---

## Dispatching Jobs and Workflows

Both `MutationContext` and `ActionContext` support dispatching background jobs and starting workflows.

### From a Mutation

```rust
#[forge::mutation]
pub async fn create_user_with_onboarding(
    ctx: &MutationContext,
    email: String,
    name: String,
) -> Result<User> {
    // Create user in database
    let user = sqlx::query_as::<_, User>(
        "INSERT INTO users (id, email, name, created_at, updated_at) \
         VALUES ($1, $2, $3, NOW(), NOW()) RETURNING *"
    )
    .bind(Uuid::new_v4())
    .bind(&email)
    .bind(&name)
    .fetch_one(ctx.db())
    .await?;

    // Dispatch a background job
    let job_id = ctx.dispatch_job("send_welcome_email", serde_json::json!({
        "user_id": user.id,
        "email": &email,
    })).await?;
    tracing::info!(%job_id, "Dispatched welcome email job");

    // Start a workflow
    let workflow_id = ctx.start_workflow("user_onboarding", serde_json::json!({
        "user_id": user.id,
    })).await?;
    tracing::info!(%workflow_id, "Started onboarding workflow");

    Ok(user)
}
```

### From an Action

```rust
#[forge::action]
pub async fn process_webhook(
    ctx: &ActionContext,
    payload: WebhookPayload,
) -> Result<ProcessResult> {
    // Call external API
    let response = ctx.http()
        .post("https://api.example.com/process")
        .json(&payload)
        .send()
        .await?;

    // Dispatch follow-up job
    ctx.dispatch_job("sync_data", serde_json::json!({
        "source": "webhook",
        "id": payload.id,
    })).await?;

    Ok(ProcessResult { processed: true })
}
```

---

## Function Registration

Functions must be registered with the `FunctionRegistry` before the server starts:

```rust
use forge::prelude::*;

#[tokio::main]
async fn main() -> Result<()> {
    let config = ForgeConfig::from_file("forge.toml")?;
    let mut builder = Forge::builder();

    // Register queries
    builder.function_registry_mut().register_query::<GetUsersQuery>();
    builder.function_registry_mut().register_query::<GetUserQuery>();

    // Register mutations
    builder.function_registry_mut().register_mutation::<CreateUserMutation>();
    builder.function_registry_mut().register_mutation::<UpdateUserMutation>();
    builder.function_registry_mut().register_mutation::<DeleteUserMutation>();

    // Register actions (if any)
    // builder.function_registry_mut().register_action::<SyncExternalApiAction>();

    builder.config(config).build()?.run().await
}
```

---

## Runtime Execution

### FunctionRegistry

The `FunctionRegistry` stores all registered functions:

```rust
impl FunctionRegistry {
    /// Register a query function.
    pub fn register_query<Q: ForgeQuery>(&mut self);

    /// Register a mutation function.
    pub fn register_mutation<M: ForgeMutation>(&mut self);

    /// Register an action function.
    pub fn register_action<A: ForgeAction>(&mut self);

    /// Get a function by name.
    pub fn get(&self, name: &str) -> Option<&FunctionEntry>;

    /// Get all registered function names.
    pub fn function_names(&self) -> impl Iterator<Item = &str>;

    /// Get all queries.
    pub fn queries(&self) -> impl Iterator<Item = (&str, &FunctionInfo)>;

    /// Get all mutations.
    pub fn mutations(&self) -> impl Iterator<Item = (&str, &FunctionInfo)>;

    /// Get all actions.
    pub fn actions(&self) -> impl Iterator<Item = (&str, &FunctionInfo)>;
}
```

### FunctionRouter

The `FunctionRouter` handles authorization and routes calls to the appropriate handler:

```rust
impl FunctionRouter {
    /// Route and execute a function call.
    pub async fn route(
        &self,
        function_name: &str,
        args: Value,
        auth: AuthContext,
        request: RequestMetadata,
    ) -> Result<RouteResult>;
}
```

Authorization checks:
1. If `is_public` is true, no authentication required
2. If `requires_auth` is true, user must be authenticated
3. If `required_role` is set, user must have that role

### FunctionExecutor

The `FunctionExecutor` wraps the router with timeout handling:

```rust
impl FunctionExecutor {
    /// Execute a function with timeout.
    pub async fn execute(
        &self,
        function_name: &str,
        args: Value,
        auth: AuthContext,
        request: RequestMetadata,
    ) -> Result<ExecutionResult>;
}

pub struct ExecutionResult {
    pub function_name: String,
    pub function_kind: String,  // "query", "mutation", or "action"
    pub result: Value,
    pub duration: Duration,
    pub success: bool,
    pub error: Option<String>,
}
```

Timeouts:
- Uses function-specific timeout from `FunctionInfo.timeout` if set
- Falls back to default timeout (30 seconds)

---

## HTTP RPC Endpoint

Functions are called via the RPC endpoint:

```
POST /rpc
Content-Type: application/json

{
    "function": "get_users",
    "args": {}
}
```

Response:

```json
{
    "success": true,
    "data": [
        {"id": "...", "email": "...", "name": "..."}
    ]
}
```

Or with function name in URL:

```
POST /rpc/create_user
Content-Type: application/json

{
    "email": "user@example.com",
    "name": "John Doe"
}
```

---

## Frontend Integration

### TypeScript API Definition

```typescript
// Generated types
import { createQuery, createMutation } from '@forge/svelte';
import type { User } from './types';

export const getUsers = createQuery<Record<string, never>, User[]>('get_users');
export const getUser = createQuery<{ id: string }, User | null>('get_user');
export const createUser = createMutation<{ email: string; name: string }, User>('create_user');
export const deleteUser = createMutation<{ id: string }, boolean>('delete_user');
```

### Svelte Usage

```svelte
<script lang="ts">
    import { subscribe, mutate, query } from '@forge/svelte';
    import { getUsers, getUser, createUser } from '$lib/forge';

    // Real-time subscription (auto-updates on changes)
    const users = subscribe(getUsers, {});

    // One-time query
    async function loadUser(id: string) {
        const result = await query(getUser, { id });
        if (result.data) {
            selectedUser = result.data;
        }
    }

    // Mutation
    async function handleCreate() {
        await mutate(createUser, { email, name });
        // $users automatically updates via subscription!
    }
</script>

{#if $users.loading}
    <p>Loading...</p>
{:else if $users.data}
    {#each $users.data as user}
        <p>{user.name}</p>
    {/each}
{/if}
```

---

## Dispatch Traits

For dependency injection, dispatch capabilities are abstracted into traits:

### JobDispatch

```rust
// crates/forge-core/src/function/dispatch.rs

pub trait JobDispatch: Send + Sync {
    /// Dispatch a job by its registered name.
    fn dispatch_by_name(
        &self,
        job_type: &str,
        args: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<Uuid>> + Send + '_>>;
}
```

### WorkflowDispatch

```rust
pub trait WorkflowDispatch: Send + Sync {
    /// Start a workflow by its registered name.
    fn start_by_name(
        &self,
        workflow_name: &str,
        input: serde_json::Value,
    ) -> Pin<Box<dyn Future<Output = Result<Uuid>> + Send + '_>>;
}
```

These traits are implemented by `JobDispatcher` and `WorkflowExecutor` in `forge-runtime` and injected into contexts via the builder pattern:

```rust
let ctx = MutationContext::with_dispatch(
    db_pool,
    auth,
    request,
    Some(Arc::new(job_dispatcher)),
    Some(Arc::new(workflow_executor)),
);
```

---

## Best Practices

### Keep Functions Small

Each function should do one thing well. Compose multiple smaller functions instead of one large function.

### Use Actions for External Calls

Never call external APIs from mutations. Use actions or dispatch background jobs.

```rust
// Bad: External API in mutation
#[forge::mutation]
pub async fn bad_mutation(ctx: &MutationContext) -> Result<()> {
    // Don't do this in a mutation!
    reqwest::get("https://api.example.com").await?;
    Ok(())
}

// Good: Dispatch a job for external work
#[forge::mutation]
pub async fn good_mutation(ctx: &MutationContext, user_id: Uuid) -> Result<()> {
    // Create database record
    sqlx::query("INSERT INTO orders ...").execute(ctx.db()).await?;

    // Dispatch job for external API call
    ctx.dispatch_job("sync_external", serde_json::json!({ "user_id": user_id })).await?;
    Ok(())
}
```

### Validate Early

Validate input at the start of the function before any database operations.

### Use ctx.db() Not ctx.pool

The database pool is accessed via the `db()` method, not a public field:

```rust
// Correct
sqlx::query("...").fetch_all(ctx.db()).await?;

// Wrong - pool is private
// sqlx::query("...").fetch_all(ctx.pool).await?;
```

---

## Related Documentation

- [JOBS.md](JOBS.md) - Background job processing
- [WORKFLOWS.md](WORKFLOWS.md) - Multi-step durable workflows
- [CRONS.md](CRONS.md) - Scheduled task execution
- [REACTIVITY.md](REACTIVITY.md) - Real-time subscriptions

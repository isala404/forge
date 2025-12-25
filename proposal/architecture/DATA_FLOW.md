# Data Flow

> *How requests move through FORGE*

---

## Request Types Overview

FORGE handles several types of requests, each with different flow patterns:

| Request Type | Direction | Response | Example |
|--------------|-----------|----------|---------|
| Query | Client → Server | Immediate | Get user profile |
| Mutation | Client → Server | Immediate | Create project |
| Action | Client → Server | Immediate (may be slow) | Sync with Stripe |
| Subscription | Server → Client | Streaming | Live dashboard |
| Job Dispatch | Internal | Async | Process video |
| Webhook | External → Server | Immediate ACK | Stripe webhook |

---

## Query Flow

Queries are read-only functions that can be cached and subscribed to.

```
┌────────┐                                                              ┌──────────┐
│ Client │                                                              │ PostgreSQL│
└───┬────┘                                                              └─────┬────┘
    │                                                                         │
    │  1. query("getProjects", {userId: "abc"})                               │
    │─────────────────────────────────┐                                       │
    │                                 ▼                                       │
    │                         ┌──────────────┐                                │
    │                         │   Gateway    │                                │
    │                         │              │                                │
    │                         │ • Validate   │                                │
    │                         │ • Auth check │                                │
    │                         └──────┬───────┘                                │
    │                                │                                        │
    │                                │  2. Check cache                        │
    │                                ▼                                        │
    │                         ┌──────────────┐                                │
    │                         │ Query Cache  │                                │
    │                         │              │                                │
    │                         │ Cache hit?   │──── Yes ────┐                  │
    │                         └──────┬───────┘             │                  │
    │                                │ No                  │                  │
    │                                ▼                     │                  │
    │                         ┌──────────────┐             │                  │
    │                         │  Function    │             │                  │
    │                         │  Executor    │             │                  │
    │                         │              │             │                  │
    │                         │ 3. Execute   │             │                  │
    │                         │    query fn  │             │                  │
    │                         └──────┬───────┘             │                  │
    │                                │                     │                  │
    │                                │  4. SELECT ...      │                  │
    │                                │─────────────────────┼─────────────────►│
    │                                │                     │                  │
    │                                │◄────────────────────┼──────────────────│
    │                                │     Result rows     │                  │
    │                                │                     │                  │
    │                                ▼                     │                  │
    │                         ┌──────────────┐             │                  │
    │                         │ 5. Cache     │             │                  │
    │                         │    result    │             │                  │
    │                         └──────┬───────┘             │                  │
    │                                │                     │                  │
    │                                │◄────────────────────┘                  │
    │                                │                                        │
    │◄───────────────────────────────│                                        │
    │  6. Response: [{id: 1, ...}]   │                                        │
    │                                                                         │
```

### Query Caching

Cache keys are derived from:
- Function name
- Arguments (serialized)
- User context (if applicable)

```rust
// Cache key structure
let cache_key = format!(
    "query:{}:{}:{}",
    function_name,
    hash(arguments),
    user_id.unwrap_or("anonymous")
);
```

### Cache Invalidation

When data changes, related cache entries are invalidated:

```rust
// After a mutation that modifies projects:
cache.invalidate_pattern("query:getProjects:*");
cache.invalidate_pattern("query:getProject:*");
```

---

## Mutation Flow

Mutations modify data within a transaction.

```
┌────────┐                                                              ┌──────────┐
│ Client │                                                              │ PostgreSQL│
└───┬────┘                                                              └─────┬────┘
    │                                                                         │
    │  1. mutate("createProject", {name: "My App"})                           │
    │─────────────────────────────────┐                                       │
    │                                 ▼                                       │
    │                         ┌──────────────┐                                │
    │                         │   Gateway    │                                │
    │                         │              │                                │
    │                         │ • Validate   │                                │
    │                         │ • Auth check │                                │
    │                         │ • Rate limit │                                │
    │                         └──────┬───────┘                                │
    │                                │                                        │
    │                                ▼                                        │
    │                         ┌──────────────┐                                │
    │                         │  Function    │                                │
    │                         │  Executor    │                                │
    │                         │              │                                │
    │                         │ 2. BEGIN     │                                │
    │                         │    transaction│                               │
    │                         └──────┬───────┘                                │
    │                                │                                        │
    │                                │  3. BEGIN                              │
    │                                │─────────────────────────────────────── │
    │                                │                                        │
    │                                │  4. INSERT INTO projects ...           │
    │                                │───────────────────────────────────────►│
    │                                │                                        │
    │                                │◄───────────────────────────────────────│
    │                                │     OK                                 │
    │                                │                                        │
    │                                │  5. INSERT INTO events (change log)    │
    │                                │───────────────────────────────────────►│
    │                                │                                        │
    │                                │  6. COMMIT                             │
    │                                │───────────────────────────────────────►│
    │                                │                                        │
    │                                ▼                                        │
    │                         ┌──────────────┐                                │
    │                         │ 7. Invalidate│                                │
    │                         │    caches    │                                │
    │                         └──────┬───────┘                                │
    │                                │                                        │
    │                                ▼                                        │
    │                         ┌──────────────┐                                │
    │                         │ 8. Notify    │                                │
    │                         │ subscribers  │───────────────────────────────►│
    │                         │ (NOTIFY)     │                                │
    │                         └──────┬───────┘                                │
    │                                │                                        │
    │◄───────────────────────────────│                                        │
    │  9. Response: {id: "new-id"}   │                                        │
```

### Transaction Guarantees

All mutations run in **serializable isolation**:

```rust
#[forge::mutation]
pub async fn transfer_funds(ctx: &MutationContext, from: Uuid, to: Uuid, amount: Decimal) -> Result<()> {
    // This entire function is ONE transaction
    // If anything fails, everything rolls back
    
    let from_account = ctx.db.get::<Account>(from).await?;
    
    if from_account.balance < amount {
        return Err(Error::InsufficientFunds);
    }
    
    ctx.db.update(from, |a| a.balance -= amount).await?;
    ctx.db.update(to, |a| a.balance += amount).await?;
    
    // Both updates commit together, or neither does
    Ok(())
}
```

→ See [PostgreSQL Schema](../database/POSTGRES_SCHEMA.md#transactions) for isolation details.

---

## Action Flow

Actions can call external services and are NOT transactional by default.

```
┌────────┐                                                    ┌─────────┐
│ Client │                                                    │ External│
└───┬────┘                                                    │   API   │
    │                                                         └────┬────┘
    │  1. action("syncWithStripe", {userId: "abc"})                │
    │─────────────────────────────────┐                            │
    │                                 ▼                            │
    │                         ┌──────────────┐                     │
    │                         │   Gateway    │                     │
    │                         └──────┬───────┘                     │
    │                                │                             │
    │                                ▼                             │
    │                         ┌──────────────┐                     │
    │                         │  Function    │                     │
    │                         │  Executor    │                     │
    │                         │              │                     │
    │                         │ 2. Run query │                     │
    │                         │    (get user)│                     │
    │                         └──────┬───────┘                     │
    │                                │                             │
    │                                │  3. Call Stripe API         │
    │                                │─────────────────────────────►
    │                                │                             │
    │                                │◄─────────────────────────────
    │                                │     Stripe response         │
    │                                │                             │
    │                                ▼                             │
    │                         ┌──────────────┐                     │
    │                         │ 4. Run       │                     │
    │                         │   mutation   │                     │
    │                         │  (update DB) │                     │
    │                         └──────┬───────┘                     │
    │                                │                             │
    │◄───────────────────────────────│                             │
    │  5. Response: {synced: true}   │                             │
```

### Action Composition

Actions call queries and mutations as separate transactions:

```rust
#[forge::action]
pub async fn sync_with_stripe(ctx: &ActionContext, user_id: Uuid) -> Result<SyncResult> {
    // Query (read-only, cached)
    let user = ctx.query(get_user, user_id).await?;
    
    // External call (NOT transactional)
    let stripe_data = stripe::Customer::retrieve(&user.stripe_id).await?;
    
    // Mutation (transactional)
    ctx.mutate(update_subscription, UpdateSubscription {
        user_id,
        plan: stripe_data.plan,
    }).await?;
    
    Ok(SyncResult::success())
}
```

---

## Subscription Flow

Subscriptions provide real-time updates when query results change.

```
┌────────┐           ┌──────────┐           ┌──────────┐           ┌──────────┐
│ Client │           │  Gateway │           │ Function │           │PostgreSQL│
└───┬────┘           └────┬─────┘           └────┬─────┘           └────┬─────┘
    │                     │                      │                      │
    │  1. subscribe("getProjects", {userId})     │                      │
    │────────────────────►│                      │                      │
    │                     │                      │                      │
    │                     │  2. Register         │                      │
    │                     │     subscription     │                      │
    │                     │─────────────────────►│                      │
    │                     │                      │                      │
    │                     │                      │  3. Execute query    │
    │                     │                      │─────────────────────►│
    │                     │                      │◄─────────────────────│
    │                     │                      │                      │
    │                     │                      │  4. Track read set   │
    │                     │                      │  (tables/rows read)  │
    │                     │                      │                      │
    │◄────────────────────│◄─────────────────────│                      │
    │  5. Initial result  │                      │                      │
    │                     │                      │                      │
    │         ... time passes ...                │                      │
    │                     │                      │                      │
    │                     │                      │  6. NOTIFY on change │
    │                     │                      │◄─────────────────────│
    │                     │                      │                      │
    │                     │  7. Check: does this │                      │
    │                     │     affect any       │                      │
    │                     │     subscriptions?   │                      │
    │                     │─────────────────────►│                      │
    │                     │                      │                      │
    │                     │                      │  8. Yes, re-execute  │
    │                     │                      │─────────────────────►│
    │                     │                      │◄─────────────────────│
    │                     │                      │                      │
    │                     │  9. Compute delta    │                      │
    │                     │◄─────────────────────│                      │
    │                     │                      │                      │
    │◄────────────────────│                      │                      │
    │  10. Delta update   │                      │                      │
    │  {added: [...]}     │                      │                      │
```

### Read Set Tracking

FORGE tracks what data each subscription read:

```rust
struct SubscriptionState {
    query_name: String,
    args: Value,
    user_id: Option<Uuid>,
    
    // What was read during last execution
    read_set: ReadSet,
    
    // Last result (for delta computation)
    last_result: Value,
}

struct ReadSet {
    tables: HashSet<String>,
    rows: HashMap<String, HashSet<RowId>>,  // table -> row IDs
}
```

When a change event comes in:

```rust
fn should_rerun_subscription(sub: &SubscriptionState, event: &ChangeEvent) -> bool {
    // Check if the changed table is in the read set
    if !sub.read_set.tables.contains(&event.table) {
        return false;
    }
    
    // Check if the specific row was read
    if let Some(rows) = sub.read_set.rows.get(&event.table) {
        return rows.contains(&event.row_id);
    }
    
    // Table was read but we don't have row-level tracking
    // Conservatively re-run
    true
}
```

→ See [Reactivity](../core/REACTIVITY.md) for detailed subscription mechanics.

---

## Job Flow

Jobs are dispatched asynchronously and processed by workers.

```
┌────────┐       ┌──────────┐       ┌──────────┐       ┌──────────┐       ┌──────────┐
│ Client │       │ Mutation │       │PostgreSQL│       │ Scheduler│       │  Worker  │
└───┬────┘       └────┬─────┘       └────┬─────┘       └────┬─────┘       └────┬─────┘
    │                 │                  │                  │                  │
    │  1. mutate()    │                  │                  │                  │
    │────────────────►│                  │                  │                  │
    │                 │                  │                  │                  │
    │                 │  2. INSERT job   │                  │                  │
    │                 │  (status=pending)│                  │                  │
    │                 │─────────────────►│                  │                  │
    │                 │                  │                  │                  │
    │◄────────────────│  3. Return       │                  │                  │
    │  job_id         │     job_id       │                  │                  │
    │                 │                  │                  │                  │
    │                 │                  │  4. NOTIFY       │                  │
    │                 │                  │─────────────────►│                  │
    │                 │                  │                  │                  │
    │                 │                  │                  │  5. Assign job   │
    │                 │                  │                  │  to worker       │
    │                 │                  │                  │─────────────────►│
    │                 │                  │                  │                  │
    │                 │                  │                  │                  │  6. SELECT
    │                 │                  │                  │                  │  FOR UPDATE
    │                 │                  │◄─────────────────────────────────────│
    │                 │                  │                  │                  │  SKIP LOCKED
    │                 │                  │─────────────────────────────────────►│
    │                 │                  │                  │                  │
    │                 │                  │                  │                  │  7. Execute
    │                 │                  │                  │                  │     job
    │                 │                  │                  │                  │
    │                 │                  │                  │  8. Progress     │
    │                 │                  │                  │◄─────────────────│
    │                 │                  │                  │                  │
    │                 │                  │  9. UPDATE       │                  │
    │                 │                  │  job status      │                  │
    │                 │                  │◄─────────────────────────────────────│
    │                 │                  │                  │                  │
```

### Job States

```
┌─────────┐     ┌─────────┐     ┌─────────┐     ┌─────────┐
│ PENDING │────►│ CLAIMED │────►│ RUNNING │────►│COMPLETE │
└─────────┘     └─────────┘     └─────────┘     └─────────┘
                     │               │
                     │               │
                     ▼               ▼
               ┌─────────┐     ┌─────────┐
               │  RETRY  │     │ FAILED  │
               └────┬────┘     └────┬────┘
                    │               │
                    │               ▼
                    │         ┌─────────┐
                    └────────►│  DEAD   │
                              │ LETTER  │
                              └─────────┘
```

→ See [Jobs](../core/JOBS.md) and [Job Queue](../database/JOB_QUEUE.md).

---

## Webhook Flow

External webhooks are acknowledged immediately, processed asynchronously.

```
┌──────────┐       ┌──────────┐       ┌──────────┐       ┌──────────┐
│  Stripe  │       │  Gateway │       │PostgreSQL│       │  Worker  │
└────┬─────┘       └────┬─────┘       └────┬─────┘       └────┬─────┘
     │                  │                  │                  │
     │  1. POST         │                  │                  │
     │  /webhooks/stripe│                  │                  │
     │─────────────────►│                  │                  │
     │                  │                  │                  │
     │                  │  2. Verify       │                  │
     │                  │     signature    │                  │
     │                  │                  │                  │
     │                  │  3. INSERT job   │                  │
     │                  │  (webhook event) │                  │
     │                  │─────────────────►│                  │
     │                  │                  │                  │
     │◄─────────────────│                  │                  │
     │  4. 200 OK       │                  │                  │
     │  (< 100ms)       │                  │                  │
     │                  │                  │                  │
     │                  │                  │  ... async ...   │
     │                  │                  │                  │
     │                  │                  │                  │  5. Process
     │                  │                  │◄─────────────────│     webhook
     │                  │                  │─────────────────►│
     │                  │                  │                  │
```

### Webhook Deduplication

Webhooks are deduplicated by event ID:

```rust
#[forge::webhook("POST /webhooks/stripe")]
#[idempotent(key = "event.id")]  // Dedupe by Stripe event ID
pub async fn stripe_webhook(ctx: &WebhookContext, event: StripeEvent) -> Result<Response> {
    // Even if Stripe sends the same event twice,
    // we only process it once
    ctx.dispatch_job(process_stripe_event, event).await?;
    Ok(Response::accepted())
}
```

---

## Cross-Node Communication

When a request needs to execute on a different node:

```
┌──────────┐                   ┌──────────┐                   ┌──────────┐
│  Node A  │                   │  Node B  │                   │  Node C  │
│ (gateway)│                   │(function)│                   │ (worker) │
└────┬─────┘                   └────┬─────┘                   └────┬─────┘
     │                              │                              │
     │  1. Client request           │                              │
     │  lands here                  │                              │
     │                              │                              │
     │  2. Node A is busy,          │                              │
     │  forward to Node B           │                              │
     │                              │                              │
     │  gRPC: ExecuteFunction       │                              │
     │─────────────────────────────►│                              │
     │                              │                              │
     │                              │  3. Execute query            │
     │                              │                              │
     │◄─────────────────────────────│                              │
     │  4. Result                   │                              │
     │                              │                              │
     │  5. Job dispatched           │                              │
     │                              │                              │
     │  gRPC: ClaimJob              │                              │
     │──────────────────────────────┼─────────────────────────────►│
     │                              │                              │
     │                              │                              │  6. Execute job
     │                              │                              │
```

### Load Balancing

The gateway selects a target node based on:

1. **Locality**: Prefer executing locally if possible
2. **Load**: Route to least-loaded node
3. **Capability**: Route to nodes with required capability (for workers)

```rust
fn select_executor_node(cluster: &Cluster, request: &Request) -> NodeId {
    let eligible = cluster.nodes()
        .filter(|n| n.has_role(Role::Function))
        .filter(|n| n.status == NodeStatus::Active);
    
    // Prefer local execution
    if eligible.contains(&cluster.self_id) && self_load < 0.8 {
        return cluster.self_id;
    }
    
    // Otherwise, pick least loaded
    eligible.min_by_key(|n| n.current_load).unwrap()
}
```

---

## Trace Propagation

Every request gets a trace ID that follows it across all operations:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                      TRACE: abc123                                           │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  ┌─────────────────────────────────────────────────────────────────────┐    │
│  │ Span: http.request                                     [Node A]     │    │
│  │ Duration: 145ms                                                     │    │
│  │                                                                     │    │
│  │  ┌─────────────────────────────────────────────────────────────┐   │    │
│  │  │ Span: function.execute (getProjects)           [Node A]     │   │    │
│  │  │ Duration: 120ms                                              │   │    │
│  │  │                                                              │   │    │
│  │  │  ┌─────────────────────────────────────────────────────┐    │   │    │
│  │  │  │ Span: db.query                         [PostgreSQL]  │    │   │    │
│  │  │  │ Duration: 15ms                                       │    │   │    │
│  │  │  │ Query: SELECT * FROM projects WHERE owner_id = ...   │    │   │    │
│  │  │  └─────────────────────────────────────────────────────┘    │   │    │
│  │  │                                                              │   │    │
│  │  │  ┌─────────────────────────────────────────────────────┐    │   │    │
│  │  │  │ Span: db.query                         [PostgreSQL]  │    │   │    │
│  │  │  │ Duration: 8ms                                        │    │   │    │
│  │  │  │ Query: SELECT * FROM users WHERE id = ...            │    │   │    │
│  │  │  └─────────────────────────────────────────────────────┘    │   │    │
│  │  │                                                              │   │    │
│  │  └─────────────────────────────────────────────────────────────┘   │    │
│  │                                                                     │    │
│  │  ┌─────────────────────────────────────────────────────────────┐   │    │
│  │  │ Span: job.dispatch (processAnalytics)      [Node A]         │   │    │
│  │  │ Duration: 2ms                                                │   │    │
│  │  └─────────────────────────────────────────────────────────────┘   │    │
│  │                                                                     │    │
│  └─────────────────────────────────────────────────────────────────────┘    │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

Trace context is propagated via:
- HTTP headers (`X-Trace-Id`, `X-Span-Id`)
- gRPC metadata
- Job payload metadata

→ See [Tracing](../observability/TRACING.md) for implementation details.

---

## Related Documentation

- [Functions](../core/FUNCTIONS.md) — Query, Mutation, Action details
- [Reactivity](../core/REACTIVITY.md) — Subscription system
- [Jobs](../core/JOBS.md) — Background job processing
- [Job Queue](../database/JOB_QUEUE.md) — PostgreSQL-based queue
- [Meshing](../cluster/MESHING.md) — Inter-node communication

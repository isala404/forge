# Reactivity

> *Real-time updates without the complexity*

---

## Overview

FORGE provides **automatic real-time updates**. When data changes, subscribed clients receive updates instantly—no manual WebSocket management, no pub/sub setup.

```svelte
<script>
  import { subscribe } from '$lib/forge';
  
  // This automatically updates when ANY mutation changes projects
  const projects = subscribe(get_projects, { userId: $user.id });
</script>

{#each $projects as project}
  <ProjectCard {project} />
{/each}
```

---

## How It Works

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                      REACTIVITY SYSTEM                                       │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  1. CLIENT SUBSCRIBES                                                        │
│  ┌──────────────┐         ┌──────────────┐                                  │
│  │    Client    │────────►│   Gateway    │                                  │
│  │  subscribe() │ WS      │              │                                  │
│  └──────────────┘         └──────┬───────┘                                  │
│                                  │                                           │
│  2. INITIAL QUERY                │                                           │
│                                  ▼                                           │
│                           ┌──────────────┐         ┌──────────────┐         │
│                           │   Function   │────────►│  PostgreSQL  │         │
│                           │   Executor   │◄────────│              │         │
│                           └──────┬───────┘         └──────────────┘         │
│                                  │                                           │
│  3. TRACK READ SET               │                                           │
│  ┌───────────────────────────────┴───────────────────────────────────┐      │
│  │  Subscription State:                                               │      │
│  │  - Query: get_projects                                             │      │
│  │  - Args: { userId: "abc" }                                         │      │
│  │  - Read tables: [projects, users]                                  │      │
│  │  - Read rows: { projects: [1, 2, 3], users: ["abc"] }              │      │
│  │  - Last result hash: "a1b2c3..."                                   │      │
│  └───────────────────────────────────────────────────────────────────┘      │
│                                                                              │
│  4. DATA CHANGES (mutation elsewhere)                                        │
│                           ┌──────────────┐                                  │
│                           │   Mutation   │──► INSERT INTO projects...       │
│                           │  (any node)  │                                  │
│                           └──────┬───────┘                                  │
│                                  │                                           │
│  5. CHANGE NOTIFICATION          │ NOTIFY                                    │
│                                  ▼                                           │
│                           ┌──────────────┐                                  │
│                           │  PostgreSQL  │──► forge_changes channel         │
│                           │   NOTIFY     │                                  │
│                           └──────┬───────┘                                  │
│                                  │                                           │
│  6. CHECK SUBSCRIPTIONS          │                                           │
│                                  ▼                                           │
│                           ┌──────────────┐                                  │
│                           │   Gateway    │                                  │
│                           │  (all nodes) │                                  │
│                           └──────┬───────┘                                  │
│                                  │                                           │
│  7. RE-EXECUTE IF AFFECTED       │ projects table changed                    │
│                                  ▼                                           │
│                           ┌──────────────┐                                  │
│                           │  Re-run      │                                  │
│                           │  get_projects│                                  │
│                           └──────┬───────┘                                  │
│                                  │                                           │
│  8. COMPUTE & SEND DELTA         │                                           │
│                                  ▼                                           │
│                           ┌──────────────┐         ┌──────────────┐         │
│                           │   Gateway    │────────►│    Client    │         │
│                           │              │ WS      │  (updated!)  │         │
│                           └──────────────┘         └──────────────┘         │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Subscribing to Queries

### Basic Subscription

```svelte
<script>
  import { subscribe } from '$lib/forge';
  import { get_projects } from '$lib/forge/api';
  
  const projects = subscribe(get_projects, { userId: $user.id });
</script>

{#if $projects.loading}
  <Spinner />
{:else if $projects.error}
  <Error message={$projects.error.message} />
{:else}
  {#each $projects.data as project (project.id)}
    <ProjectCard {project} />
  {/each}
{/if}
```

### Subscription States

```typescript
interface SubscriptionState<T> {
  loading: boolean;      // Initial load in progress
  data: T | null;        // Current data
  error: Error | null;   // Error if any
  stale: boolean;        // Reconnecting, data may be outdated
}
```

### Reactive Arguments

When arguments change, the subscription automatically re-subscribes:

```svelte
<script>
  import { subscribe } from '$lib/forge';
  
  let selectedUserId = $state('');
  
  // Automatically re-subscribes when selectedUserId changes
  const projects = subscribe(get_projects, () => ({ 
    userId: selectedUserId 
  }));
</script>

<select bind:value={selectedUserId}>
  {#each users as user}
    <option value={user.id}>{user.name}</option>
  {/each}
</select>

{#each $projects.data ?? [] as project}
  <ProjectCard {project} />
{/each}
```

---

## Query Invalidation

When a mutation modifies data, related queries are automatically invalidated:

```rust
#[forge::mutation]
pub async fn create_project(ctx: &MutationContext, input: CreateProjectInput) -> Result<Project> {
    let project = ctx.db.insert(Project { ... }).await?;
    
    // FORGE automatically:
    // 1. Detects that 'projects' table changed
    // 2. Finds all subscriptions that read from 'projects'
    // 3. Re-runs those queries
    // 4. Sends deltas to clients
    
    Ok(project)
}
```

### How Invalidation Is Tracked

```rust
// During query execution, FORGE tracks:
struct ReadSet {
    // Tables accessed
    tables: HashSet<String>,  // ["projects", "users"]
    
    // Specific rows read (for fine-grained invalidation)
    rows: HashMap<String, HashSet<Uuid>>,  // { "projects": [id1, id2] }
    
    // Columns used in filters (for smarter invalidation)
    filter_columns: HashMap<String, HashSet<String>>,
}

// After mutation, FORGE checks:
fn should_invalidate(subscription: &Subscription, change: &Change) -> bool {
    // Table-level check
    if !subscription.read_set.tables.contains(&change.table) {
        return false;
    }
    
    // Row-level check (if available)
    if let Some(rows) = subscription.read_set.rows.get(&change.table) {
        if change.operation == Operation::Update || change.operation == Operation::Delete {
            return rows.contains(&change.row_id);
        }
        // For inserts, we need to re-run to see if new row matches filters
    }
    
    true  // Conservative: invalidate if unsure
}
```

---

## Delta Updates

Instead of sending the full result every time, FORGE computes and sends deltas:

```typescript
// Delta format
interface Delta<T> {
  added: T[];      // New items
  removed: string[];  // IDs of removed items
  updated: Partial<T>[];  // Changed fields only
}

// Example delta
{
  "added": [
    { "id": "new-1", "name": "New Project", ... }
  ],
  "removed": ["deleted-id"],
  "updated": [
    { "id": "existing-1", "name": "Renamed Project" }  // Only changed fields
  ]
}
```

### Client-Side Merge

The client library automatically merges deltas:

```typescript
// Internal client implementation
function applyDelta<T>(current: T[], delta: Delta<T>): T[] {
  let result = current.filter(item => !delta.removed.includes(item.id));
  
  for (const update of delta.updated) {
    const index = result.findIndex(item => item.id === update.id);
    if (index >= 0) {
      result[index] = { ...result[index], ...update };
    }
  }
  
  result.push(...delta.added);
  
  return result;
}
```

---

## Optimistic Updates

For instant UI feedback, use optimistic updates:

```svelte
<script>
  import { subscribe, mutate } from '$lib/forge';
  
  const projects = subscribe(get_projects, { userId: $user.id });
  
  async function createProject(name: string) {
    // Optimistically add to local state
    const optimisticProject = {
      id: crypto.randomUUID(),
      name,
      status: 'draft',
      _optimistic: true,  // Mark as optimistic
    };
    
    $projects.data = [...$projects.data, optimisticProject];
    
    try {
      // Actually create
      const real = await mutate(create_project, { name });
      
      // Replace optimistic with real
      $projects.data = $projects.data
        .filter(p => p.id !== optimisticProject.id)
        .concat(real);
    } catch (error) {
      // Revert on failure
      $projects.data = $projects.data.filter(p => p.id !== optimisticProject.id);
      throw error;
    }
  }
</script>
```

### Built-in Optimistic Support

```svelte
<script>
  import { mutateOptimistic } from '$lib/forge';
  
  async function handleCreate() {
    await mutateOptimistic(create_project, {
      input: { name: 'New Project' },
      
      // Optimistic update applied immediately
      optimistic: (current) => [...current, { 
        id: 'temp', 
        name: 'New Project',
        status: 'draft' 
      }],
      
      // Rollback on error
      rollback: (current, error) => current.filter(p => p.id !== 'temp'),
    });
  }
</script>
```

---

## Subscription Lifecycle

### Connection Management

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                    WEBSOCKET LIFECYCLE                                       │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  ┌─────────────┐                                                             │
│  │ CONNECTING  │ ◄─── Initial connection                                     │
│  └──────┬──────┘                                                             │
│         │                                                                    │
│         │ Connection established                                             │
│         ▼                                                                    │
│  ┌─────────────┐                                                             │
│  │  CONNECTED  │ ◄─── Subscriptions active                                   │
│  └──────┬──────┘                                                             │
│         │                                                                    │
│         ├─── Network issue ───────────────────────────┐                      │
│         │                                             │                      │
│         │                                             ▼                      │
│         │                                      ┌─────────────┐               │
│         │                                      │RECONNECTING │               │
│         │                                      │ (stale=true)│               │
│         │                                      └──────┬──────┘               │
│         │                                             │                      │
│         │◄──────────── Reconnected ───────────────────┘                      │
│         │              (re-sync all subscriptions)                           │
│         │                                                                    │
│         │ User navigates away / component unmounts                           │
│         ▼                                                                    │
│  ┌─────────────┐                                                             │
│  │DISCONNECTED │                                                             │
│  └─────────────┘                                                             │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Automatic Reconnection

```typescript
// Client configuration
const forge = createForgeClient({
  url: 'wss://api.example.com',
  
  reconnect: {
    enabled: true,
    maxAttempts: 10,
    delay: 1000,        // Start with 1s
    maxDelay: 30000,    // Max 30s
    backoff: 'exponential',
  },
  
  // Called on reconnect
  onReconnect: () => {
    console.log('Reconnected! Subscriptions will re-sync.');
  },
});
```

---

## Cross-Node Subscriptions

Subscriptions work across the cluster:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                    CROSS-NODE SUBSCRIPTION                                   │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  Client A connected to Node 1                                                │
│  Client B connected to Node 2                                                │
│  Both subscribed to get_projects                                             │
│                                                                              │
│  ┌─────────────┐              ┌─────────────┐              ┌─────────────┐  │
│  │   Node 1    │◄────────────►│   Node 2    │◄────────────►│   Node 3    │  │
│  │  Client A   │    gRPC      │  Client B   │    gRPC      │             │  │
│  └──────┬──────┘              └──────┬──────┘              └──────┬──────┘  │
│         │                            │                            │         │
│         │                            │                            │         │
│         │                            │         Mutation executed  │         │
│         │                            │         on Node 3          │         │
│         │                            │                            │         │
│         │                            │                    INSERT INTO       │
│         │                            │                    projects...       │
│         │                            │                            │         │
│         │                            │                            ▼         │
│         │                            │              ┌─────────────────────┐ │
│         │                            │              │     PostgreSQL      │ │
│         │                            │              │   NOTIFY trigger    │ │
│         │                            │              └──────────┬──────────┘ │
│         │                            │                         │            │
│         │                            │         LISTEN          │            │
│         │◄───────────────────────────┼─────────────────────────┘            │
│         │                            │◄────────────────────────┘            │
│         │                            │                                      │
│         │  Re-run query              │  Re-run query                        │
│         │  Send delta to A           │  Send delta to B                     │
│         │                            │                                      │
│         ▼                            ▼                                      │
│  ┌─────────────┐              ┌─────────────┐                               │
│  │  Client A   │              │  Client B   │                               │
│  │  (updated!) │              │  (updated!) │                               │
│  └─────────────┘              └─────────────┘                               │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Cross-Node Propagation Latency

PostgreSQL `NOTIFY` is the coordination mechanism for cross-node subscription updates. Understanding its latency characteristics:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                    CROSS-NODE LATENCY BREAKDOWN                              │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  Mutation on Node A → Client on Node B receives update                       │
│                                                                              │
│  Step                                    Typical Latency                     │
│  ────────────────────────────────────────────────────────                   │
│  1. Mutation executes                    1-10ms (query time)                │
│  2. Trigger fires NOTIFY                 <1ms                               │
│  3. PostgreSQL delivers to listeners     1-5ms                              │
│  4. Node B receives notification         1-2ms (network)                    │
│  5. Node B checks affected subs          <1ms                               │
│  6. Query re-execution (if needed)       1-50ms (query time)                │
│  7. Delta computation                    <1ms                               │
│  8. WebSocket send                       <1ms                               │
│  ────────────────────────────────────────────────────────                   │
│  Total (simple query):                   5-20ms                             │
│  Total (complex query):                  10-100ms                           │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

**Expected latencies by scenario:**

| Scenario | Latency | Notes |
|----------|---------|-------|
| Same node (mutation → same client) | 2-10ms | No network hop |
| Cross-node (simple query) | 5-20ms | NOTIFY + re-execute |
| Cross-node (complex query) | 10-100ms | Dominated by query time |
| High load (>10K changes/sec) | +50-200ms | Debounce kicks in |

**When sub-10ms latency is critical:**

For real-time games or trading, where PostgreSQL latency is too high, consider:

```toml
# forge.toml

[reactivity]
# Bypass PostgreSQL NOTIFY, use gRPC mesh directly
# Adds memory overhead but reduces latency by ~5-10ms
direct_mesh_propagation = true

# Only for specific high-priority queries
[reactivity.fast_path]
queries = ["get_live_positions", "get_order_book"]
# These use gRPC broadcast instead of NOTIFY
```

**Note:** For 99% of SaaS applications, the default PostgreSQL-based propagation (5-20ms) is more than sufficient. Only enable `direct_mesh_propagation` if you've measured that NOTIFY latency is your actual bottleneck.

---

## Read-Set Tracking & Memory Management

Subscriptions track which data they read to enable precise invalidation. This has memory implications.

### Tracking Modes

```toml
# forge.toml

[subscriptions]
# Default tracking mode
tracking_mode = "table"  # or "row" or "adaptive"
```

| Mode | Memory per sub | Invalidation precision | Best for |
|------|---------------|----------------------|----------|
| `table` | ~400 bytes | Coarse (any change to table) | High fan-out, simple queries |
| `row` | ~400 bytes + 100 bytes/row | Precise (only tracked rows) | Filtered queries, low fan-out |
| `adaptive` | Varies | Automatic selection | General use (recommended) |

### Adaptive Mode (Default)

FORGE automatically chooses tracking granularity based on query characteristics:

```rust
fn choose_tracking_mode(query_stats: &QueryStats) -> TrackingMode {
    // Small result set → track rows precisely
    if query_stats.avg_rows < 50 {
        return TrackingMode::Row { max_rows: 100 };
    }

    // Large result set → track tables only (cheaper)
    if query_stats.avg_rows > 500 {
        return TrackingMode::Table;
    }

    // High update frequency → table-level (less re-computation)
    if query_stats.updates_per_minute > 100 {
        return TrackingMode::Table;
    }

    // Default: row-level with limit
    TrackingMode::Row { max_rows: 100 }
}
```

### Memory Budget Configuration

```toml
# forge.toml

[subscriptions.memory]
# Total memory budget for subscription tracking per node
budget = "500MB"

# When approaching budget, degrade to table-level tracking
degrade_threshold = "400MB"

# Maximum rows tracked per subscription in row mode
max_tracked_rows = 100

# Subscriptions exceeding row limit auto-switch to table mode
auto_degrade = true
```

### Memory Pressure Handling

When memory budget is approached:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                    MEMORY PRESSURE RESPONSE                                  │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│  Memory usage: ━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━░░░░░ 80% of budget           │
│                                                                              │
│  Stage 1 (70%): Log warning                                                  │
│    "Subscription memory at 70%, consider reducing max_tracked_rows"         │
│                                                                              │
│  Stage 2 (80%): Degrade new subscriptions                                    │
│    - New subscriptions default to table-level tracking                       │
│    - Existing row-level subscriptions preserved                              │
│                                                                              │
│  Stage 3 (90%): Degrade existing subscriptions                               │
│    - Convert largest row-level subscriptions to table-level                  │
│    - Log which subscriptions were degraded                                   │
│                                                                              │
│  Stage 4 (95%): Reject new subscriptions                                     │
│    - Return error to client: "Server at capacity, retry later"              │
│    - Existing subscriptions continue working                                 │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

### Monitoring Memory Usage

```sql
-- Via dashboard SQL or direct query
SELECT
    tracking_mode,
    count(*) as subscription_count,
    pg_size_pretty(sum(memory_bytes)) as total_memory,
    avg(tracked_row_count) as avg_tracked_rows,
    max(tracked_row_count) as max_tracked_rows
FROM forge_subscription_stats
GROUP BY tracking_mode;

-- Find memory-heavy subscriptions
SELECT
    query_name,
    count(*) as instances,
    pg_size_pretty(sum(memory_bytes)) as total_memory
FROM forge_subscription_stats
WHERE tracking_mode = 'row'
GROUP BY query_name
ORDER BY sum(memory_bytes) DESC
LIMIT 10;
```

### Optimizing for Memory

**1. Use table-level tracking for high-frequency updates:**

```rust
#[forge::query]
#[tracking = "table"]  // Override default
pub async fn get_live_feed(ctx: &QueryContext) -> Result<Vec<FeedItem>> {
    // This query's data changes very frequently
    // Row-level tracking would be expensive and provide little benefit
    ctx.db.query::<FeedItem>()
        .order_by(|f| f.created_at.desc())
        .limit(100)
        .fetch_all()
        .await
}
```

**2. Limit result sizes:**

```rust
#[forge::query]
pub async fn get_projects(ctx: &QueryContext, page: Page) -> Result<Paginated<Project>> {
    // Pagination limits tracked rows
    ctx.db.query::<Project>()
        .order_by(|p| p.created_at.desc())
        .paginate(page)  // max 50 rows tracked
        .await
}
```

**3. Use focused queries:**

```rust
// ❌ Tracks all projects (potentially thousands of rows)
pub async fn get_all_projects(ctx: &QueryContext) -> Result<Vec<Project>> {
    ctx.db.query::<Project>().fetch_all().await
}

// ✅ Tracks only user's projects (typically <100 rows)
pub async fn get_user_projects(ctx: &QueryContext, user_id: Uuid) -> Result<Vec<Project>> {
    ctx.db.query::<Project>()
        .filter(|p| p.owner_id == user_id)
        .fetch_all()
        .await
}
```

---

## Performance Considerations

### Subscription Limits

```toml
# forge.toml

[gateway]
# Per-connection limits
max_subscriptions_per_connection = 50
subscription_timeout = "30s"

# Rate limiting for subscription creation
subscription_rate_limit = 100  # per minute per connection
```

### Debouncing & Coalescing

When mutations happen rapidly (bulk imports, batch updates), naive re-execution would overwhelm both the database and clients. FORGE uses a simple debounce + coalesce strategy:

```toml
# forge.toml

[reactivity]
# Debounce window - wait this long after a change before re-executing
debounce_ms = 50

# Maximum wait - even if changes keep coming, re-execute after this
max_debounce_ms = 200

# Coalesce by table - group changes to same table
coalesce_by_table = true
```

**How it works:**

```
Time:     0ms    10ms   30ms   50ms   100ms  150ms  200ms
          │       │      │      │       │      │      │
Changes:  ●       ●      ●      ·       ●      ·      ·
          │       │      │      │       │      │      │
          └───────┴──────┴──────┼───────┴──────┴──────┤
                                │                     │
                          debounce                 max_debounce
                          window                   triggers
                          (50ms)                   re-execution
                                                        │
                                                        ▼
                                              Single re-execution
                                              (all 4 changes coalesced)
```

**Coalescing rules:**

```rust
// Multiple changes to the same table are merged
Change { table: "projects", op: Insert, id: "a" }
Change { table: "projects", op: Insert, id: "b" }
Change { table: "projects", op: Update, id: "c" }
// → One re-execution for "projects" table

// Changes to different tables are tracked separately
Change { table: "projects", ... }
Change { table: "users", ... }
// → Subscriptions reading only "projects" ignore "users" change
```

**Per-subscription debouncing:**

Each subscription has its own debounce timer. A slow query (complex join) won't block a fast query from updating:

```rust
// Subscription A: simple lookup (5ms query) → short debounce
// Subscription B: complex aggregate (500ms query) → longer debounce

[reactivity.adaptive]
enabled = true
min_debounce_ms = 20
max_debounce_ms = 500
# Debounce = max(min, query_time * 0.5)
```

### Batching Updates

Multiple changes are batched before sending:

```rust
// Internal: batch window
const BATCH_WINDOW: Duration = Duration::from_millis(50);

// Changes within 50ms are batched into one update
// Prevents overwhelming clients during bulk operations
```

### Intelligent Batching for Bulk Operations

When you know you're doing a bulk operation, tell FORGE explicitly:

```rust
#[forge::mutation]
pub async fn import_projects(ctx: &MutationContext, projects: Vec<ProjectInput>) -> Result<usize> {
    // Wrap in batch - all changes coalesced, single notification at end
    ctx.batch(async {
        for project in projects {
            ctx.db.insert(Project::from(project)).await?;
        }
        Ok(projects.len())
    }).await
}
```

Without `ctx.batch()`: 1000 inserts → up to 1000 re-executions
With `ctx.batch()`: 1000 inserts → 1 re-execution after commit

### Subscription Coalescing

Multiple clients subscribing to the same query with the same arguments share one server-side subscription:

```
Client A: subscribe(get_projects, { team_id: "abc" })
Client B: subscribe(get_projects, { team_id: "abc" })  // Same query + args
Client C: subscribe(get_projects, { team_id: "xyz" })  // Different args

Server maintains:
- 1 subscription for team_id="abc" (shared by A and B)
- 1 subscription for team_id="xyz" (only C)

When team "abc" data changes:
- Query re-executed ONCE
- Delta sent to BOTH Client A and Client B
```

This is automatic—no configuration needed.

### Selective Re-execution

Not all changes trigger re-execution:

```rust
#[forge::query]
pub async fn get_user_projects(ctx: &QueryContext, user_id: Uuid) -> Result<Vec<Project>> {
    ctx.db.query::<Project>()
        .filter(|p| p.owner_id == user_id)  // Only projects for this user
        .fetch_all()
        .await
}

// If a project is created for a DIFFERENT user,
// this subscription is NOT re-executed (filtered out by read set)
```

---

## Debugging Subscriptions

### Dashboard

The built-in dashboard shows:
- Active subscriptions per node
- Subscription re-execution counts
- Delta sizes
- Client connection status

### Logging

```toml
# forge.toml

[observability.logging]
subscription_events = "debug"  # Log subscription lifecycle
```

```
[DEBUG] Subscription created: get_projects userId=abc conn=ws-123
[DEBUG] Change detected: projects table, row=xyz
[DEBUG] Checking 15 subscriptions for invalidation
[DEBUG] 3 subscriptions affected, re-executing
[DEBUG] Sending delta to conn=ws-123: added=1, removed=0, updated=0
```

---

## Best Practices

### 1. Keep Queries Focused

```rust
// ❌ Too broad - invalidates on any project change
#[forge::query]
pub async fn get_all_data(ctx: &QueryContext) -> Result<AllData> {
    let users = ctx.db.query::<User>().fetch_all().await?;
    let projects = ctx.db.query::<Project>().fetch_all().await?;
    let tasks = ctx.db.query::<Task>().fetch_all().await?;
    Ok(AllData { users, projects, tasks })
}

// ✅ Focused - only invalidates on specific user's projects
#[forge::query]
pub async fn get_user_projects(ctx: &QueryContext, user_id: Uuid) -> Result<Vec<Project>> {
    ctx.db.query::<Project>()
        .filter(|p| p.owner_id == user_id)
        .fetch_all()
        .await
}
```

### 2. Use Pagination for Large Lists

```rust
#[forge::query]
pub async fn get_projects(ctx: &QueryContext, page: Page) -> Result<Paginated<Project>> {
    ctx.db.query::<Project>()
        .order_by(|p| p.created_at.desc())
        .paginate(page)  // Only subscribe to visible page
        .await
}
```

### 3. Unsubscribe When Not Needed

```svelte
<script>
  import { subscribe } from '$lib/forge';
  import { onDestroy } from 'svelte';
  
  const projects = subscribe(get_projects, { userId: $user.id });
  
  // Subscription automatically cleaned up when component unmounts
  // But you can also manually unsubscribe:
  onDestroy(() => {
    projects.unsubscribe();
  });
</script>
```

---

## Related Documentation

- [Functions](FUNCTIONS.md) — Query definitions
- [WebSocket](../frontend/WEBSOCKET.md) — Connection handling
- [Change Tracking](../database/CHANGE_TRACKING.md) — PostgreSQL triggers
- [Data Flow](../architecture/DATA_FLOW.md) — Request flow

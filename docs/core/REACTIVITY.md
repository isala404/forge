# FORGE Reactivity System

This document describes the actual implementation of the FORGE reactivity system - the real-time data synchronization layer that automatically pushes updates to subscribed clients when data changes.

## Overview

The reactivity system enables automatic real-time updates without manual WebSocket management. When data changes in PostgreSQL, subscribed clients receive updates instantly.

```
PostgreSQL NOTIFY  ->  ChangeListener  ->  InvalidationEngine  ->  Reactor  ->  WebSocket Push
```

## Architecture

The system consists of these components in `crates/forge-runtime/src/realtime/`:

| Component | File | Purpose |
|-----------|------|---------|
| Reactor | `reactor.rs` | Central orchestrator connecting all components |
| ChangeListener | `listener.rs` | Listens for PostgreSQL NOTIFY events |
| InvalidationEngine | `invalidation.rs` | Debounces changes and finds affected subscriptions |
| SubscriptionManager | `manager.rs` | Tracks active subscriptions per session |
| WebSocketServer | `websocket.rs` | Manages WebSocket connections and message sending |

Core types are defined in `crates/forge-core/src/realtime/`:

| Type | File | Purpose |
|------|------|---------|
| ReadSet | `readset.rs` | Tracks tables/rows accessed during query execution |
| Change | `readset.rs` | Represents a database change event |
| SessionInfo | `session.rs` | WebSocket session metadata |
| SubscriptionInfo | `subscription.rs` | Subscription metadata and read set |

## PostgreSQL NOTIFY Triggers

### forge_notify_change()

The `forge_notify_change()` function in `0000_forge_internal.sql` sends NOTIFY events on the `forge_changes` channel:

```sql
CREATE OR REPLACE FUNCTION forge_notify_change() RETURNS TRIGGER AS $$
DECLARE
    row_id TEXT;
    payload TEXT;
BEGIN
    -- Get the row ID (assumes 'id' column exists)
    IF TG_OP = 'DELETE' THEN
        row_id := COALESCE(OLD.id::TEXT, '');
    ELSE
        row_id := COALESCE(NEW.id::TEXT, '');
    END IF;

    -- Build payload: table:operation:row_id
    payload := TG_TABLE_NAME || ':' || TG_OP || ':' || row_id;

    -- Send notification
    PERFORM pg_notify('forge_changes', payload);

    IF TG_OP = 'DELETE' THEN
        RETURN OLD;
    ELSE
        RETURN NEW;
    END IF;
END;
$$ LANGUAGE plpgsql;
```

### forge_enable_reactivity()

Helper function to enable reactivity on any table:

```sql
CREATE OR REPLACE FUNCTION forge_enable_reactivity(table_name TEXT) RETURNS VOID AS $$
DECLARE
    trigger_name TEXT;
BEGIN
    trigger_name := 'forge_notify_' || table_name;

    -- Drop existing trigger if any
    EXECUTE format('DROP TRIGGER IF EXISTS %I ON %I', trigger_name, table_name);

    -- Create AFTER INSERT/UPDATE/DELETE trigger
    EXECUTE format('
        CREATE TRIGGER %I
        AFTER INSERT OR UPDATE OR DELETE ON %I
        FOR EACH ROW EXECUTE FUNCTION forge_notify_change()
    ', trigger_name, table_name);
END;
$$ LANGUAGE plpgsql;
```

### Usage

In user migrations:

```sql
-- Enable reactivity on your table
SELECT forge_enable_reactivity('users');
SELECT forge_enable_reactivity('projects');

-- Disable when no longer needed
SELECT forge_disable_reactivity('users');
```

The internal tables `forge_jobs`, `forge_workflow_runs`, and `forge_workflow_steps` have reactivity enabled by default for job/workflow progress subscriptions.

### Payload Format

```
table_name:OPERATION:row_id[:changed_columns]
```

Examples:
- `users:INSERT:550e8400-e29b-41d4-a716-446655440000`
- `projects:UPDATE:550e8400-e29b-41d4-a716-446655440000:name,status`
- `tasks:DELETE:550e8400-e29b-41d4-a716-446655440000`

## Reactor Pipeline

The `Reactor` struct in `reactor.rs` orchestrates the full pipeline:

```rust
pub struct Reactor {
    node_id: NodeId,
    db_pool: sqlx::PgPool,
    registry: FunctionRegistry,
    subscription_manager: Arc<SubscriptionManager>,
    ws_server: Arc<WebSocketServer>,
    change_listener: Arc<ChangeListener>,
    invalidation_engine: Arc<InvalidationEngine>,
    active_subscriptions: Arc<RwLock<HashMap<SubscriptionId, ActiveSubscription>>>,
    job_subscriptions: Arc<RwLock<HashMap<Uuid, Vec<JobSubscription>>>>,
    workflow_subscriptions: Arc<RwLock<HashMap<Uuid, Vec<WorkflowSubscription>>>>,
    shutdown_tx: broadcast::Sender<()>,
}
```

### Pipeline Flow

1. **ChangeListener** receives PostgreSQL NOTIFY via `PgListener`
2. **Reactor** routes changes based on table:
   - `forge_jobs` -> Job subscription handlers
   - `forge_workflow_runs` / `forge_workflow_steps` -> Workflow subscription handlers
   - Other tables -> Query subscription invalidation
3. **InvalidationEngine** finds affected subscriptions via read set matching
4. **Reactor** re-executes invalidated queries
5. **WebSocketServer** pushes updated data to clients

### Starting the Reactor

```rust
impl Reactor {
    pub async fn start(&self) -> forge_core::Result<()> {
        // Spawn change listener task
        let listener_handle = tokio::spawn(async move {
            listener_clone.run().await
        });

        // Subscribe to changes
        let mut change_rx = listener.subscribe();

        // Main reactor loop
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    result = change_rx.recv() => {
                        match result {
                            Ok(change) => {
                                Self::handle_change(&change, ...).await;
                            }
                            Err(broadcast::error::RecvError::Lagged(n)) => {
                                tracing::warn!("Reactor lagged by {} messages", n);
                            }
                            Err(broadcast::error::RecvError::Closed) => break,
                        }
                    }
                    _ = shutdown_rx.recv() => break,
                }
            }
        });
    }
}
```

## ChangeListener

The `ChangeListener` in `listener.rs` uses PostgreSQL's `PgListener` for LISTEN/NOTIFY:

```rust
pub struct ChangeListener {
    pool: sqlx::PgPool,
    config: ListenerConfig,
    running: Arc<AtomicBool>,
    change_tx: broadcast::Sender<Change>,
    shutdown_tx: watch::Sender<bool>,
}
```

### Configuration

```rust
pub struct ListenerConfig {
    /// PostgreSQL channel name (default: "forge_changes")
    pub channel: String,
    /// Broadcast buffer size (default: 1024)
    pub buffer_size: usize,
}
```

### Notification Parsing

```rust
fn parse_notification(&self, payload: &str) -> Option<Change> {
    // Expected format: table:operation:row_id[:columns]
    let parts: Vec<&str> = payload.split(':').collect();

    if parts.len() < 2 {
        return None;
    }

    let table = parts[0].to_string();
    let operation = parts[1].parse().ok()?;

    let mut change = Change::new(table, operation);

    if parts.len() >= 3 {
        if let Ok(row_id) = Uuid::parse_str(parts[2]) {
            change = change.with_row_id(row_id);
        }
    }

    if parts.len() >= 4 {
        let columns: Vec<String> = parts[3].split(',').map(String::from).collect();
        change = change.with_columns(columns);
    }

    Some(change)
}
```

## InvalidationEngine

The `InvalidationEngine` in `invalidation.rs` handles debouncing and subscription matching:

```rust
pub struct InvalidationEngine {
    subscription_manager: Arc<SubscriptionManager>,
    config: InvalidationConfig,
    pending: Arc<RwLock<HashMap<SubscriptionId, PendingInvalidation>>>,
    invalidation_tx: mpsc::Sender<Vec<SubscriptionId>>,
}
```

### Configuration

```rust
pub struct InvalidationConfig {
    /// Debounce window (default: 50ms)
    pub debounce_ms: u64,
    /// Max debounce wait (default: 200ms)
    pub max_debounce_ms: u64,
    /// Coalesce changes by table (default: true)
    pub coalesce_by_table: bool,
    /// Max buffer before forced flush (default: 1000)
    pub max_buffer_size: usize,
}
```

### Processing Changes

```rust
pub async fn process_change(&self, change: Change) {
    // Find affected subscriptions via SubscriptionManager
    let affected = self.subscription_manager
        .find_affected_subscriptions(&change)
        .await;

    if affected.is_empty() {
        return;
    }

    let now = Instant::now();
    let mut pending = self.pending.write().await;

    for sub_id in affected {
        let entry = pending.entry(sub_id).or_insert_with(|| PendingInvalidation {
            subscription_id: sub_id,
            changed_tables: HashSet::new(),
            first_change: now,
            last_change: now,
        });
        entry.changed_tables.insert(change.table.clone());
        entry.last_change = now;
    }

    // Force flush if buffer is full
    if pending.len() >= self.config.max_buffer_size {
        drop(pending);
        self.flush_all().await;
    }
}
```

### Debounce Logic

```rust
pub async fn check_pending(&self) -> Vec<SubscriptionId> {
    let now = Instant::now();
    let debounce = Duration::from_millis(self.config.debounce_ms);
    let max_debounce = Duration::from_millis(self.config.max_debounce_ms);

    let mut pending = self.pending.write().await;
    let mut ready = Vec::new();

    pending.retain(|_, inv| {
        let since_last = now.duration_since(inv.last_change);
        let since_first = now.duration_since(inv.first_change);

        // Ready if debounce window passed OR max wait exceeded
        if since_last >= debounce || since_first >= max_debounce {
            ready.push(inv.subscription_id);
            false // Remove from pending
        } else {
            true // Keep in pending
        }
    });

    ready
}
```

In practice, the Reactor uses `flush_all()` for immediate invalidation instead of debounced `check_pending()` to provide real-time updates.

## Read Set Tracking

### ReadSet Structure

```rust
pub struct ReadSet {
    /// Tables accessed
    pub tables: HashSet<String>,
    /// Specific rows read per table (for row-level tracking)
    pub rows: HashMap<String, HashSet<Uuid>>,
    /// Columns used in filters
    pub filter_columns: HashMap<String, HashSet<String>>,
    /// Tracking mode
    pub mode: TrackingMode,
}

pub enum TrackingMode {
    Table,    // Track only tables (coarse-grained)
    Row,      // Track individual rows (fine-grained)
    Adaptive, // Auto-choose based on query characteristics
}
```

### Query Name Pattern Extraction

The Reactor extracts table names from query function names using common patterns:

```rust
fn extract_table_name(query_name: &str) -> String {
    if let Some(rest) = query_name.strip_prefix("get_") {
        rest.to_string()
    } else if let Some(rest) = query_name.strip_prefix("list_") {
        rest.to_string()
    } else if let Some(rest) = query_name.strip_prefix("find_") {
        rest.to_string()
    } else if let Some(rest) = query_name.strip_prefix("fetch_") {
        rest.to_string()
    } else {
        query_name.to_string()
    }
}
```

Examples:
- `get_users` -> tracks `users` table
- `list_projects` -> tracks `projects` table
- `find_tasks` -> tracks `tasks` table

### Change Invalidation

```rust
impl Change {
    pub fn invalidates(&self, read_set: &ReadSet) -> bool {
        // Table must be in read set
        if !read_set.includes_table(&self.table) {
            return false;
        }

        // For row-level tracking, check specific row
        if read_set.mode == TrackingMode::Row {
            if let Some(row_id) = self.row_id {
                match self.operation {
                    ChangeOperation::Update | ChangeOperation::Delete => {
                        return read_set.includes_row(&self.table, row_id);
                    }
                    ChangeOperation::Insert => {
                        // Inserts always potentially invalidate
                    }
                }
            }
        }

        true // Conservative: invalidate if unsure
    }
}
```

## WebSocket Message Protocol

### Client Messages

Defined in `crates/forge-runtime/src/gateway/websocket.rs`:

```rust
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMessage {
    /// Subscribe to a query
    Subscribe {
        id: String,
        #[serde(rename = "function")]
        function_name: String,
        args: Option<serde_json::Value>,
    },
    /// Unsubscribe from a subscription
    Unsubscribe { id: String },
    /// Subscribe to job progress
    SubscribeJob { id: String, job_id: String },
    /// Unsubscribe from job
    UnsubscribeJob { id: String },
    /// Subscribe to workflow progress
    SubscribeWorkflow { id: String, workflow_id: String },
    /// Unsubscribe from workflow
    UnsubscribeWorkflow { id: String },
    /// Keepalive ping
    Ping,
    /// Authentication
    Auth { token: String },
}
```

### Server Messages

```rust
#[derive(Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ServerMessage {
    /// Connection established
    Connected,
    /// Ping response
    Pong,
    /// Query subscription data
    Data { id: String, data: serde_json::Value },
    /// Job progress update
    JobUpdate { id: String, job: JobData },
    /// Workflow progress update
    WorkflowUpdate { id: String, workflow: WorkflowData },
    /// Error
    Error { id: Option<String>, code: String, message: String },
    /// Subscription confirmed
    Subscribed { id: String },
    /// Unsubscribe confirmed
    Unsubscribed { id: String },
}
```

### Job/Workflow Data Types

```rust
pub struct JobData {
    pub job_id: String,
    pub status: String,
    pub progress_percent: Option<i32>,
    pub progress_message: Option<String>,
    pub output: Option<serde_json::Value>,
    pub error: Option<String>,
}

pub struct WorkflowData {
    pub workflow_id: String,
    pub status: String,
    pub current_step: Option<String>,
    pub steps: Vec<WorkflowStepData>,
    pub output: Option<serde_json::Value>,
    pub error: Option<String>,
}

pub struct WorkflowStepData {
    pub name: String,
    pub status: String,
    pub error: Option<String>,
}
```

### Protocol Examples

**Query Subscription:**
```json
// Client sends:
{"type": "subscribe", "id": "sub-1", "function": "get_users", "args": null}

// Server responds:
{"type": "data", "id": "sub-1", "data": [{"id": "...", "name": "Alice"}]}

// When data changes, server pushes:
{"type": "data", "id": "sub-1", "data": [{"id": "...", "name": "Alice"}, {"id": "...", "name": "Bob"}]}
```

**Job Subscription:**
```json
// Client sends:
{"type": "subscribe_job", "id": "job-sub-1", "job_id": "550e8400-..."}

// Server responds with current state:
{"type": "job_update", "id": "job-sub-1", "job": {"job_id": "550e8400-...", "status": "running", "progress_percent": 50, ...}}

// As job progresses:
{"type": "job_update", "id": "job-sub-1", "job": {"job_id": "550e8400-...", "status": "running", "progress_percent": 75, ...}}
{"type": "job_update", "id": "job-sub-1", "job": {"job_id": "550e8400-...", "status": "completed", "progress_percent": 100, ...}}
```

**Workflow Subscription:**
```json
// Client sends:
{"type": "subscribe_workflow", "id": "wf-sub-1", "workflow_id": "550e8400-..."}

// Server responds:
{"type": "workflow_update", "id": "wf-sub-1", "workflow": {
  "workflow_id": "550e8400-...",
  "status": "running",
  "steps": [
    {"name": "step_1", "status": "completed", "error": null},
    {"name": "step_2", "status": "running", "error": null}
  ]
}}
```

## Job and Workflow Subscriptions

### Job Subscriptions

The Reactor maintains a map of job subscriptions:

```rust
job_subscriptions: Arc<RwLock<HashMap<Uuid, Vec<JobSubscription>>>>

struct JobSubscription {
    subscription_id: SubscriptionId,
    session_id: SessionId,
    client_sub_id: String,
    job_id: Uuid,
}
```

When `forge_jobs` table changes:

```rust
async fn handle_job_change(job_id: Uuid, ...) {
    let subs = job_subscriptions.read().await;
    let subscribers = subs.get(&job_id).cloned();

    // Fetch latest job state
    let job_data = fetch_job_data_static(job_id, db_pool).await?;

    // Push to all subscribers
    for sub in subscribers {
        let message = WebSocketMessage::JobUpdate {
            client_sub_id: sub.client_sub_id,
            job: job_data.clone(),
        };
        ws_server.send_to_session(sub.session_id, message).await;
    }
}
```

### Workflow Subscriptions

Similar structure for workflows:

```rust
workflow_subscriptions: Arc<RwLock<HashMap<Uuid, Vec<WorkflowSubscription>>>>

struct WorkflowSubscription {
    subscription_id: SubscriptionId,
    session_id: SessionId,
    client_sub_id: String,
    workflow_id: Uuid,
}
```

Workflow step changes trigger updates via parent workflow lookup:

```rust
async fn handle_workflow_step_change(step_id: Uuid, ...) {
    // Look up parent workflow_id
    let workflow_id: Option<Uuid> = sqlx::query_scalar(
        "SELECT workflow_run_id FROM forge_workflow_steps WHERE id = $1"
    ).bind(step_id).fetch_optional(db_pool).await?;

    if let Some(wf_id) = workflow_id {
        handle_workflow_change(wf_id, ...).await;
    }
}
```

### UUID Validation

All job/workflow IDs are validated before processing:

```rust
fn parse_uuid(s: &str, field_name: &str) -> Result<Uuid, String> {
    if s.len() > 36 {
        return Err(format!("Invalid {}: too long", field_name));
    }
    Uuid::parse_str(s).map_err(|_| format!("Invalid {}: must be a valid UUID", field_name))
}

const MAX_CLIENT_SUB_ID_LEN: usize = 255;
```

## Session Management

### Session Lifecycle

1. **Connection**: WebSocket upgrade creates `SessionId`
2. **Registration**: Session inserted into `forge_sessions` table and registered with Reactor
3. **Subscriptions**: Client subscribes to queries/jobs/workflows
4. **Updates**: Changes push through WebSocket
5. **Disconnection**: Session removed from database and all subscriptions cleaned up

### Database Tracking

```rust
// On connect
sqlx::query(
    "INSERT INTO forge_sessions (id, node_id, status, connected_at, last_activity)
     VALUES ($1, $2, 'connected', NOW(), NOW())
     ON CONFLICT (id) DO UPDATE SET status = 'connected', last_activity = NOW()"
).bind(session_uuid).bind(node_uuid).execute(&db_pool).await;

// On disconnect
sqlx::query("DELETE FROM forge_sessions WHERE id = $1")
    .bind(session_uuid).execute(&db_pool).await;
```

### Cleanup on Disconnect

```rust
pub async fn remove_session(&self, session_id: SessionId) {
    // Clean up query subscriptions
    if let Some(subscription_ids) = self.ws_server.remove_connection(session_id).await {
        for sub_id in subscription_ids {
            self.subscription_manager.remove_subscription(sub_id).await;
            self.active_subscriptions.write().await.remove(&sub_id);
        }
    }

    // Clean up job subscriptions
    {
        let mut job_subs = self.job_subscriptions.write().await;
        for subscribers in job_subs.values_mut() {
            subscribers.retain(|s| s.session_id != session_id);
        }
        job_subs.retain(|_, v| !v.is_empty());
    }

    // Clean up workflow subscriptions
    {
        let mut workflow_subs = self.workflow_subscriptions.write().await;
        for subscribers in workflow_subs.values_mut() {
            subscribers.retain(|s| s.session_id != session_id);
        }
        workflow_subs.retain(|_, v| !v.is_empty());
    }
}
```

## WebSocket Server

### Configuration

```rust
pub struct WebSocketConfig {
    /// Max subscriptions per connection (default: 50)
    pub max_subscriptions_per_connection: usize,
    /// Subscription timeout (default: 30s)
    pub subscription_timeout: Duration,
    /// Rate limit per minute (default: 100)
    pub subscription_rate_limit: usize,
    /// Heartbeat interval (default: 30s)
    pub heartbeat_interval: Duration,
    /// Max message size (default: 1MB)
    pub max_message_size: usize,
    /// Reconnect settings
    pub reconnect: ReconnectConfig,
}
```

### Connection Management

```rust
pub struct WebSocketConnection {
    pub session_id: SessionId,
    pub subscriptions: Vec<SubscriptionId>,
    pub sender: mpsc::Sender<WebSocketMessage>,
    pub connected_at: DateTime<Utc>,
    pub last_active: DateTime<Utc>,
}
```

### Sending Messages

```rust
pub async fn send_to_session(
    &self,
    session_id: SessionId,
    message: WebSocketMessage,
) -> Result<()> {
    let connections = self.connections.read().await;
    let conn = connections.get(&session_id)
        .ok_or_else(|| ForgeError::Validation("Session not found".into()))?;

    conn.send(message).await
        .map_err(|_| ForgeError::Internal("Failed to send message".into()))
}
```

## SubscriptionManager

Tracks subscriptions with multiple indexes for efficient lookup:

```rust
pub struct SubscriptionManager {
    /// Subscriptions by ID
    subscriptions: Arc<RwLock<HashMap<SubscriptionId, SubscriptionInfo>>>,
    /// Subscriptions by session (for cleanup)
    by_session: Arc<RwLock<HashMap<SessionId, Vec<SubscriptionId>>>>,
    /// Subscriptions by query hash (for coalescing)
    by_query_hash: Arc<RwLock<HashMap<String, Vec<SubscriptionId>>>>,
    /// Max subscriptions per session
    max_per_session: usize,
}
```

### Finding Affected Subscriptions

```rust
pub async fn find_affected_subscriptions(&self, change: &Change) -> Vec<SubscriptionId> {
    let subscriptions = self.subscriptions.read().await;
    subscriptions
        .iter()
        .filter(|(_, sub)| sub.should_invalidate(change))
        .map(|(id, _)| *id)
        .collect()
}
```

## Statistics

### Reactor Stats

```rust
pub struct ReactorStats {
    pub connections: usize,
    pub subscriptions: usize,
    pub pending_invalidations: usize,
    pub listener_running: bool,
}

impl Reactor {
    pub async fn stats(&self) -> ReactorStats {
        let ws_stats = self.ws_server.stats().await;
        let inv_stats = self.invalidation_engine.stats().await;

        ReactorStats {
            connections: ws_stats.connections,
            subscriptions: ws_stats.subscriptions,
            pending_invalidations: inv_stats.pending_subscriptions,
            listener_running: self.change_listener.is_running(),
        }
    }
}
```

### WebSocket Stats

```rust
pub struct WebSocketStats {
    pub connections: usize,
    pub subscriptions: usize,
    pub node_id: NodeId,
}
```

### Invalidation Stats

```rust
pub struct InvalidationStats {
    pub pending_subscriptions: usize,
    pub pending_tables: usize,
}
```

## Differences from Proposal

The actual implementation differs from `proposal/core/REACTIVITY.md` in these ways:

| Feature | Proposal | Implementation |
|---------|----------|----------------|
| Delta updates | Full delta computation | Currently sends full data (delta types exist but not computed) |
| Memory budget | Configurable memory limits | Not implemented |
| Adaptive tracking | Auto-selects row/table mode | Uses table-level with pattern extraction |
| Subscription coalescing | Multiple clients share execution | Index exists but full coalescing not implemented |
| Cross-node propagation | gRPC mesh option | PostgreSQL NOTIFY only |
| Rate limiting | Subscription rate limits | Config exists but not enforced |

## Frontend Integration

The scaffolded frontend includes tracker utilities in `$lib/forge/stores.ts`:

```typescript
// Job tracking
const job = createJobTracker<ExportUsersInput, ExportResult>('export_users');
await job.start({ format: 'csv' });

// Workflow tracking
const workflow = createWorkflowTracker<VerificationInput, VerificationResult>('account_verification');
await workflow.start({ user_id: 'abc', email: 'user@example.com' });

// Resume after page refresh
job.resume(jobIdFromLocalStorage);
workflow.resume(workflowIdFromLocalStorage);
```

These trackers handle:
- Dispatching jobs/workflows via RPC
- WebSocket subscription for progress
- Automatic cleanup on component destroy
- LocalStorage persistence for resume

## Related Files

- `crates/forge-runtime/src/realtime/reactor.rs` - Main reactor
- `crates/forge-runtime/src/realtime/listener.rs` - Change listener
- `crates/forge-runtime/src/realtime/invalidation.rs` - Invalidation engine
- `crates/forge-runtime/src/realtime/manager.rs` - Session/subscription managers
- `crates/forge-runtime/src/realtime/websocket.rs` - WebSocket server
- `crates/forge-runtime/src/gateway/websocket.rs` - WebSocket handler
- `crates/forge-core/src/realtime/` - Core types
- `crates/forge-runtime/migrations/0000_forge_internal.sql` - NOTIFY triggers

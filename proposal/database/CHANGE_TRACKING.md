# Change Tracking

> *Real-time data change detection*

---

## Overview

FORGE tracks all data changes to power:

- **Real-time subscriptions** — Push updates to clients
- **Cache invalidation** — Keep query cache fresh
- **Audit logging** — Who changed what, when
- **Event sourcing** — Replay changes if needed

---

## PostgreSQL LISTEN/NOTIFY

PostgreSQL's built-in pub/sub system enables instant change notifications:

```
┌─────────────────────────────────────────────────────────────────────────────┐
│                     LISTEN/NOTIFY FLOW                                       │
├─────────────────────────────────────────────────────────────────────────────┤
│                                                                              │
│   Client Transaction                                                         │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │  BEGIN;                                                              │   │
│   │  INSERT INTO projects (name) VALUES ('New Project');                 │   │
│   │  COMMIT;                                                             │   │
│   └─────────────────────────────────────────────────────────────────────┘   │
│                                       │                                      │
│                                       │ Trigger fires                        │
│                                       ▼                                      │
│   ┌─────────────────────────────────────────────────────────────────────┐   │
│   │  AFTER INSERT TRIGGER                                                │   │
│   │  PERFORM pg_notify('forge_changes', '{"table":"projects",...}');    │   │
│   └─────────────────────────────────────────────────────────────────────┘   │
│                                       │                                      │
│                                       │ PostgreSQL broadcasts               │
│                    ┌──────────────────┼──────────────────┐                  │
│                    │                  │                  │                  │
│                    ▼                  ▼                  ▼                  │
│             ┌───────────┐      ┌───────────┐      ┌───────────┐            │
│             │  Node 1   │      │  Node 2   │      │  Node 3   │            │
│             │ LISTENING │      │ LISTENING │      │ LISTENING │            │
│             └───────────┘      └───────────┘      └───────────┘            │
│                                                                              │
└─────────────────────────────────────────────────────────────────────────────┘
```

---

## Change Notification Trigger

Applied to every tracked table:

```sql
-- Trigger function
CREATE OR REPLACE FUNCTION forge_notify_change() 
RETURNS TRIGGER AS $$
DECLARE
    payload JSONB;
    row_id UUID;
BEGIN
    -- Get the row ID
    IF TG_OP = 'DELETE' THEN
        row_id := OLD.id;
    ELSE
        row_id := NEW.id;
    END IF;
    
    -- Build notification payload
    payload := jsonb_build_object(
        'table', TG_TABLE_NAME,
        'schema', TG_TABLE_SCHEMA,
        'operation', TG_OP,
        'id', row_id,
        'timestamp', NOW()
    );
    
    -- Add changed columns for updates
    IF TG_OP = 'UPDATE' THEN
        payload := payload || jsonb_build_object(
            'changed_columns', (
                SELECT jsonb_agg(key)
                FROM jsonb_each(to_jsonb(NEW))
                WHERE to_jsonb(NEW) -> key IS DISTINCT FROM to_jsonb(OLD) -> key
            )
        );
    END IF;
    
    -- Send notification (async, doesn't block transaction)
    PERFORM pg_notify('forge_changes', payload::text);
    
    RETURN COALESCE(NEW, OLD);
END;
$$ LANGUAGE plpgsql;

-- Apply to a table
CREATE TRIGGER projects_changes
    AFTER INSERT OR UPDATE OR DELETE ON projects
    FOR EACH ROW
    EXECUTE FUNCTION forge_notify_change();
```

---

## Listening for Changes

Each FORGE node listens for notifications:

```rust
use sqlx::postgres::PgListener;

impl ChangeListener {
    pub async fn start(&self) -> Result<()> {
        let mut listener = PgListener::connect(&self.database_url).await?;
        listener.listen("forge_changes").await?;
        
        loop {
            let notification = listener.recv().await?;
            
            let change: ChangeEvent = serde_json::from_str(notification.payload())?;
            
            // Process the change
            self.handle_change(change).await?;
        }
    }
    
    async fn handle_change(&self, change: ChangeEvent) -> Result<()> {
        // 1. Invalidate query cache
        self.cache.invalidate_for_table(&change.table).await;
        
        // 2. Check affected subscriptions
        let affected_subs = self.subscriptions
            .find_by_table(&change.table)
            .await;
        
        // 3. Re-run affected queries and push deltas
        for sub in affected_subs {
            if self.should_rerun(&sub, &change) {
                let new_result = self.execute_query(&sub.query).await?;
                let delta = self.compute_delta(&sub.last_result, &new_result);
                
                if !delta.is_empty() {
                    self.push_to_client(&sub.session_id, delta).await?;
                    sub.last_result = new_result;
                }
            }
        }
        
        Ok(())
    }
}
```

---

## Event Log (Persistent)

For audit trails and event sourcing, changes are also logged to a table:

```sql
CREATE TABLE forge_events (
    id BIGSERIAL PRIMARY KEY,
    
    -- What changed
    table_name VARCHAR(255) NOT NULL,
    schema_name VARCHAR(255) DEFAULT 'public',
    operation VARCHAR(10) NOT NULL,  -- INSERT, UPDATE, DELETE
    row_id UUID,
    
    -- Before/After state
    old_data JSONB,
    new_data JSONB,
    changed_columns TEXT[],
    
    -- Context
    transaction_id BIGINT DEFAULT txid_current(),
    timestamp TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    
    -- Who made the change (from application context)
    user_id UUID,
    function_name VARCHAR(255),
    trace_id VARCHAR(32)
);

-- Partition by time for efficient cleanup
-- Daily partitions, retain for 30 days
```

### Extended Trigger (with Event Log)

```sql
CREATE OR REPLACE FUNCTION forge_notify_and_log_change() 
RETURNS TRIGGER AS $$
DECLARE
    payload JSONB;
    row_id UUID;
    v_user_id UUID;
    v_function_name TEXT;
    v_trace_id TEXT;
BEGIN
    -- Get application context (set by FORGE before mutations)
    v_user_id := current_setting('forge.user_id', true)::UUID;
    v_function_name := current_setting('forge.function_name', true);
    v_trace_id := current_setting('forge.trace_id', true);
    
    -- Get row ID
    IF TG_OP = 'DELETE' THEN
        row_id := OLD.id;
    ELSE
        row_id := NEW.id;
    END IF;
    
    -- Build payload
    payload := jsonb_build_object(
        'table', TG_TABLE_NAME,
        'operation', TG_OP,
        'id', row_id,
        'timestamp', NOW()
    );
    
    -- Log to events table
    INSERT INTO forge_events (
        table_name, operation, row_id,
        old_data, new_data,
        user_id, function_name, trace_id
    ) VALUES (
        TG_TABLE_NAME,
        TG_OP,
        row_id,
        CASE WHEN TG_OP IN ('UPDATE', 'DELETE') THEN to_jsonb(OLD) END,
        CASE WHEN TG_OP IN ('INSERT', 'UPDATE') THEN to_jsonb(NEW) END,
        v_user_id,
        v_function_name,
        v_trace_id
    );
    
    -- Send notification
    PERFORM pg_notify('forge_changes', payload::text);
    
    RETURN COALESCE(NEW, OLD);
END;
$$ LANGUAGE plpgsql;
```

---

## Setting Application Context

Before executing mutations, FORGE sets context for audit:

```rust
impl MutationContext {
    async fn execute_with_context<F, R>(&self, f: F) -> Result<R>
    where
        F: FnOnce() -> Result<R>,
    {
        // Set context for triggers
        sqlx::query("SELECT set_config('forge.user_id', $1, true)")
            .bind(&self.user_id.to_string())
            .execute(&self.db)
            .await?;
            
        sqlx::query("SELECT set_config('forge.function_name', $1, true)")
            .bind(&self.function_name)
            .execute(&self.db)
            .await?;
            
        sqlx::query("SELECT set_config('forge.trace_id', $1, true)")
            .bind(&self.trace_id)
            .execute(&self.db)
            .await?;
        
        // Execute the mutation
        f()
    }
}
```

---

## Subscription Invalidation

When a change comes in, FORGE determines which subscriptions need updating:

```rust
struct Subscription {
    id: Uuid,
    query_name: String,
    query_args: Value,
    
    // What was read during last execution
    read_tables: HashSet<String>,
    read_rows: HashMap<String, HashSet<Uuid>>,
    
    last_result_hash: String,
}

impl SubscriptionManager {
    fn should_invalidate(&self, sub: &Subscription, change: &ChangeEvent) -> bool {
        // Check if subscription reads from changed table
        if !sub.read_tables.contains(&change.table) {
            return false;
        }
        
        // For updates/deletes, check if specific row was read
        if let Some(row_id) = &change.id {
            if let Some(read_rows) = sub.read_rows.get(&change.table) {
                // Row-level check
                if !read_rows.contains(row_id) {
                    return false;
                }
            }
        }
        
        // For inserts, we might need to check if new row matches query filters
        // Conservative approach: always invalidate on insert to tracked table
        
        true
    }
}
```

---

## Change Event Format

```typescript
interface ChangeEvent {
    table: string;
    schema: string;
    operation: 'INSERT' | 'UPDATE' | 'DELETE';
    id: string | null;
    timestamp: string;
    
    // For updates
    changed_columns?: string[];
}

// Examples:
{
    "table": "projects",
    "schema": "public",
    "operation": "INSERT",
    "id": "123e4567-e89b-12d3-a456-426614174000",
    "timestamp": "2024-01-15T10:30:00Z"
}

{
    "table": "projects",
    "schema": "public",
    "operation": "UPDATE",
    "id": "123e4567-e89b-12d3-a456-426614174000",
    "timestamp": "2024-01-15T10:31:00Z",
    "changed_columns": ["name", "updated_at"]
}
```

---

## NOTIFY Limitations

Be aware of PostgreSQL NOTIFY limits:

| Limit | Value | Mitigation |
|-------|-------|------------|
| Payload size | 8000 bytes | Keep payloads small, fetch details separately |
| Queue size | ~1GB | Ensure listeners are active |
| Lost if no listeners | Yes | Use event log for durability |

```rust
// If payload is too large, use reference
let payload = if full_payload.len() > 7000 {
    // Store in temp table, send reference
    let ref_id = self.store_large_payload(&full_payload).await?;
    json!({ "ref": ref_id, "table": change.table })
} else {
    full_payload
};
```

---

## Querying Event Log

### Recent Changes to a Record

```sql
SELECT operation, changed_columns, user_id, timestamp
FROM forge_events
WHERE table_name = 'projects' AND row_id = '...'
ORDER BY timestamp DESC
LIMIT 10;
```

### Who Changed What Today

```sql
SELECT 
    user_id,
    table_name,
    operation,
    count(*) as changes
FROM forge_events
WHERE timestamp > CURRENT_DATE
GROUP BY user_id, table_name, operation
ORDER BY changes DESC;
```

### Reconstruct Row State at Point in Time

```sql
-- Get the state of a row at a specific time
WITH events_before AS (
    SELECT *
    FROM forge_events
    WHERE table_name = 'projects'
      AND row_id = '...'
      AND timestamp <= '2024-01-15T10:00:00Z'
    ORDER BY timestamp DESC
    LIMIT 1
)
SELECT new_data FROM events_before;
```

---

## Cleanup and Retention

```sql
-- Delete events older than 30 days
DELETE FROM forge_events
WHERE timestamp < NOW() - INTERVAL '30 days';

-- Or with partitioning, just drop old partitions
DROP TABLE forge_events_2024_01_01;
```

---

## Related Documentation

- [Reactivity](../core/REACTIVITY.md) — Subscription system
- [PostgreSQL Schema](POSTGRES_SCHEMA.md) — Table definitions
- [Tracing](../observability/TRACING.md) — Distributed tracing

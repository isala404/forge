# Patterns Reference

Backend, auth, integrations. Testing lives in [testing.md](./testing.md); copy-paste templates in [recipes.md](./recipes.md).

## 0. Compile Loop Discipline

Use `forge check`. It auto-prepares the offline cache before running the rest of the pipeline, so you don't have to think about prepare ordering. If anything in the loop feels off, run `forge doctor` first.

The only thing you still have to remember by hand: when you add a new handler file under `src/functions/`, append `pub mod <name>;` to `src/functions/mod.rs`. The macro generates the inventory entry, but the module must be reachable from the crate root.

For raw `cargo check`, `SQLX_OFFLINE=true` is mandatory (see Compile-Loop Hard Rules in SKILL.md, or `eval "$(forge env)"`). Pass `--no-prepare` to `forge check` in CI.

## 1. Backend Design

### Shared logic via `DbConn`
Use `DbConn<'_>` when a helper must run in both queries (non-transactional) and mutations (transactional). It wraps either a pool handle or an active transaction, so the caller's scope is preserved.

**Inverted calling convention**: `DbConn` is not a sqlx `Executor`. Call `.fetch_*` on the `DbConn`, passing the query as the argument. Only `query_as!` is wrapped — for `query!` / `query_scalar!` use `&mut *conn` directly.

```rust
pub async fn list_active_items(db: DbConn<'_>) -> Result<Vec<Item>> {
    db.fetch_all(
        sqlx::query_as!(Item, "SELECT * FROM items WHERE status = 'active'")
    ).await.map_err(Into::into)
}
// Call from either: list_active_items(ctx.db_conn()).await
```

If the helper only runs in one context, skip `DbConn` and take `ForgeDb` (from `ctx.db()`) or `&mut ForgeConn<'_>` (from `ctx.conn()`) — both are `sqlx::Executor`s.

### Background jobs

```rust
#[forge::job(priority = "high", retry(max_attempts = 5, backoff = "exponential"), timeout = "30m")]
pub async fn process_video(ctx: &JobContext, args: Args) -> Result<Res> {
    ctx.progress(0, "Initializing...")?;   // streams to subscribers
    ctx.check_cancelled().await?;          // cooperative exit
    ctx.heartbeat().await?;                // extends lease
    Ok(res)
}
```

- Always dispatch from a mutation. Transactions are on by default, so dispatch is safe unless you explicitly set `transactional = false` (which the macro rejects at compile time if you dispatch).
- `idempotent(key = "args.field")` prevents duplicate processing.
- Lease reclaim after 5 min without a heartbeat.
- Persist progress context via `ctx.save(&state)` / `ctx.saved::<State>()?` so restarts resume gracefully.

### Scheduled tasks

```rust
#[forge::cron("0 */6 * * *", catch_up)]
pub async fn sync_external_data(ctx: &CronContext) -> Result<()> {
    if ctx.is_late() { /* log or skip */ }
    Ok(())
}
```

Advisory-lock leader election + UNIQUE constraint on `(cron_name, scheduled_time)` = exactly-once across the cluster. `catch_up` replays up to 10 missed intervals.

### Durable workflows

```rust
#[forge::workflow(name = "onboarding", version = "2026-05", timeout = "30d")]
pub async fn onboarding_wf(ctx: &WorkflowContext, user_id: Uuid) -> Result<()> {
    ctx.step("welcome_email", || async { send_email(user_id).await })
        .timeout(Duration::from_secs(30))
        .retry(3, Duration::from_secs(5))
        .compensate(|id| async move { rollback_action(id).await })
        .run().await?;

    ctx.sleep(Duration::from_secs(86_400)).await?;                        // survives restarts
    ctx.wait_for_event("profile_completed", Some(Duration::from_secs(3 * 86_400))).await?;
    Ok(())
}
```

- Steps cached by **name** — renaming breaks resume.
- Version bump required when step keys, wait keys, or data types change. Signature mismatch blocks runs.
- Step names: string literals only, max 64 chars, `[a-zA-Z0-9_-]`. No format strings or runtime values.
- Signature derivation is frozen at GA — blake3 hash (128-bit, truncated) over: name, version, step keys (sorted), wait keys (sorted), timeout_secs, input type, output type. Never add fields without bumping version.
- `ctx.sleep()` / `ctx.wait_for_event()` survive restarts. Never use `tokio::sleep`.

### Migrating a workflow safely (deprecate → drain → remove)

1. Bump `version`, mark old as `deprecated`, deploy. Both versions ship in the same binary; new dispatches go to the active version, in-flight runs of the old version keep going on the old code.
2. Wait for in-flight runs of the old version to drain (query `forge_workflow_runs` filtering by name, version, and non-terminal status).
3. Delete the old handler code and redeploy. If anything is still in-flight, the runtime detects it at boot and logs a warning. Boot succeeds.
4. Operator unblocks via admin endpoints: `POST /_api/admin/workflows/{id}/cancel` (with compensation) or `POST /_api/admin/workflows/{id}/force-abort` (terminal `cancelled_by_operator`, no compensation).

```sql
-- If you must use direct SQL (prefer admin endpoints):
UPDATE forge_workflow_runs
SET status = 'failed', resolution_reason = '...'
WHERE workflow_name = '...' AND workflow_version = '...'
  AND status NOT IN ('completed', 'failed');
```

## 2. Authentication and Authorization

### Social login (OAuth bridge)
Exchange codes on the server — never expose provider secrets to the browser.

1. Frontend obtains the code from the provider and POSTs to a public mutation.
2. Backend swaps code for tokens via `ctx.http().post(...)`.
3. Fetch user info, upsert into `user_identities`, link to a `user_id`.
4. Drop `ctx.conn()` **before** calling `ctx.issue_token_pair(user_id, roles)` — token issuance needs its own connection and will block on pool exhaustion otherwise.

| Provider | Token exchange | User info |
|---|---|---|
| Google | `oauth2.googleapis.com/token` | `googleapis.com/oauth2/v2/userinfo` |
| GitHub | `github.com/login/oauth/access_token` | `api.github.com/user` |

### Authorisation utilities
- `ctx.auth.require_role("admin")` — 403 if missing.
- `ctx.user_id()?` — principal UUID.
- Compile-time scope check fails the build when a private query doesn't filter by `user_id` / `owner_id` (opt out with `unscoped`).

### RoleResolver — dynamic RBAC

By default, `require_role` checks the flat `roles` list from the JWT. Register a `RoleResolver` to expand roles dynamically (hierarchy, group membership, remote permission service):

```rust
use forge_core::auth::{RoleResolver, AuthContext};

struct HierarchyResolver;

impl RoleResolver for HierarchyResolver {
    fn resolve(&self, auth: &AuthContext) -> Vec<String> {
        let mut roles = auth.roles().to_vec();
        if roles.contains(&"admin".to_string()) {
            roles.extend(["editor", "viewer"].map(String::from));
        }
        roles
    }
}

Forge::builder()
    .config(config)
    .with_role_resolver(Arc::new(HierarchyResolver))
    .auto_register()
    .build()?
    .run()
    .await
```

The resolver is called once per `require_role` check, not cached between calls. Keep it cheap — cache remote lookups internally if needed.

### Reserved JWT claim names

`ClaimsBuilder::claim()` returns `Err` for reserved names: `sub`, `exp`, `iat`, `nbf`, `jti`, `iss`, `aud`, `roles`. These map to structural fields or have dedicated setters:

```rust
// Wrong — returns ForgeError::InvalidArgument at runtime
ctx.issue_token_pair(
    Claims::builder()
        .user_id(id)
        .claim("sub", json!("other"))?,  // errors here
    ...
).await?;

// Right — use the typed setters
Claims::builder()
    .user_id(id)
    .claim("org_id", json!("org-123"))?  // custom claims are fine
    .audience("https://api.example.com")  // aud has its own setter
    .build()?
```

The restriction exists to prevent duplicate-keyed tokens where structural fields and a flattened custom key serialize under the same JSON key — some validators read one while `ctx.claim()` reads the other.

## 3. Integrations

### Webhooks
- Always set a `signature` constructor. Never `allow_unsigned` in production. Full table: [API Reference](./api.md#forgewebhook).
- Set `idempotency` — `"header:..."` for providers that send a delivery ID, `"body:$.id"` to extract from the payload.
- Non-Stripe schemes require senders to ship `x-webhook-timestamp: <unix-seconds>`. Forge rejects (401) anything missing, malformed, future-dated, or older than `replay_window_secs` (default 300). Tighten the window via `replay_window_secs = N` for low-latency callers; set `0` only when integrating with a sender that cannot stamp the header.
- Ack fast: return `WebhookResult::Accepted` and dispatch a job for any work over a few hundred ms. Webhook senders have short timeouts and retry on slow responses.
- Decode into a typed struct, not `serde_json::Value` — the macro deserialises the parameter type automatically.
- **Race condition**: webhooks and sync confirmation paths can arrive in any order. Use `COALESCE($1, column)` in updates so a slow webhook can't clobber data the faster path already set.

#### Provider quick-reference

| Provider | Constructor | Idempotency |
|---|---|---|
| Stripe | `stripe_webhooks("ENV")` | `"header:stripe-request-id"` |
| Shopify | `shopify_webhooks("ENV")` | `"body:$.id"` |
| GitHub | `hmac_sha256("X-Hub-Signature-256", "ENV")` | `"header:X-GitHub-Delivery"` |
| Ed25519-based | `ed25519("X-Signature", "PUBLIC_KEY_ENV")` | varies |

### MCP tools
- Annotate read-only vs destructive so agents pick correctly.
- Auth required by default; `require_role` restricts further.

### Custom Axum Routes
Reach for `ForgeBuilder::custom_routes` when a handler cannot fit the RPC/query/mutation shape (streaming responses, non-JSON content types, file downloads that aren't `Upload`). Everything else should stay in a `#[forge::query]` or `#[forge::mutation]` so it gets codegen bindings for free.

```rust
Forge::builder()
    .config(config)
    .custom_routes(|pool| {
        Router::new()
            .route("/export/csv", get(csv_export))
            .with_state(Arc::new(pool))
    })
    .auto_register()
    .build()?
    .run()
    .await
```

- The factory receives Forge's managed `PgPool`. Use `|_|` if you don't need it.
- Route paths mount under `/_api`. A route declared as `/export/csv` is served at `/_api/export/csv`.
- JWT auth, CORS, tracing, concurrency limits, and timeouts apply automatically — do not re-implement them.
- Read the authenticated user with `Extension<AuthContext>`. Unauthenticated requests still reach the handler, so guard with `match auth.user_id()` (never `.unwrap()`).
- Do not collide with built-in paths under `/_api`: `/health`, `/ready`, `/rpc`, `/rpc/*`, `/events`, `/subscribe*`, `/signal/*`, `/webhooks/*`, `/mcp`, `/oauth/*`.

## 4. Operations

- **Consistent reads**: `#[query(consistent)]` forces the primary when eventual consistency is unacceptable.
- **Single pool**: queries, mutations, jobs, cron, daemons, workflows, observability, and signals all share `[database] pool_size`. Size for the union of expected concurrency. Use `statement_timeout` to bound runaway queries; isolate heavy workloads with dedicated worker nodes, not extra pools.
- **Observability**: enable OTLP in `forge.toml`; `x-correlation-id` propagates across RPC boundaries. Full metric/span catalog: `docs/docs/reference/observability-catalog.mdx`.
- **Workflow safety**: startup validates active versions against persisted signatures; run the binary before shipping. Signature mismatches are logged as warnings at boot.
- **Cluster fencing**: leader-exclusive writes (cron scheduling, stale reclaim, workflow recovery) check `current_term` against the DB before executing. If a stale leader lost its election during a partition, the fencing check rejects the write. Custom leader-mode daemons should follow the same pattern.
- **Webhook idempotency**: best-effort via `INSERT ... ON CONFLICT`. Narrow race window between claim and handler completion. For strict exactly-once (financial ops), add application-level dedup inside your handler's transaction.
- **Signal analytics**: approximate by design. Unauthenticated endpoints, bounded channel that drops under pressure, UA-based bot detection, daily-rotating visitor IDs. Not suitable as a security audit log.
- **System tables**: `forge_jobs`, `forge_workflow_runs`, `forge_signals_events`, etc. are framework-owned. Use `ctx.dispatch_job()`, `ctx.start_workflow()`, `ctx.record_signal()`. `forge check` fails the build on manual writes.
- **DB primary failover**: the LISTEN/NOTIFY connection (`ChangeListener`) is not auto-reconnected after a primary failover. Restart the process. Pool connections reconnect automatically.
- **`forge_*` namespace**: reserved for framework tables. Never create application tables with this prefix.

### Admin endpoints and audit log

The `/_api/admin/*` surface is operator-grade. Every state-changing call requires `AuthContext::has_role("admin")` and appends a row to `forge_admin_audit` (actor, roles, target type/id, reason, request_id, trace_id). Read-only list/inspect calls are dashboard hot paths and don't audit.

- **Stop a runaway job**: `POST /_api/admin/jobs/{id}/cancel`. The worker checks `JobContext::is_cancelled()` between iterations. For unkillable jobs (no cancel check), `/force-abort` moves to `dead_letter`.
- **Stop a runaway workflow**: `POST /_api/admin/workflows/{id}/cancel`. Sets `cancel_requested_at` and fires NOTIFY `forge_workflow_wakeup`. A run in `ctx.sleep("...", 24h)` wakes within 50 ms, runs its compensation chain, and lands in `cancelled_by_operator`. No process restart needed.
- **Bleed off a hot queue**: `POST /_api/admin/queues/{name}/pause`. In-flight jobs finish, no new ones claim until `/resume`. The claim SQL uses `NOT EXISTS (SELECT 1 FROM forge_paused_queues ...)`.
- **Recover a stranded workflow**: when blocked runs are detected (server logs a warning at boot), use `/cancel` (with compensation) or `/force-abort` (no compensation, terminal `cancelled_by_operator`). Don't edit `forge_workflow_runs` directly — the admin path captures the reason.
- **Diagnostics**: `/_api/admin/nodes` returns `forge_nodes` with heartbeat + load; `/_api/admin/leaders` shows which node holds each advisory lock. Use these instead of `pg_locks` inspection.

Pass `{"reason": "..."}` on every state-changing call. The audit log is searched after incidents — empty reasons make post-mortems harder.

### Readiness as a deploy gate

`/_api/ready` is the load-balancer contract. Don't ship a binary that doesn't return 200 against the target DB.

- `migrations_ok=false` → `forge migrate up` before the new binary takes traffic. The check compares the embedded system migration count to `forge_system_migrations`.
- `notify_queue_ok=false` → some LISTEN consumer is stuck. Find with `SELECT pid, query FROM pg_stat_activity WHERE wait_event='AsyncWait'` and `pg_terminate_backend()`. Threshold is `pg_notification_queue_usage() < 0.75`.
- `cluster_registered=false` at boot is a race — wait one heartbeat (~5 s). If it persists, the node's row in `forge_nodes` was marked dead; check logs.
- PG < 18 is a startup hard-fail, not a readiness flag — the process exits non-zero with a clear error so orchestrators restart instead of accepting traffic.

# API Reference

This reference provides a comprehensive guide to Forge macros, context types, configuration options, and error variants.

## Macro Attributes

Forge handlers are defined using Rust macros that generate necessary structs and registration logic.

> Scaffold a new handler with `forge new <kind> <name>` (e.g. `forge new query list_invoices`). It writes the file with sane defaults, appends `pub mod <name>;` to `src/functions/mod.rs`, and inserts `mod functions;` in `src/main.rs` if missing. Kinds: `query`, `mutation`, `job`, `cron`, `workflow`, `daemon`, `webhook`, `mcp_tool`, `model`, `enum`.

### `#[forge::query]`
Defines a read-only operation. The macro generates a `{PascalCase}Query` struct and implements the `ForgeQuery` trait. All private queries must explicitly filter results by the current user or owner unless the `unscoped` attribute is used.

| Attribute | Description and Rationale |
|---|---|
| `name = "x"` | Overrides the default wire name (derived from the function name). |
| `public` | Skips JWT validation entirely — anonymous callers allowed. `ctx.auth.user_id()` returns `None`. Use for login pages, public APIs, landing data. |
| `unscoped` | JWT still required; skips the compile-time `user_id`/`owner_id` filter rule. Use for admin or shared data that any authenticated user may read. |
| `consistent` | Forces the query to read from the primary database to ensure data consistency after a recent write. |
| `require_role("x")` | Returns a 403 Forbidden error if the user lacks the specified role. |
| `cache = "30s"` | Enables a per-identity cache with the specified TTL to reduce database load. |
| `timeout = "30s"` | Sets the maximum execution time. Accepts duration strings: `"30s"`, `"5m"`, `"1h"`. |
| `rate_limit(requests = N, per = "1m", key = "user")` | Configures rate limiting. `key` values: `"user"`, `"ip"`, `"global"`, `"tenant"`, `"user_action"`, `"custom(claim_name)"`. |
| `description = "..."` | Adds a human-readable description to the function metadata. |
| `log = "info"` | Sets the log level for handler execution. |
| `tables("foo", "bar")` | Manually specifies table dependencies to trigger reactive cache invalidation. |

### `#[forge::mutation]`
Defines a data-modifying operation. The macro generates a `{PascalCase}Mutation` struct and implements the `ForgeMutation` trait.

| Attribute | Description and Rationale |
|---|---|
| `name = "x"` | Overrides the default wire name (derived from the function name). |
| `public` | Skips JWT validation entirely — anonymous callers allowed. Use for login endpoints and unauthenticated signups. |
| `unscoped` | JWT still required; skips the compile-time `user_id`/`owner_id` filter rule. Use for admin mutations that operate across all users. |
| `require_role("x")` | Restricts access to users with the specified role. |
| `transactional` | Wraps the entire operation in a PostgreSQL transaction. **Default: on.** Opt out with `transactional = false` for high-throughput writes that don't need atomicity. Cannot be disabled when using `dispatch_job()` or `start_workflow()`. |
| `timeout = "30s"` | Sets the handler timeout. Accepts duration strings: `"30s"`, `"5m"`, `"1h"`. |
| `max_size = "200mb"` | Defines the maximum allowable request body size for this mutation. |
| `rate_limit(requests = N, per = "1m", key = "user")` | Configures rate limiting. `key` values: `"user"`, `"ip"`, `"global"`, `"tenant"`, `"user_action"`, `"custom(claim_name)"`. |
| `description = "..."` | Adds a human-readable description to the function metadata. |
| `allow_http` | Permits `ctx.http()` calls inside a transactional mutation (normally a compile error). |

### `#[forge::job]`
Defines an asynchronous background task. These tasks are durable and automatically retried upon failure.

**Queue model**: PG-backed (`forge_jobs` table). Workers claim with `FOR UPDATE SKIP LOCKED`, ordered `priority DESC, scheduled_at ASC`. Concurrency bounded by a semaphore (`max_concurrent`, default 8). System jobs (workflow resumes, cron) hold 4 reserved permits so user job floods cannot starve them. Stale `claimed`/`running` jobs without a heartbeat for 5 minutes are released back to `pending` automatically.

**Status lifecycle**: `Pending` → `Claimed` (locked, not yet executing) → `Running` → `Completed` / `Failed` (retries remaining → `Retry` → `Pending`) / `DeadLetter` (max_attempts exhausted) / `CancelRequested` → `Cancelled`.

| Attribute | Description and Rationale |
|---|---|
| `name = "x"` | Overrides the default job name. |
| `timeout = "1h"` | Sets the maximum execution duration. **Default: `"1h"`**. |
| `priority = "normal"` | Priority level. Values: `background`(0), `low`(25), `normal`(50), `high`(75), `critical`(100). **Default: `"normal"`**. |
| `retry(max_attempts = 3, backoff = "exponential")` | Retry config. `backoff` accepts `"exponential"`, `"linear"`, or `"fixed"`. **Defaults: `max_attempts = 3`, `backoff = "exponential"`**. |
| `worker_capability` | Specifies a capability string required by the worker node to execute this job. |
| `idempotent` | Prevents duplicate job executions. Use `key = "input.id"` to specify the uniqueness key. |
| `ttl = "24h"` | Defines how long the job record persists in the database after completion. |
| `compensate = "fn"` | Specifies a cleanup function to run if the job ultimately fails after all retries. |

### `#[forge::cron("0 9 * * *")]`
Defines a task that runs on a recurring schedule. Execution is guaranteed to happen exactly once across the cluster.

| Attribute | Description and Rationale |
|---|---|
| `name = "x"` | Overrides the default registry name (derived from the function name). |
| `schedule = "0 9 * * *"` | Named form of the positional cron expression. |
| `every = "5m"` | Sugar for simple interval schedules. Converts to a cron expression internally. |
| `daily_at = "03:00"` | Sugar for daily schedules at a specific time. |
| `timezone = "UTC"` | Sets the schedule's timezone. Compile-time validated against the IANA tz database (`chrono_tz`). An unknown value fails with `Invalid timezone: "X". Must be an IANA tz database name (e.g., "UTC", "America/New_York").` |
| `group = "default"` | Groups crons for concurrency management. |
| `timeout = "1h"` | Sets the maximum allowed execution time. |
| `catch_up` | Executes missed intervals if the system was offline. **Default limit: 10 catch-up executions**. |

### `#[forge::workflow]`
Defines a durable, multi-step business process. Workflows are versioned to ensure that in-flight runs can complete even if the code changes.

| Attribute | Description and Rationale |
|---|---|
| `name = "x"` | Provides a logical ID shared across different versions of the workflow. |
| `version = "..."` | A unique version string. Changes to steps require a version bump. |
| `status = "active"` | Lifecycle status. Values: `"active"` (default, accepts new runs), `"deprecated"` (finishes existing runs only), `"staging"` (registered but never elected as the active version). |
| `active` | Shorthand flag equivalent to `status = "active"`. |
| `deprecated` | Shorthand flag equivalent to `status = "deprecated"`. |
| `timeout = "24h"` | Sets the maximum time a workflow run is allowed to execute. |
| `public` | Skips JWT validation for workflow trigger endpoints. |
| `require_role("x")` | Restricts who can trigger the workflow. |

### `#[forge::webhook]`
Defines an HTTP endpoint for receiving events from external services. The handler is registered at `POST /webhooks/{path}`.

| Attribute | Description and Rationale |
|---|---|
| `name = "x"` | Overrides the default registry name (derived from the function name). |
| `path = "/webhooks/stripe"` | The URL path this webhook listens on. Must start with `/`. |
| `signature = WebhookSignature::...` | Configures signature verification. Omitting this attribute causes the handler to reject all requests unless `allow_unsigned` is set. |
| `allow_unsigned` | Accept requests with no signature. Only use this during local development or for sources that cannot sign requests. |
| `idempotency = "header:X-Id"` | Extracts a deduplication key from the given header. Use `"body:$.id"` to extract from the request body via JSONPath. |
| `timeout = "30s"` | Sets the handler timeout. Also applies to `ctx.http()` calls within the handler. |
| `replay_window_secs = 300` | Max age in seconds of the `x-webhook-timestamp` header for non-Stripe schemes. Default `300`. Set `0` to disable. Stripe ignores this — it uses its own `t=` field. |

#### Signature Constructors

Use `WebhookSignature` (from `forge::prelude::*`) to configure signature verification. Each constructor sets the algorithm, the header to read the signature from, and the environment variable holding the secret.

| Constructor | Algorithm | Header | Notes |
|---|---|---|---|
| `WebhookSignature::hmac_sha256("Header", "ENV")` | HMAC-SHA256, hex | caller-supplied | GitHub and most providers using `sha256=...` |
| `WebhookSignature::stripe_webhooks("ENV")` | HMAC-SHA256 over `{ts}.{body}`, hex | `Stripe-Signature` | Stripe; built-in 5-min replay guard via the header's `t=` field |
| `WebhookSignature::shopify_webhooks("ENV")` | HMAC-SHA256, base64 | `X-Shopify-Hmac-Sha256` | Shopify storefront and admin webhooks |
| `WebhookSignature::ed25519("Header", "ENV")` | Ed25519 asymmetric | caller-supplied | Service publishes the public key; ENV holds the base64-encoded 32-byte key |

Non-Stripe schemes also require senders to attach `x-webhook-timestamp: <unix-seconds>`. Requests with a missing, malformed, future, or older-than-`replay_window_secs` header are rejected with 401. Set `replay_window_secs = 0` to opt out (not recommended).

```rust
// GitHub
#[forge::webhook(
    path = "/webhooks/github",
    signature = WebhookSignature::hmac_sha256("X-Hub-Signature-256", "GITHUB_WEBHOOK_SECRET"),
    idempotency = "header:X-GitHub-Delivery"
)]
pub async fn github_webhook(ctx: &WebhookContext, payload: Value) -> Result<WebhookResult> { ... }

// Stripe
#[forge::webhook(
    path = "/webhooks/stripe",
    signature = WebhookSignature::stripe_webhooks("STRIPE_WEBHOOK_SECRET"),
    idempotency = "header:stripe-request-id"
)]
pub async fn stripe_webhook(ctx: &WebhookContext, payload: Value) -> Result<WebhookResult> { ... }

// Shopify
#[forge::webhook(
    path = "/webhooks/shopify",
    signature = WebhookSignature::shopify_webhooks("SHOPIFY_WEBHOOK_SECRET"),
    idempotency = "body:$.id"
)]
pub async fn shopify_webhook(ctx: &WebhookContext, payload: Value) -> Result<WebhookResult> { ... }

// Ed25519 (e.g., a service that publishes a public key)
#[forge::webhook(
    path = "/webhooks/custom",
    signature = WebhookSignature::ed25519("X-Webhook-Signature", "WEBHOOK_PUBLIC_KEY")
)]
pub async fn custom_webhook(ctx: &WebhookContext, payload: Value) -> Result<WebhookResult> { ... }
```

## Environment Variables

Use context methods instead of `std::env::var()` — they are mockable in tests and fail fast at startup with a clear error.

| Method | Behavior |
|---|---|
| `ctx.env_require("KEY")` | Returns the value or a `ForgeError::Config` if missing. Use for required secrets. |
| `ctx.env_or("KEY", "default")` | Returns the value or a fallback string. Use for optional config with a sensible default. |

## KV Store

All handler contexts expose `ctx.kv()` for durable, namespaced key-value storage backed by PostgreSQL. Call `.kv()` to get a `&dyn KvHandle` reference; it returns `ForgeError::Internal` if the runtime did not thread the store in (this cannot happen in production — only in manually constructed test contexts that skip `with_kv`).

```rust
// Read a flag
let raw = ctx.kv()?.get("feature:dark-mode").await?;
let enabled = raw.map(|b| b == b"true").unwrap_or(false);

// Write with a 1-hour TTL
ctx.kv()?.set("feature:dark-mode", b"true", Some(Duration::from_secs(3600))).await?;

// Set only if the key is absent (distributed lock / idempotency)
let claimed = ctx.kv()?.set_if_absent("lock:send-email", b"1", Some(Duration::from_secs(60))).await?;

// Atomic counter (rate limiting, usage tracking)
let count = ctx.kv()?.increment("req:user:123", 1, Some(Duration::from_secs(60))).await?;

// Remove a key
ctx.kv()?.delete("lock:send-email").await?;
```

Available in: `QueryContext`, `MutationContext`, `JobContext`, `CronContext`, `DaemonContext`, `WebhookContext`, `WorkflowContext`, `McpToolContext`. Keys are namespaced by the runtime (`handlers:` prefix) so user code does not need to worry about cross-subsystem collisions.

In tests, call `.with_kv(Arc::new(MockKvStore::new()))` on the test context builder to supply a KV handle. Without it, `ctx.kv()` returns an error.

## HTTP Client

`ctx.http()` returns a circuit-breaker-backed `reqwest` client. The default timeout matches the handler's configured `timeout`. Always use this instead of constructing your own client so circuit breaking and tracing work correctly.

```rust
let resp: MyResponse = ctx.http()
    .post("https://api.example.com/action")
    .json(&payload)
    .send().await
    .map_err(|e| ForgeError::Internal(e.to_string()))?
    .json().await
    .map_err(|e| ForgeError::Deserialization(e.to_string()))?;
```

## `forge.toml` Key Configuration

These are the options most likely to cause silent runtime failures when missing. See the full reference at `docs/docs/ship/configuration.mdx`.

```toml
[auth]
# REQUIRED if issue_token_pair() is used. Missing these causes a panic at startup.
access_token_ttl = "15m"
refresh_token_ttl = "7d"
jwt_secret = "${JWT_SECRET}"   # must be ≥ 32 bytes; startup fails otherwise when auth is active
jwt_audience = "https://api.example.com"  # required by default (audience_required = true)
# audience_required = false    # set during migration if clients don't send aud yet
# required_claims = ["exp", "sub"]         # default; add "aud" for claim-level enforcement too

# Retired HMAC secrets — accepted for validation only. Each entry needs a valid_until
# timestamp; expired entries are dropped at startup. JWTs carry a `kid` derived from
# the signing secret so validation routes a token to its key directly.
# [[auth.legacy_secrets]]
# secret = "${OLD_JWT_SECRET}"
# valid_until = "2026-06-01T00:00:00Z"

# Browser clients store refresh tokens in an HttpOnly Secure cookie by default
# (XSS cannot read them). Set false only when the refresh endpoint cannot share
# a registrable domain with the frontend, or for legacy non-browser clients.
# refresh_cookie = true

# AuthConfig::dev_mode() / AuthMiddleware::permissive() return Result and refuse
# construction when FORGE_ENV=production. Don't ship dev-mode auth to prod.

[database]
url = "${DATABASE_URL}"
pool_size = 20                # default pool size

[gateway]
max_body_size = "20mb"        # total multipart body cap (default)
max_file_size = "10mb"        # per-file cap when mutation has no max_size (default)
# cors_enabled = true requires cors_origins to be non-empty. Mixing "*" with concrete origins fails at startup.

[worker]
job_timeout = "1h"            # default per-job timeout
poll_interval = "5s"          # fallback poll cadence; wakeups are NOTIFY-driven otherwise

# Per-queue worker pool reservations. Reserved queues:
#   default   — untagged user jobs                  (default: 8 workers)
#   workflows — $workflow_resume                    (default: 4 workers)
#   cron      — $cron:<name>                        (default: 2 workers)
# Heavy traffic on one queue cannot starve another. Add custom queues by
# tagging jobs with worker_capability and configuring a matching entry.
[worker.queues.default]
workers = 8
[worker.queues.workflows]
workers = 4
[worker.queues.cron]
workers = 2

[rate_limit]
mode = "hybrid"               # "hybrid" (default, per-node DashMap for user/ip) or "strict" (PG counter every check, cluster-correct)

[realtime]
# All fields are optional; production-safe defaults shown.
# PG helper functions (call in migrations to wire up reactivity):
#   SELECT forge_enable_reactivity('table_name');   -- installs forge_notify_{table} trigger (INSERT/UPDATE/DELETE)
#   SELECT forge_disable_reactivity('table_name');  -- drops the trigger
# System tables (forge_jobs, forge_workflow_runs, forge_workflow_steps) are enabled automatically.
# Table name limit: 50 chars (trigger prefix adds 13; PG caps identifiers at 63).
debounce_quiet_window = "50ms"       # coalesce window for change notifications
debounce_max_wait = "200ms"          # max wait before forcing a flush
max_concurrent_reexecutions = 64     # parallel query re-runs during invalidation
resync_interval = "600s"             # periodic sweep to recover dropped NOTIFYs (10 min); "0s" disables
postgres_change_buffer_size = 1024   # broadcast channel buffer for raw PG change events
subscription_max_per_session = 100   # max subscriptions a single SSE client may hold
sse_max_sessions = 10000             # max concurrent SSE sessions across all clients

[observability]
# Optional. Enables OTLP trace/metric export.
otlp_endpoint = "${FORGE_OTEL_ENDPOINT-http://localhost:4318}"    # any ${VAR-default} interpolation works
metrics_interval = "15s"      # metrics export period

[signals]
enabled = true                # master switch; set false to disable analytics
auto_capture = true           # auto-emit rpc_call events for RPC and server_execution events for jobs/crons/workflows/webhooks/daemons
diagnostics = true            # accept frontend error reports at /_api/signal (type: "report")
session_timeout_mins = 30     # inactivity window before a session closes
retention_days = 90           # drop monthly partitions older than this
anonymize_ip = true           # default; set false only with a lawful basis for raw IP storage (GDPR)
batch_size = 100              # events per batch INSERT
flush_interval_ms = 5000      # max milliseconds between flushes
excluded_functions = []       # function names to skip from auto-capture
bot_detection = true          # tag bot traffic via UA patterns
rate_limit_per_minute = 600   # per-IP cap on /signal in a rolling 60s window
# GeoIP: embedded DB-IP Country Lite resolves IPs to country codes automatically (zero config)
geoip_db_path = ""            # optional: path to MaxMind GeoLite2-City.mmdb for city-level resolution

# TLS on the gateway. Off by default — use a load balancer for public TLS.
# Enable [gateway.tls] when you need encrypted traffic between the LB and app
# (ALB backend HTTPS) or direct HTTPS on the app. Both cert_path and key_path
# set → TLS on. Both omitted → plain HTTP. Half-set → startup error.
# For a quick cert: openssl req -x509 -newkey rsa:2048 -nodes -days 365 \
#   -keyout key.pem -out cert.pem -subj "/CN=app.internal"
[gateway.tls]
cert_path = "${GATEWAY_TLS_CERT_PATH}"
key_path = "${GATEWAY_TLS_KEY_PATH}"
```

### Upload Size Limits

`gateway.max_body_size` caps the total HTTP body. `gateway.max_file_size` caps any single file when the target mutation does not declare its own `max_size`. When a mutation sets `max_size = "200mb"`, that value becomes both the total and per-file limit for that endpoint (explicit opt-in). Validation requires `max_file_size <= max_body_size`.

### Signal Endpoint

A single `POST /_api/signal` endpoint accepts a discriminated payload via the top-level `type` field. The server short-circuits `event` and `view` payloads when the request carries `DNT: 1` or `Sec-GPC: 1`. Crash reports (`type: "report"`) still land so production errors from DNT users don't disappear. When signals are disabled (the default) the endpoint still returns `204 No Content` and drops the body, so the always-on client trackers (web vitals, page views) never produce console errors against a missing route.

| `type` | Payload | Purpose |
|---|---|---|
| `event` | `{ events: [{event, properties?, correlation_id?, timestamp?}], context?: {page_url?, referrer?, session_id?} }` (max 50 events) | Batch of custom events, including `track`, `identify`, `web_vital`, `error`, `breadcrumb`, `page_view` |
| `view` | `{ url, referrer?, title?, utm_source?, utm_medium?, utm_campaign?, utm_term?, utm_content?, correlation_id? }` | Page view with referrer and UTM params |
| `report` | `{ errors: [{message, stack?, context?, correlation_id?, page_url?, breadcrumbs?}] }` (max 50 errors) | Frontend error report (bypasses DNT) |

### Auto-captured Event Types

| `event_type` | Emitted by |
|---|---|
| `page_view` | Client auto-track on SPA navigation |
| `rpc_call` | Server: every function executor invocation (query/mutation) |
| `server_execution` | Server: job worker, cron scheduler, workflow executor, webhook handler, daemon runner |
| `track` | Custom `track()` calls, plus server diagnostics: `auth.failed`, `rate_limit.exceeded`, `network.offline`, `network.online` |
| `identify` | Client `identify()` call |
| `web_vital` | Client auto-capture (LCP, CLS, INP, FCP, TTFB, navigation, long_task) + manual `vital()` |
| `error` | Client `captureError()` + auto-capture of `window.onerror` / `unhandledrejection` |
| `breadcrumb` | Client `breadcrumb()` call |

### Single Pool

Forge runs one primary connection pool. Queries, mutations, jobs, cron, daemons, workflows, observability, and signals all share it. Workload separation belongs at the worker level (concurrency limits, dedicated worker nodes), not at the connection layer. Size `database.pool_size` for the union of expected concurrency and use `database.statement_timeout` to bound runaway queries.

## MCP and OAuth Endpoints

### MCP transport (`mcp.enabled = true`)

| Method | Path | Notes |
|--------|------|-------|
| `POST` | `/_api/mcp` (default) | JSON-RPC request/notification. Returns 200 with result or 202 for notification-only. Path controlled by `mcp.path`. |
| `GET`  | `/_api/mcp` | Not used for stream transport in v1 — returns 405. |

### OAuth 2.1 (`mcp.oauth = true`, requires `mcp-oauth` feature)

Endpoints bypass Forge auth middleware. When OAuth is disabled both `/.well-known` routes return `404 {"error":"oauth_not_supported"}`.

| Method | Path | Notes |
|--------|------|-------|
| `GET`  | `/.well-known/oauth-authorization-server` | RFC 8414 AS metadata. Advertises authorize/token/register URLs. |
| `GET`  | `/.well-known/oauth-protected-resource` | RFC 9728 resource metadata. Points at this server as AS. |
| `POST` | `/_api/oauth/register` | Dynamic client registration (RFC 7591). Rate-limited 10/min per IP; 1 000 client cap. |
| `GET`  | `/_api/oauth/authorize` | Consent page. Reads `forge_session` cookie for single-click re-auth. |
| `POST` | `/_api/oauth/authorize` | Form submit. Validates CSRF + credentials + PKCE; issues auth code (60 s TTL, single-use); redirects to `redirect_uri`. |
| `POST` | `/_api/oauth/token` | Exchange code+PKCE verifier for access+refresh tokens, or refresh. Tokens carry `aud:"forge:mcp"`. |

## Admin Endpoints

All `/_api/admin/*` routes require the `admin` role on `AuthContext`. Every state-changing call appends a row to `forge_admin_audit` capturing actor, roles, target type/id, reason, request_id, and trace_id. Read-only list/inspect routes don't audit.

| Method | Path | Purpose |
|---|---|---|
| `GET`  | `/_api/admin/jobs?status=&job_type=&limit=` | List jobs (most recent first). |
| `GET`  | `/_api/admin/jobs/{id}` | Inspect a single job. |
| `POST` | `/_api/admin/jobs/{id}/cancel` | Mark job cancelled; worker observes on next poll. Body: `{ "reason": "..." }`. |
| `POST` | `/_api/admin/jobs/{id}/retry` | Reset a failed job to `pending` for re-claim. |
| `POST` | `/_api/admin/jobs/{id}/force-abort` | Move job to `dead_letter` regardless of state. Use sparingly. |
| `GET`  | `/_api/admin/workflows?status=&workflow_name=&limit=` | List workflow runs. |
| `GET`  | `/_api/admin/workflows/{id}` | Inspect a single run with step states. |
| `POST` | `/_api/admin/workflows/{id}/cancel` | Set `cancel_requested_at` + NOTIFY `forge_workflow_wakeup`; sleeping runs wake within 50 ms and run compensation. Lands in `cancelled_by_operator`. |
| `POST` | `/_api/admin/workflows/{id}/retry` | Re-pin a `blocked_*` run to the active version after signature reconciliation. |
| `POST` | `/_api/admin/workflows/{id}/force-abort` | Move run to `cancelled_by_operator` without compensation. |
| `GET`  | `/_api/admin/queues` | List queues with reserved worker pool size, paused flag, depth. |
| `POST` | `/_api/admin/queues/{name}/pause` | Insert into `forge_paused_queues`; claim SQL skips entries via `NOT EXISTS`. In-flight work finishes. |
| `POST` | `/_api/admin/queues/{name}/resume` | Remove the queue from `forge_paused_queues`. |
| `GET`  | `/_api/admin/nodes` | List `forge_nodes` rows with status, heartbeat, load metrics. |
| `GET`  | `/_api/admin/leaders` | Current advisory-lock holders per leader role. |
| `POST` | `/_api/admin/sessions/{session_id}/revoke` | Server-side auth revocation: drops cached `AuthContext` on the reactor and evicts the SSE connection. Body: `{ "reason": "..." }`. Wire to identity-system revocation events (demotion, tenant move, force-logout). |

State-changing routes accept an optional `reason` string; pass it — the audit log is searched after incidents.

## Production Topology

One binary runs all subsystems: gateway (Axum HTTP), function executor, job worker, cron scheduler (leader-elected), reactor (SSE/NOTIFY), daemon runner, and workflow executor. Deploy more copies for redundancy and scale — no separate worker process or sidecar.

**Node roles** (`[node] roles = [...]`): `gateway` (HTTP server), `function` (query/mutation execution), `worker` (job processing), `scheduler` (cron, leader-only). Default is all four. Omit `worker` on API nodes, omit `gateway` on dedicated worker nodes.

**Cluster config** (`[cluster]`): `discovery = "postgres"` (also supports `"dns"`, `"kubernetes"`, and `"static"`), `heartbeat_interval` (default `"5s"`), `dead_threshold` (default `"15s"`). No extra infrastructure beyond PostgreSQL.

**Minimum production**: 2 nodes (all roles) + PostgreSQL + load balancer routing on `/_api/ready`. One node wins the scheduler advisory lock; the other stands by. If the leader crashes, the lock releases when its PG connection closes and the standby acquires within the next heartbeat interval (default 5 s).

**Daemon leader election**: daemons marked leader-elected get an advisory lock derived from the daemon's name via FNV-1a hash (offset basis `0x464F52474000`). Stable across restarts; collisions between different daemon names are not possible in practice.

**Required infrastructure**: PostgreSQL 18 only. No Redis, no message bus, no separate scheduler process. Optional: read replicas (`[database.replicas]`), OTLP collector (`[observability]`), PgBouncer/RDS Proxy if approaching `max_connections`.

**MCP OAuth sticky sessions**: `/_api/oauth/*` stores CSRF state in-memory on the initiating node. Configure sticky sessions for that path prefix on the load balancer, or dedicate a single gateway node for MCP traffic.

Docs: `ship/production-architecture`, `scale/multiple-nodes`.

## Health and Readiness Probes

`GET /_api/health` — liveness probe. Returns 200 as long as the process is running. No DB call. Body: `{"status":"healthy","version":"0.x.x"}`. Use as Kubernetes `livenessProbe`.

`GET /_api/ready` — readiness probe. Returns 200 only when every flag is `true`; otherwise 503 with the body identifying failures. No authentication required. Body also includes `ready` (aggregate) and `version`.

```json
{
  "database":           true,
  "reactor":            true,
  "notify_queue_ok":    true,
  "migrations_ok":      true,
  "cluster_registered": true
}
```

| Flag | Source | False means |
|---|---|---|
| `database` | `SELECT 1` on primary pool | Pool exhausted, network split, or PG down. |
| `reactor` | NOTIFY listener attached and alive | Reactivity offline; new subscribes return 503. |
| `notify_queue_ok` | `pg_notification_queue_usage() < 0.75` | A LISTEN consumer is stuck — restart the affected node or `pg_terminate_backend()`. |
| `migrations_ok` | Embedded migration count == `forge_system_migrations` count | Code ahead of DB — run `forge migrate up` before traffic. |
| `cluster_registered` | This node's row in `forge_nodes` has `status = 'active'` | Heartbeat hasn't landed yet (boot race) or row was marked dead. |

Startup also hard-fails when PostgreSQL major < 18 (`MIN_POSTGRES_MAJOR` in `pg/pool.rs`) — Forge v2 relies on `pg_notification_queue_usage()`, partitioned `SET ACCESS METHOD`, `pg_stat_statements.toplevel`, and `NOWAIT` skip-locked semantics. No v1-style fallback.

## Custom Axum Routes

`ForgeBuilder::custom_routes(|pool| Router)` registers additional HTTP routes that inherit the gateway's middleware stack. The factory runs once during `run()` after the pool is connected.

```rust
builder.custom_routes(|pool| {
    Router::new()
        .route("/export/csv", get(csv_export))
        .with_state(Arc::new(pool))
})
```

- Factory receives `sqlx::PgPool`. Ignore it with `|_|` if not needed.
- Returned router is merged into the gateway's `/_api` namespace, so `/export/csv` is reachable at `/_api/export/csv`.
- Full middleware applies automatically: JWT auth, CORS, tracing, concurrency limits, request timeouts.
- Handlers read `Extension<AuthContext>` to access the authenticated user. Unauthenticated requests still arrive with an unauthenticated context — check `auth.user_id()` if login is required.
- Avoid paths that conflict with built-ins: `/health`, `/ready`, `/rpc`, `/rpc/*`, `/events`, `/events/ticket`, `/subscribe`, `/unsubscribe`, `/subscribe-job`, `/subscribe-workflow`, `/signal`, `/webhooks/*`, `/mcp`, `/oauth/*`. Conflicts panic at startup.

## SSE Authentication

Authenticated SSE streams use one-shot tickets so the JWT never appears in the URL:

1. Client `POST /_api/events/ticket` with `Authorization: Bearer <jwt>`.
2. Server returns `{ "ticket": "<uuid>", "expires_in_secs": 30 }`.
3. Client opens `GET /_api/events?ticket=<uuid>`.

Tickets are single-use, expire after 30s, are bound to the resolved client IP, and live only in process memory (no DB row). A `Authorization` header on `GET /_api/events` is also accepted for clients that can set headers (native, server-to-server). Anonymous SSE connects without any ticket. The generated TypeScript and Dioxus clients perform the ticket fetch automatically.

## API Versioning

RPC routes require the header `Accept: application/vnd.forge.v1+json`. Omitting the header is allowed (treated as v1). Any other value returns HTTP 406 with error code `unsupported_api_version`. Generated clients send this header automatically.

## RoleResolver

`RoleResolver` is a pluggable trait for dynamic RBAC. Implement it to expand or remap roles beyond the `roles` JWT claim (e.g. hierarchy expansion, DB lookups, tenant-scoped permissions).

```rust
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

// Register on the builder:
Forge::builder()
    .with_role_resolver(Arc::new(HierarchyResolver))
    .build()?
    .run()
    .await
```

The resolver is called once per `require_role` check. Cache expensive lookups internally. Without a custom resolver, the default returns `auth.roles()` as-is.

## Tenant Isolation

`TenantIsolationMode` controls how tenant scoping is applied inside a handler. Three variants:

| Mode | `as_str()` | Meaning |
|------|-----------|---------|
| `None` | `"none"` | No isolation; global access. Default. |
| `Strict` | `"strict"` | Reads and writes scoped to own tenant only. |
| `ReadShared` | `"read_shared"` | Reads include global rows; writes are tenant-scoped. |

`TenantContext` holds the resolved tenant and mode:
- `TenantContext::none()` — no tenant set.
- `TenantContext::strict(tenant_id)` — strict mode shorthand.
- `TenantContext::new(tenant_id, mode)` — arbitrary mode.
- `.requires_filtering()` — true when tenant is set and mode != `None`.
- `.sql_filter("column", param_index)` — returns `Some(("\"column\" = $N", uuid))` for safe parameterized injection; returns `None` if column name is invalid or no tenant is set.
- `.require_tenant()` — returns `Err(ForgeError::Unauthorized)` if no tenant.

`tenant_id` flows from JWT claim → `ctx.auth.tenant_id()` → handler SQL. When a private query's SQL contains `tenant_id` in a WHERE clause, the macro marks `requires_tenant_scope = true`. The runtime enforces the claim's presence and returns 403 if absent — before the handler runs.

Testing: use `.with_tenant(uuid)` on any test context builder.

## Duration Formats
Time durations can be expressed as `500ms`, `30s`, `5m`, `2h`, `7d`, or a bare number representing seconds. All handler timeout attributes accept these duration strings.

## Context Capability Matrix

Each handler type receives a specific context object providing access to framework services.

| Feature | Query | Mut | Job | Cron | WF | Dmn | Web | MCP |
|---|---|---|---|---|---|---|---|---|
| `db()` (Read access) | yes | — | yes | yes | yes | yes | yes | yes |
| `tx()` (DbConn, mutation only) | — | yes | — | — | — | — | — | — |
| `conn()` (Write access) | — | yes | yes | yes | yes | yes | yes | yes |
| `http()` (Client) | — | yes | yes | yes | yes | yes | yes | — |
| `auth` (Session info) | yes | yes | yes | yes | yes | — | — | yes |
| `dispatch_job` | — | yes | — | — | — | yes | yes | yes |
| `issue_token_pair` | — | yes | — | — | — | — | — | — |
| `step()` / `sleep()` | — | — | — | — | yes | — | — | — |
| `heartbeat()` | — | — | yes | — | — | yes | — | — |
| `save()` | — | — | yes | — | — | — | — | — |
| `EnvAccess` | yes | yes | yes | yes | yes | yes | yes | yes |

### Context Usage Notes
- **Database Access**: In mutations, use `let mut conn = ctx.conn().await?` to obtain a transactional connection. Pass `&mut conn` to SQL macros to ensure your queries are part of the transaction.
- **Environment Variables**: Use the `EnvAccess` methods (e.g., `ctx.env_require()`) for all configuration to ensure your code is mockable in tests.
- **HTTP Client**: Use `ctx.http()` for circuit-breaker-backed requests. The default timeout for these requests matches the handler's defined timeout.

## ForgeError Variants

Forge uses structured error variants to ensure consistent error handling across the stack and proper HTTP status code mapping.

The canonical status mapping lives on `ForgeError::http_status() -> u16`. Downstream consumers (outside forge-runtime) can call this without depending on the gateway layer.

| Variant | HTTP Code | Internal Code | Rationale |
|---|---|---|---|
| `NotFound` | 404 | `NOT_FOUND` | Resource does not exist. |
| `Unauthorized` | 401 | `UNAUTHORIZED` | Authentication is missing or invalid. |
| `Forbidden` | 403 | `FORBIDDEN` | User lacks permission for the operation. |
| `Validation` | 400 | `VALIDATION_ERROR` | Request data is malformed or invalid. |
| `InvalidArgument` | 400 | `INVALID_ARGUMENT` | Caller-supplied argument is semantically invalid. |
| `Deserialization` | 400 | `INVALID_ARGUMENT` | Request body could not be parsed; details are hidden from clients. |
| `Timeout` | 504 | `TIMEOUT` | Operation exceeded its allotted time. |
| `RateLimitExceeded` | 429 | `RATE_LIMITED` | Too many requests from the same identity. Includes top-level `retry_after_secs` on the wire. |
| `JobCancelled` | 409 | `JOB_CANCELLED` | Job was cancelled before it could complete. |
| `Conflict` | 409 | `CONFLICT` | Resource state conflicts with the requested operation. |
| `UnprocessableEntity` | 422 | `UNPROCESSABLE_ENTITY` | Request is syntactically valid but semantically invalid. |
| `ServiceUnavailable` | 503 | `SERVICE_UNAVAILABLE` | Service temporarily unable to handle the request. |
| `Internal` / all others | 500 | `INTERNAL_ERROR` | Server-side error; details never leak to clients. |

## CLI Command Reference

| Command | Purpose |
|---|---|
| `forge new <name>` | Scaffolds a new project from a template. |
| `forge generate` | Synchronizes backend changes with frontend bindings and types. **Manual step — not part of `cargo build`.** Run after changing handler signatures, argument/return types, models, or enums. `forge check` catches staleness. In CI, use `forge check` to validate without overwriting. |
| `forge check` | Runs linting, formatting, and validates SQL and bindings. |
| `forge migrate up` | Run all pending migrations under advisory lock. Safe against a live cluster. |
| `forge migrate status` | Show applied/pending migrations. Flags `[DRIFT]` (checksum mismatch) and `[SOURCE FILE MISSING]` anomalies. |
| `forge migrate prepare` | Run pending migrations, then regenerate `.sqlx/` offline cache via `cargo sqlx prepare --workspace`. Requires `cargo-sqlx`. |
| `forge test` | Executes full-stack E2E tests using Playwright. |

## Project File Standards
- **Source Code**: Editable logic resides in `src/functions/`, `src/schema/`, and `src/utils/`.
- **Generated Code**: **MANDATE:** Never edit generated files. See [Pitfalls](./pitfalls.md#1-generated-code).
- **Migrations**: Create new SQL files in `migrations/`. Forward-only; do not include `-- @down`. The optional `-- @up` marker is stripped if present. Do not use `IF NOT EXISTS` clauses; migrations should be deterministic.

## Cargo Features

Subsystems are feature-gated; default is `full`. Opt out with `default-features = false` and pick a preset.

**Presets**: `full` = all (default) · `worker` = jobs+workflows+cron+daemons+otel (no HTTP) · `api` = gateway+otel (no workers) · `minimal` = gateway only.

| Feature | Default | Bundles | Extra crates |
|---|---|---|---|
| `gateway` | yes | HTTP RPC + SSE + OAuth + MCP + webhooks + signals + TLS | axum, tower, tower-http, jsonwebtoken, argon2, ring, rustls |
| `jobs` | yes | PG-backed queue + SKIP LOCKED worker | — |
| `workflows` | yes | Versioned durable workflow executor | — |
| `cron` | yes | Leader-only cron scheduler | — |
| `daemons` | yes | Long-running daemon runner | — |
| `mcp-oauth` | yes | OAuth 2.1 + PKCE for MCP (req. `gateway`) | — |
| `geoip` | yes (in `full`) | Runtime MaxMind MMDB reader for signals; offline-safe (req. `gateway`) | maxminddb |
| `geoip-embedded` | no | Bundled DB-IP country DB baked in (req. `geoip`); build-time download | db_ip |
| `otel` | yes | OTel trace/metric/log exporters | opentelemetry ×5, protobuf stubs |
| `testcontainers` | no | Test context helpers that spin up a real PG container | testcontainers |
| `embedded-frontend` | no | Embeds compiled frontend into binary at build time | rust-embed |

```toml
forgex = { version = "0.10.2", default-features = false, features = ["worker"] }
```

`#[forge::job/cron/workflow/daemon/webhook/mcp_tool]` without the matching feature errors at the generated `forge::AutoHandler` reference. Without `otel`, `tracing-subscriber` still logs to stderr. `geoip` (in `full`) is offline-safe — it only adds the runtime MMDB reader; set `signals.geoip_db_path` to a GeoLite2-City MMDB for enrichment. Only `geoip-embedded` fetches a ~10 MB DB at compile time, so keep it off for air-gapped builds.

## Build Profiles

| Profile | LTO | codegen-units | Use for |
|---|---|---|---|
| `dev` | off | 256 | Iteration (line-only debug, deps stripped) |
| `release` | full | 1 | Production |
| `release-fast` | off | 16 | Smoke tests / ad-hoc benchmarks (skips ~30-90s LTO link) |

Linker tuning (env): `RUSTFLAGS="-C link-arg=-fuse-ld=mold"` (Linux) / `=lld` (macOS); `RUSTC_WRAPPER=sccache` for cross-build cache.

## Observability Surface (Frozen at GA)

Full catalog at `docs/docs/reference/observability-catalog.mdx`. Key points for code generation and integrations:

**Stable metric names** (meter `forge-runtime` unless noted):
- `http_requests_total` / `http_request_duration_seconds` — dims: `method`, `path`, `status`
- `fn.executions_total` / `fn.duration_seconds` — dims: `function`, `kind`, `status`
- `job_executions_total` / `job_duration_seconds` — dims: `job_type`, `status`
- `db.client.operation.duration` — dims: `db.system`, `db.operation.name` (meter: `forge.db`)

**Stable span names**: `http.request`, `fn.execute`, `db.query`, `db.transaction`, `job.execute`, `cron.tick`, `cron.execute`, `daemon.lifecycle`, `daemon.execute`.

**Workflow signature (frozen)**: blake3 hash (128-bit, truncated) of: name → version → step keys (sorted) → wait keys (sorted) → timeout_secs (u64 LE) → input type string → output type string. Never add fields to this derivation.

**Step name rules**: string literals only, max 64 chars, `[a-zA-Z0-9_-]`.

**`forge_*` reserved**: do not create application tables with this prefix.

**Config substitution**: `${VAR-default}` uses default only when var is *unset*. `VAR=""` (set to empty) expands to empty, not the default. `${VAR}` with no default and no env var preserves the literal `${VAR}` in the TOML (parse error likely).

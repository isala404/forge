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
| `public` | Disables authentication requirements for the query. |
| `consistent` | Forces the query to read from the primary database to ensure data consistency after a recent write. |
| `require_role("x")` | Returns a 403 Forbidden error if the user lacks the specified role. |
| `cache = "30s"` | Enables a per-identity cache with the specified TTL to reduce database load. |
| `timeout = "30s"` | Sets the maximum execution time. Accepts duration strings: `"30s"`, `"5m"`, `"1h"`. |
| `rate_limit(requests = N, per = "1m", key = "user")` | Configures rate limiting. `key` values: `"user"`, `"ip"`, `"global"`, `"custom:claim_name"`. |
| `log = "info"` | Sets the log level for handler execution. |
| `unscoped` | Skips mandatory scope enforcement checks at compile time. |
| `tables = [...]` | Manually specifies table dependencies to trigger reactive cache invalidation. |

### `#[forge::mutation]`
Defines a data-modifying operation. The macro generates a `{PascalCase}Mutation` struct and implements the `ForgeMutation` trait.

| Attribute | Description and Rationale |
|---|---|
| `name = "x"` | Overrides the default wire name (derived from the function name). |
| `public` | Allows unauthenticated access to the mutation. |
| `require_role("x")` | Restricts access to users with the specified role. |
| `transactional` | Wraps the entire operation in a PostgreSQL transaction. **Default: on.** Opt out with `transactional = false` for high-throughput writes that don't need atomicity. Cannot be disabled when using `dispatch_job()` or `start_workflow()`. |
| `timeout = "30s"` | Sets the handler timeout. Accepts duration strings: `"30s"`, `"5m"`, `"1h"`. |
| `max_size = "200mb"` | Defines the maximum allowable request body size for this mutation. |
| `rate_limit(requests = N, per = "1m", key = "user")` | Configures rate limiting. `key` values: `"user"`, `"ip"`, `"global"`, `"custom:claim_name"`. |
| `unscoped` | Disables compile-time scope validation. |

### `#[forge::job]`
Defines an asynchronous background task. These tasks are durable and automatically retried upon failure.

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
| `timezone = "UTC"` | Sets the schedule's timezone. |
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

#### Signature Constructors

Use `WebhookSignature` (from `forge::prelude::*`) to configure signature verification. Each constructor sets the algorithm, the header to read the signature from, and the environment variable holding the secret.

| Constructor | Algorithm | Notes |
|---|---|---|
| `WebhookSignature::hmac_sha256("Header", "ENV")` | HMAC-SHA256, hex-encoded | GitHub, most generic providers |
| `WebhookSignature::hmac_sha1("Header", "ENV")` | HMAC-SHA1, hex-encoded | Legacy GitHub |
| `WebhookSignature::hmac_sha512("Header", "ENV")` | HMAC-SHA512, hex-encoded | Uncommon |
| `WebhookSignature::standard_webhooks("ENV")` | HMAC-SHA256, base64, `{id}\n{ts}\n{body}` | Polar, Svix, Clerk — header always `webhook-signature` |
| `WebhookSignature::stripe_webhooks("ENV")` | HMAC-SHA256, hex, `{ts}.{body}`, 5-min replay guard | Stripe — header always `Stripe-Signature` |
| `WebhookSignature::shopify_webhooks("ENV")` | HMAC-SHA256, base64-encoded | Shopify — header always `X-Shopify-Hmac-Sha256` |
| `WebhookSignature::ed25519("Header", "ENV")` | Ed25519 asymmetric verification | For services that publish a public key instead of a shared secret |

For `ed25519`, the `ENV` variable holds a **base64-encoded Ed25519 public key** (32 bytes), not a shared secret.

```rust
// Polar / Standard Webhooks
#[forge::webhook(
    path = "/webhooks/polar",
    signature = WebhookSignature::standard_webhooks("POLAR_WEBHOOK_SECRET"),
    idempotency = "header:webhook-id"
)]
pub async fn polar_webhook(ctx: &WebhookContext, payload: Value) -> Result<WebhookResult> { ... }

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
# legacy_secrets = ["${OLD_JWT_SECRET}"]   # accepted for validation only; rotate by removing after one TTL

[database]
url = "${DATABASE_URL}"
max_connections = 20          # default pool size

[gateway]
max_body_size = "20mb"        # total multipart body cap (default)
max_file_size = "10mb"        # per-file cap when mutation has no max_size (default)
# cors_enabled = true requires cors_origins to be non-empty. Mixing "*" with concrete origins fails at startup.

[worker]
concurrency = 10              # parallel job slots per node

[rate_limit]
mode = "hybrid"               # "hybrid" (default, per-node DashMap for user/ip) or "strict" (PG counter every check, cluster-correct)

[realtime]
# All fields are optional; production-safe defaults shown.
debounce_quiet_window = "50ms"       # coalesce window for change notifications
debounce_max_wait = "200ms"          # max wait before forcing a flush
max_concurrent_reexecutions = 64     # parallel query re-runs during invalidation
resync_interval = "60s"              # periodic sweep to recover dropped NOTIFYs; "0s" disables
postgres_change_buffer_size = 1024   # broadcast channel buffer for raw PG change events
subscription_max_per_session = 100   # max subscriptions a single SSE client may hold
change_tracking_row_threshold = 200  # switches from row-level to table-level tracking above this
sse_max_sessions = 10000             # max concurrent SSE sessions across all clients

[observability]
# Optional. Enables OTLP trace/metric export.
otlp_endpoint = "${FORGE_OTEL_ENDPOINT-http://localhost:4318}"    # any ${VAR-default} interpolation works
metrics_interval = "15s"      # metrics export period

[signals]
enabled = true                # master switch; set false to disable analytics
auto_capture = true           # auto-emit rpc_call events for RPC and server_execution events for jobs/crons/workflows/webhooks/daemons
diagnostics = true            # accept frontend error reports at /_api/signal/report
session_timeout_mins = 30     # inactivity window before a session closes
retention_days = 90           # drop monthly partitions older than this
anonymize_ip = false          # drop raw client IPs from stored events (visitor_id stays hashed)
batch_size = 100              # events per batch INSERT
flush_interval_ms = 5000      # max milliseconds between flushes
excluded_functions = []       # function names to skip from auto-capture
bot_detection = true          # tag bot traffic via UA patterns
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

### Signal Endpoints

The server short-circuits `/_api/signal/view`, `/_api/signal/event`, `/_api/signal/user`, and `/_api/signal/vital` when the request carries `DNT: 1` or `Sec-GPC: 1`. Crash reports still land so production errors from DNT users don't disappear.

| Endpoint | Method | Purpose |
|---|---|---|
| `/_api/signal/event` | POST | Batch custom events (max 50 per request) |
| `/_api/signal/view` | POST | Page view with referrer and UTM params |
| `/_api/signal/user` | POST | Identify user and store traits |
| `/_api/signal/report` | POST | Frontend error reports with breadcrumbs |
| `/_api/signal/vital` | POST | Web Vitals / performance metrics (max 50 per request) |

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

### Pool Routing

Forge uses isolated connection pools to prevent jobs and analytics from starving web requests.

| Pool | Used by |
|---|---|
| `default` | Queries, mutations, crons, webhooks |
| `jobs` | Job worker polling and execution |
| `observability` | OTLP metric writes |
| `analytics` | Signals / `forge_signals_events` writes |

Each pool can be sized independently under `[database.pools.jobs]`, etc.

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
- Avoid paths that conflict with built-ins: `/health`, `/ready`, `/rpc`, `/rpc/*`, `/events`, `/subscribe`, `/unsubscribe`, `/subscribe-job`, `/subscribe-workflow`, `/signal/*`, `/webhooks/*`, `/mcp`, `/oauth/*`. Conflicts panic at startup.

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

## Duration Formats
Time durations can be expressed as `500ms`, `30s`, `5m`, `2h`, `7d`, or a bare number representing seconds. Note that `query`, `mutation`, and `mcp_tool` timeout attributes specifically require a bare `u64` integer representing seconds.

## Context Capability Matrix

Each handler type receives a specific context object providing access to framework services.

| Feature | Query | Mut | Job | Cron | WF | Dmn | Web | MCP |
|---|---|---|---|---|---|---|---|---|
| `db()` (Read access) | yes | — | yes | yes | yes | yes | yes | yes |
| `conn()` (Write access) | — | yes | yes | yes | yes | yes | yes | yes |
| `http()` (Client) | — | yes | yes | yes | yes | yes | yes | — |
| `auth` (Session info) | yes | yes | yes | yes | yes | — | — | yes |
| `dispatch_job` | — | yes | — | — | — | yes | yes | yes |
| `issue_token_pair` | — | yes | — | — | — | — | — | — |
| `step()` / `sleep()` | — | — | — | — | yes | — | — | — |
| `heartbeat()` / `save()` | — | — | yes | — | — | yes | — | — |
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
| `Internal` / all others | 500 | `INTERNAL_ERROR` | Server-side error; details never leak to clients. |

## CLI Command Reference

| Command | Purpose |
|---|---|
| `forge new <name>` | Scaffolds a new project from a template. |
| `forge generate` | Synchronizes backend changes with frontend bindings and types. |
| `forge check` | Runs linting, formatting, and validates SQL and bindings. |
| `forge migrate <up|down>`| Manages database migrations. |
| `forge test` | Executes full-stack E2E tests using Playwright. |

## Project File Standards
- **Source Code**: Editable logic resides in `src/functions/`, `src/schema/`, and `src/utils/`.
- **Generated Code**: **MANDATE:** Never edit generated files. See [Pitfalls](./pitfalls.md#1-generated-code).
- **Migrations**: Create new SQL files in `migrations/`. Always include `-- @up` and `-- @down` markers. Do not use `IF NOT EXISTS` clauses; migrations should be deterministic.

## Cargo Features

Subsystems are feature-gated; default is `full`. Opt out with `default-features = false` and pick a preset.

| Feature | Bundles | Pulls (extra crates) |
|---|---|---|
| `gateway` | HTTP RPC + SSE + OAuth + MCP + webhooks + signals | axum, tower, tower-http, jsonwebtoken, bcrypt, ed25519 |
| `jobs` | PG-backed queue + worker | — |
| `workflows` | Durable workflow executor | — |
| `cron` | Cron scheduler | — |
| `daemons` | Long-running daemon runner | — |
| `geoip` | IP→country + MaxMind reader (req. `gateway`) | db_ip (build-time download — breaks air-gapped CI), maxminddb |
| `otel` | OpenTelemetry trace/metric/log exporters | opentelemetry ×6, reqwest-otlp |

Presets: `full` = all (default) · `worker` = jobs+workflows+cron+daemons+otel (no HTTP) · `api` = gateway+otel (no workers) · `minimal` = gateway only.

```toml
forge = { version = "0.9", default-features = false, features = ["worker"] }
```

`#[forge::job/cron/workflow/daemon/webhook/mcp_tool]` without the matching feature errors at the generated `forge::Auto{Job,Cron,Workflow,Daemon,Webhook,McpTool}` reference. Without `otel`, observability call sites (`record_*`) become no-op stubs and `tracing-subscriber` still logs to stderr.

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

**Workflow signature (frozen)**: FNV-1a 64-bit hash of: name → version → step keys (sorted) → wait keys (sorted) → timeout_secs (u64 LE) → input type string → output type string. Never add fields to this derivation.

**Step name rules**: string literals only, max 64 chars, `[a-zA-Z0-9_-]`.

**`forge_*` reserved**: do not create application tables with this prefix.

**Config substitution**: `${VAR-default}` uses default only when var is *unset*. `VAR=""` (set to empty) expands to empty, not the default. `${VAR}` with no default and no env var preserves the literal `${VAR}` in the TOML (parse error likely).

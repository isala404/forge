# Pitfalls

Fast index of mistakes. Assume the happy path will fail — entities vanish, tokens expire, networks drop.

## 1. Generated Code

- **Never edit** `frontend/src/lib/forge/` (Svelte) or `frontend/src/forge/` (Dioxus). `forge generate` overwrites them. Fix the Rust source instead.
- Run `forge generate` after every backend change. Skipping causes runtime deserialization errors.
- `#[forge::model]` must come **before** `#[derive(...)]`:

```rust
// WRONG — derive expands before model sees the struct
#[derive(Debug, Clone)]
#[forge::model]
pub struct Item { ... }

// RIGHT
#[forge::model]
#[derive(Debug, Clone)]
pub struct Item { ... }
```

## 2. Environment & Configuration

- `ctx.env_require()` / `ctx.env_or()` — **not** `std::env::var()`. See [API Reference](./api.md#environment-variables). Mocks only hook the context methods.
- `ctx.http()` for outbound RPC; `ctx.raw_http()` only when you need streaming or custom redirects.
- Don't copy-paste config helpers (`app_url()`, etc.) across handlers. Extract to `src/utils/`.
- **TLS cert/key must be set together**: `[gateway.tls]` rejects configs that set only `cert_path` or only `key_path`. Set both to enable TLS, or neither to serve plain HTTP. Startup fails fast with a clear config error.
- **TLS cert/key files must be readable and valid PEM**: Startup fails fast if paths are missing, unreadable, or malformed. The error message includes the offending path. Fix at deploy time, not at first request.
- **Health checks must switch to HTTPS when TLS is enabled**: Load balancer / Kubernetes probes targeting `/_api/ready` will fail with TLS handshake errors if they're still configured for HTTP. Update target group protocol (ALB) or probe `scheme: HTTPS` (k8s) when enabling `[gateway.tls]`.
- **Certificate rotation requires restart**: `[gateway.tls]` does not hot-reload. Rolling deployment is the intended rotation mechanism.

## 3. Macros & Registration

- Handler fn must be `pub async fn` — private fns fail codegen.
- Don't include the type in the fn name (`heartbeat`, not `heartbeat_daemon` → avoids `HeartbeatDaemonDaemon`).
- Omit the args parameter entirely when a handler takes no input — no `Option<()>` or dummy structs.
- Handlers require auth by default; opt out with `public`.
- `public` and `unscoped` are orthogonal. `public` skips auth. `unscoped` skips the compile-time row-filter check (`WHERE user_id = ...`). A query can be authenticated but unscoped (admin dashboard), or public without unscoped (public queries already skip scope checks).
- Register every handler in `src/main.rs` via `.register_*::<NameType>()` or `.auto_register()`. Macros alone don't wire them in.
- Adding a new handler file under `src/functions/` requires `pub mod <name>;` in `src/functions/mod.rs`. The simplest path is `forge new <kind> <name>`, which writes the file and updates `mod.rs` (and `src/main.rs` if needed) for you. If you write the file by hand, remember the `pub mod` line — the macro generates the inventory entry, but the module must be reachable from the crate root.
- Attribute values like `log = "info"` must be quoted strings.
- **Method names that don't exist** — these compile-fail and waste a check cycle:
  - `ctx.auth()` on `MutationContext`. Use `ctx.user_id()?` directly (or `ctx.auth_context()` for the underlying struct).
  - `ForgeConn` imported as a path inside your handler. Don't name the type — `let mut conn = ctx.conn().await?` lets inference handle it.
- `forge check`, `forge generate`, `forge migrate`, and `forge test` walk up to find `forge.toml`. You can run them from any subdirectory; the resolved project root is printed at the start.

## 4. Database & Transactions

- Always use `sqlx::query!()` / `query_as!()` — never `sqlx::query()` / `query_as::<_,T>()`.
- **Offline cache discipline**:
  - `forge check` auto-prepares the `.sqlx/` cache when sources are newer, so day-to-day you don't need to think about prepare ordering. Pass `--no-prepare` in CI where the cache should already be correct.
  - For raw `cargo check` / `cargo build`, `SQLX_OFFLINE=true` is mandatory. Without it, sqlx validates every `query!()` against your live `DATABASE_URL`, including queries inside published `forge-runtime` files you cannot edit. Easiest fix: `eval "$(forge env)"` in your shell rc.
  - `forge migrate prepare` hard-fails when `cargo-sqlx` is missing. Install with `cargo install sqlx-cli --no-default-features --features postgres`.
  - A passing `cargo sqlx prepare` already implies a passing `SQLX_OFFLINE=true cargo check` for the same code — don't chain a redundant check call right after.
- Cast enums explicitly in SELECT: `status as "status: ScanStatus"`. Use `"column?"` only to override nullability inference.
- Dispatch jobs / workflows only from mutations. Transactions are on by default. Never set `transactional = false` on a mutation that dispatches — the macro errors at compile time. See [Patterns](./patterns.md#background-jobs).
- **Pick the right handle** — details in [API Reference](./api.md#database-access--three-shapes):

```rust
// Query: ForgeDb, standard sqlx convention
sqlx::query_as!(User, "...", id).fetch_one(ctx.db()).await?

// Mutation: ForgeConn for transactional writes
let mut conn = ctx.conn().await?;
sqlx::query_as!(User, "...", id).fetch_one(&mut conn).await?
```

- **`DbConn` has an inverted convention** — it is not a sqlx `Executor`. Call `.fetch_*` on the `DbConn`, passing the query: `db.fetch_optional(sqlx::query_as!(...))`. Also: `DbConn` only wraps `query_as!`. For `query!` / `query_scalar!` / `execute`, use `ctx.db()` or `&mut *conn`.
- Use a real DB in tests (`IsolatedTestDb`) — mocks hide migration bugs and constraint violations.
- Enable reactivity via `SELECT forge_enable_reactivity('table_name');` in migrations. Never hand-write triggers.
- Avoid `SELECT *` in subscribed queries — it forces table-level invalidation. Explicit column lists unlock row-level tracking.
- For enriched / joined reads, define a dedicated struct (e.g. `SiteSummary`). Returning the base model silently drops joined columns.

## 5. Workflows

- `ctx.sleep()`, not `tokio::sleep` — only `ctx.sleep()` persists across restarts.
- Step names are cache keys. Renaming breaks resume. Bump the workflow version instead.
- Signature mismatch at startup blocks runs. The runtime logs a warning at boot. Check for in-flight runs before removing an old version.
- Removing a deprecated version's code while runs are still in-flight strands them. The runtime logs per-group warnings at boot. Use admin endpoints to resolve: `POST /_api/admin/workflows/{id}/cancel` (with compensation) or `POST /_api/admin/workflows/{id}/force-abort` (terminal `cancelled_by_operator`).
- Always set a timeout on `wait_for_event` so stalls become observable.

## 6. Frontend

- Never call `refetch()` on an SSE-backed store. The stream pushes updates.
- Guard against `loading` / `error` / null `data`. See [Frontend](./frontend.md#subscription-state-shape).
- Let the generated auth helper manage tokens. Manual `localStorage` writes bypass SSE reconnect — see [Frontend](./frontend.md#authentication-and-session-management).
- Don't fetch inside `$effect` / `use_effect` — race conditions and leaks. Use subscription hooks.
- Route mutation errors to a global handler (`onMutationError` in Svelte, `on_mutation_error` in Dioxus). Users must see failures.

## 7. Authentication

- `jwt_secret` in `forge.toml` is required for `issue_token_pair()`. TTLs default to 1h / 30d if omitted.
- **JWT secret must be at least 32 bytes** — startup hard-fails with a config error pointing at `openssl rand -base64 32`. Shorter secrets are rejected before the server binds.
- Drop `ctx.conn()` before calling `issue_token_pair()` — it needs its own connection and will deadlock on pool exhaustion.
- Refresh calls must be unauthenticated — the built-in `refresh_token` provider does this, don't hand-roll.
- Reserve `Forbidden` for real permission violations. Using it for billing/plan state triggers the global `onAuthError` handler and logs the user out. Use `InvalidArgument` for business-rule rejections.
- **`audience_required` defaults to `true` when auth is enabled** — adding `jwt_audience` to an existing project breaks existing tokens that don't carry an `aud` claim. Set `audience_required = false` in `forge.toml` while migrating, then re-enable once all clients are issuing tokens with `aud`.
- **Reserved claim names**: `ClaimsBuilder::claim("sub", ...)` (and `exp`, `iat`, `nbf`, `jti`, `iss`, `aud`, `roles`) returns `ForgeError::InvalidArgument` at call time. Use the typed setters: `.user_id()`, `.role()`, `.audience()`. The `Custom` rate-limit key reads claims by name — but if the claim doesn't actually exist in the JWT, the bucket falls back to `"unknown"`, silently grouping all callers without that claim into one shared bucket.

## 8. Error Shape

- **Rate-limit retry delays are top-level**, not under `details`. In Svelte, `error.details?.retry_after_secs` is always `undefined` — check `error.retryAfterSecs` directly. In Dioxus, check `error.retry_after_secs`. The wire shape is `{ code, message, retry_after_secs?, details? }`.
- Match errors by `code` string (`"RATE_LIMITED"`, `"NOT_FOUND"`, `"UNAUTHORIZED"`) on the frontend, not by message text.

## 9. Custom Routes and Uploads

- **Custom routes live under `/_api`**: `ForgeBuilder::custom_routes(|pool| ...)` merges into the gateway router. A declared `/export/csv` resolves to `/_api/export/csv`. Document the full path to clients — writing the raw declaration is a common off-by-prefix bug.
- **Never `.unwrap()` `AuthContext`**: The auth middleware still forwards unauthenticated requests to your handler. `auth.user_id().unwrap()` panics and hits the workspace `clippy::unwrap_used` deny. Use `match auth.user_id()` with an early 401 return.
- **Don't re-implement auth in custom handlers**: Middleware already parses the JWT and injects `Extension<AuthContext>`. Do not reach for headers or parse tokens yourself.
- **Per-file upload cap is independent of total body**: `gateway.max_body_size` caps the full multipart body, but individual files are capped by `gateway.max_file_size` (defaults to `"10mb"`). A mutation that legitimately accepts a big file must declare `max_size = "…"`; that value becomes both the total and per-file limit for that endpoint.
- **CORS startup validation**: `cors_enabled = true` with an empty `cors_origins` list fails at startup with a config error. Mixing `"*"` with concrete origins in the same list also fails — pick one or the other.
- **Auth is off by default**: without `[auth]` in `forge.toml`, every RPC endpoint is callable without a token. Non-`public` handlers still check for a JWT if one arrives, but unauthenticated requests are not rejected at the middleware layer. Configure `jwt_secret` or `jwks_url` before deploying anything user-facing.
- **No per-function rate limits by default**: `[rate_limit]` sets the counter backend, but no limits are enforced without `#[forge::query(rate_limit = ...)]` annotations. Public or unauthenticated endpoints need explicit limits.
- **`trusted_proxies` must match your infrastructure**: when empty (default), `X-Forwarded-For` is ignored and the raw socket peer IP is used. If your app runs behind a load balancer, set `trusted_proxies` to its CIDR so rate limits and signals see real client IPs.
- **`signals.anonymize_ip` defaults to `true`**: raw IPs are not stored. If you set it to `false`, raw peer IPs land in `forge_signals_events` — ensure you have a lawful basis under GDPR before doing so.

## 10. Resilience & Hygiene

- Always check entity existence with `fetch_optional().await?.ok_or_else(|| ForgeError::NotFound(format!(...)))`. See [Resilience](./resilience.md#2-database-and-data-integrity).
- Include IDs and context in error messages.
- Delete commented-out code immediately — Git is the history.
- Run `forge check` after every change to catch orphans.
- Default rate-limit mode is `hybrid`: `key = "user"` and `key = "ip"` are per-node, so a `10/min` limit becomes effectively `10 × node_count` across the cluster. Fine for DDoS protection, wrong for billing-grade quotas. Set `[rate_limit] mode = "strict"` in `forge.toml` for cluster-exact counts (every check hits PG).

## 11. Code Reuse

- Don't hand-roll `SELECT * FROM users WHERE id = $1` in every handler. Extract `current_user(db, user_id)`. See [Recipes](./recipes.md#1-current-user-helper).
- Don't hand-roll auth storage in Svelte — the generated `auth.svelte.ts` (`setAuth`, `clearAuth`, `startRefreshLoop`) handles SSE reconnection. See [Svelte](./frontend/svelte.md#authentication-and-session-management).
- Don't `INSERT INTO forge_jobs` / `forge_workflow_runs` / `forge_signals_events` manually. Direct writes skip the outbox, break idempotency, and break SKIP LOCKED ordering. Use `ctx.dispatch_job()`, `ctx.start_workflow()`, `ctx.record_signal()`. `forge check` fails the build on raw writes.

## 12. Integration Anti-patterns

- **Email HTML inlined in handlers**: escaping, i18n, and preview-time testing become painful. Use [`askama`](https://docs.rs/askama) templates under `templates/`. See [Recipes](./recipes.md#4-transactional-email-askama--smtp).
- **`serde_json::Value` webhooks**: untyped payloads defer validation to runtime. Declare a typed struct — the macro deserialises for you.
- **String-matching error messages**: match `ForgeError` variants in Rust, internal codes (`"NOT_FOUND"`, `"UNAUTHORIZED"`) on the frontend. See [API Reference](./api.md#forgeerror-variants).
- **Hand-rolled HMAC verification**: use the `WebhookSignature` constructors. See [API Reference](./api.md#signature-constructors).
- **Forgetting `x-webhook-timestamp`**: non-Stripe schemes (`hmac_sha256`, `shopify_webhooks`, `ed25519`) reject any request without a fresh `x-webhook-timestamp: <unix-seconds>` header. If you control the sender, stamp the header. If you don't, set `replay_window_secs = 0` on the macro to disable enforcement (last-resort — you lose replay protection). Stripe is unaffected. This trap is easy to hit when triggering a webhook from your own frontend (`fetch`/`reqwest` in browser code): send `X-Webhook-Timestamp` next to `X-Webhook-Signature` or every call 401s. Tests that post with Playwright's `request` fixture bypass browser CORS, so cover the in-app button click too, or the gap stays hidden.
- **Provider SDKs for payments / AI / S3**: stay neutral — standard protocols (HMAC, S3 API, HTTP JSON) work everywhere. See [Recipes](./recipes.md).

## 13. Number Precision Across the Wire

- TypeScript / JavaScript represent every number as IEEE-754 double, so values above `Number.MAX_SAFE_INTEGER` (2^53 − 1, roughly 9e15) silently lose precision after `JSON.parse`. Forge codegen still maps Rust `i64` / `u64` to TS `number` to match the default serde JSON encoding.
- For values that genuinely need 64-bit precision — Snowflake / Twitter-style IDs, large monotonic counters, microsecond timestamps — declare the field as `String` in Rust and convert at the boundary, or use `serde_with::DisplayFromStr`. Anything that fits in `i32` (under ±2.1e9) is safe to keep as `i32`.
- `Instant` / `DateTime<Utc>` already serialise as RFC 3339 strings, so timestamps never hit this trap unless you explicitly model them as `i64`.

## 14. Operations and Deploy Gates

- **PostgreSQL < 18 is a startup hard-fail**: Forge v2 reads `current_setting('server_version_num')` from a temp pool connection and refuses to continue if the major version is below 18. The runtime relies on `pg_notification_queue_usage()`, partitioned `SET ACCESS METHOD`, `pg_stat_statements.toplevel`, and `NOWAIT` skip-locked semantics. There is no v1-style fallback. Upgrade local Docker images and managed-DB engines before bumping the framework version.
- **NOTIFY queue at 75 %**: `/_api/ready` returns 503 with `notify_queue_ok=false` once `pg_notification_queue_usage() >= 0.75`. Don't raise the threshold — fix the stuck consumer. Find it with `SELECT pid, application_name, query_start, query FROM pg_stat_activity WHERE wait_event='AsyncWait'` and `pg_terminate_backend()` the offender. The most common cause is a hung gateway node still holding `LISTEN` after a hang.
- **`migrations_ok=false` after deploy**: code shipped before `forge migrate up`. The check compares embedded system-migration count to `forge_system_migrations`. Order the rollout: migrate, *then* swap traffic.
- **`cluster_registered=false` for more than 10 s**: this node's row in `forge_nodes` was either never inserted (DB unreachable at boot) or marked dead by another node. Boot races clear within one heartbeat; persistent failures need a log dig.
- **Never edit `forge_jobs` / `forge_workflow_runs` rows directly to recover from a bad deploy.** Use the admin endpoints — they audit, they fire NOTIFY, and they integrate with the cancel/compensation path. Raw `UPDATE` skips compensation and leaves you guessing which actor did what.
- **Admin endpoints without `reason` are technically valid but useless** to the next operator reading the audit log. Always pass `{"reason": "<why>"}`.
- **Per-queue worker pools are reserved, not shared**: `[worker.queues.workflows] workers = 4` means up to 4 simultaneous `$workflow_resume` jobs across this node. They don't burrow into `default`'s slot. Misconfiguring this starves a queue silently — the work just doesn't pick up.

## 15. Svelte Reactive

- Don't wrap `listTodos$()` runes helpers in a `toReactive` adapter. They already manage lifecycle via `$effect` roots — wrapping reintroduces the leaks the rune form eliminates. See [Svelte](./frontend/svelte.md#using-svelte-5-runes).
- Never create a store inside a `$derived`. Opens a new SSE subscription every recomputation.
- Set `export const ssr = false;` in `+layout.ts`. SSE / `EventSource` / `localStorage` aren't available server-side.

## 16. Job `require_role` Is Dispatch-Time Only [11.F12]

`#[forge::job(require_role = "admin")]` enforces the role check at **RPC dispatch time** (when a client calls the job directly via `/_api/rpc/{job_name}`). When a mutation handler dispatches the job programmatically with `ctx.dispatch_job::<MyJob>(args).await?`, no role check is performed — the mutation's own auth already ran before the handler executed, and jobs do not persist the dispatcher's roles in the DB record.

At execution time the worker reconstructs an `AuthContext` from the persisted `owner_subject` (the principal's UUID or sub claim) with an **empty role list**. The `required_role` check cannot run there without a schema migration to persist roles at dispatch time.

**What this means in practice**:
- Jobs called via RPC: role enforced correctly.
- Jobs dispatched from a mutation via `ctx.dispatch_job()`: no role check. The mutation's own auth still ran, so only authenticated users can trigger the dispatch — but `require_role` on the job itself is not re-checked.
- If the job also has `require_role` and is dispatched programmatically, the runtime logs a `WARN` trace line (`job has require_role but roles are not persisted…`) to make the gap visible.

**Mitigations**:
- Apply `require_role` on the **mutation** that dispatches the job, not on the job itself, when the access control point is the dispatch call.
- If the job should only ever run for admin-dispatched contexts, enforce it inside the job handler by checking `ctx.user_id()` against an allowlist stored in your DB.
- Do not rely on `require_role` on a job for enforcement when the job may be dispatched programmatically.

## 17. Signals v1 → v2 Breaking Changes

`trackVital()` and `identifyUser()` were removed in v2. Use the current API instead:

| Removed (v1) | Replacement (v2) |
| --- | --- |
| `signals.trackVital(name, value)` | `signals.vital(name, value, extra?)` |
| `signals.identifyUser(userId, traits)` | `signals.identify(userId, traits?)` |

Both are available on the `ForgeSignals` instance returned by `getForgeSignals()` (Svelte) and `use_signals()` (Dioxus). Code using the old names will fail at runtime with a `TypeError: signals.trackVital is not a function` (Svelte) or a compile error (Dioxus — the method is removed from the struct).

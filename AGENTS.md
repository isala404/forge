# Forge Framework Development

Full-stack Rust framework. Single binary, PostgreSQL-only. Axum/Tokio/SQLx backend with SvelteKit or Dioxus frontends. Proc macros generate handler traits, codegen produces type-safe frontend bindings.

## Pre-1.0 Policy

Zero tech debt. Breaking changes are encouraged without a migration path if they produce a cleaner API or simpler internals. No backward compatibility shims, deprecated fields, legacy aliases, or "old way / new way" coexistence until stable release. If removing something makes the system better, remove it.

## Workspace

```
crates/forge           CLI + public lib (publishes as `forgex`). Builder pattern, auto-registration via inventory crate, template scaffolding.
crates/forge-core      Types, traits, contexts, errors, config, testing infra. All handler traits (ForgeQuery, ForgeMutation, ForgeJob, etc.) defined here.
crates/forge-macros    Proc macros. Transforms #[query]/#[mutation]/#[job]/etc into trait impls + inventory::submit! registration. SQL table extraction via sqlparser.
crates/forge-runtime   Gateway (Axum router, SSE, RPC dispatch), job worker (SKIP LOCKED polling), workflow executor, cron scheduler, daemon runner, reactivity (LISTEN/NOTIFY -> invalidation -> re-execute -> SSE fan-out), cluster coordination (advisory lock leader election), rate limiting, observability, signals (product analytics + diagnostics).
crates/forge-codegen   AST parser (syn) -> SchemaRegistry -> BindingSet IR -> emitters. TypeScript (SvelteKit) and Rust (Dioxus) targets. All type mapping in emit.rs (single source of truth).
packages/forge-svelte  @forge-rs/svelte npm package. Runtime: ForgeClient, stores, SSE transport, auth, ForgeSignals (analytics tracker).
packages/forge-dioxus  forge-dioxus crate. Dioxus WASM client runtime, hooks, signals, ForgeSignals (analytics tracker).
examples/with-svelte/  minimal, demo, realtime-todo-list (each a workspace member)
examples/with-dioxus/  minimal, demo, realtime-todo-list
benchmarks/app         Performance testing
docs/                  Docusaurus site
```

## Stack

Rust 1.92, edition 2024. Axum 0.8, Tokio 1.48, SQLx 0.8 (compile-time checked). PostgreSQL 18. Svelte 5 (runes, SvelteKit 2) / Dioxus 0.7. Bun for JS. OpenTelemetry via OTLP HTTP (port 4318).

## Commands

```bash
cargo build --workspace                  # all crates
cargo build -p forgex                    # CLI only
cargo test --workspace                   # unit tests (SQLX_OFFLINE=true, no DB)
cargo test -p forge-svelte-demo-template --features testcontainers  # integration (needs Docker)
cargo clippy --all-targets --all-features --workspace -- -D warnings
cargo fmt --all --check

# full CI-style template test (scaffolds project, starts PG, builds, runs playwright)
scripts/ci/test-template.sh with-svelte/demo target/debug/forge .
```

### Regenerating .sqlx cache

The `.sqlx/` directory holds compile-time query metadata for `SQLX_OFFLINE=true` builds. Regenerate it when queries change.

The workspace has a shared schema problem: `benchmarks/app` and the demo examples both define a `users` table with different shapes. The solution is a single "super" database with all tables merged, plus a combined `forge_migrations` table that satisfies both `migrations/executor.rs` (uses `version` + `checksum`) and `migrations/runner.rs` (uses `name` + `down_sql`).

```bash
# 1. Start a temporary PG instance
docker run -d --name forge-sqlx-pg -e POSTGRES_PASSWORD=forge -e POSTGRES_DB=forge \
  -p 5433:5432 postgres:18
until docker exec forge-sqlx-pg pg_isready -U postgres -d forge 2>/dev/null; do sleep 1; done

# 2. Apply system schema
docker exec -i forge-sqlx-pg psql -U postgres -d forge \
  < crates/forge-runtime/migrations/system/v001_initial.sql

# 3. Apply example migrations (demo covers users/trades/etc; todos adds todos table)
sed '/^-- @down/,$d' examples/with-svelte/demo/migrations/0001_initial.sql \
  | docker exec -i forge-sqlx-pg psql -U postgres -d forge
sed '/^-- @down/,$d' examples/with-svelte/realtime-todo-list/migrations/0001_todos.sql \
  | docker exec -i forge-sqlx-pg psql -U postgres -d forge

# 4. Add benchmark-only tables (counters; skip its users — conflicts with demo)
docker exec forge-sqlx-pg psql -U postgres -d forge -c "
  CREATE TABLE counters (
    id UUID PRIMARY KEY DEFAULT gen_random_uuid(),
    name VARCHAR(255) NOT NULL UNIQUE,
    value BIGINT NOT NULL DEFAULT 0,
    updated_by UUID REFERENCES users(id),
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
  );"

# 5. Create combined forge_migrations table (merges both internal schemas)
docker exec forge-sqlx-pg psql -U postgres -d forge -c "
  CREATE TABLE forge_migrations (
    id SERIAL PRIMARY KEY,
    version VARCHAR(255) NOT NULL DEFAULT '',
    name VARCHAR(255) NOT NULL DEFAULT '',
    applied_at TIMESTAMPTZ NOT NULL DEFAULT NOW(),
    checksum VARCHAR(64),
    execution_time_ms INTEGER,
    down_sql TEXT
  );"

# 6. Generate
DATABASE_URL="postgres://postgres:forge@localhost:5433/forge" \
  cargo sqlx prepare --workspace

# 7. Cleanup
docker stop forge-sqlx-pg && docker rm forge-sqlx-pg
```

## Crate Architecture

### forge-macros: Code Generation Pipeline

Each macro follows: parse attributes -> extract fn signature -> extract SQL tables (sqlparser) -> generate args struct -> generate trait impl -> emit inventory::submit! for auto-registration.

10 attribute macros: `query`, `mutation`, `job`, `cron`, `workflow`, `daemon`, `webhook`, `mcp_tool`, `model`, `forge_enum`.

Key files: `lib.rs` (entry points), `query.rs`, `mutation.rs`, `job.rs`, `cron.rs`, `workflow.rs`, `daemon.rs`, `webhook.rs`, `mcp_tool.rs`, `model.rs`, `enum_type.rs`, `sql_extractor.rs` (SQL parsing + table dependency extraction), `utils.rs` (duration parsing, case conversion).

Generated output per handler:
1. Args struct (if multiple params, or `type Args = ()` / single type)
2. Named struct (e.g., `GetUserQuery`)
3. Trait impl (`ForgeQuery` with `info()` + `execute()`)
4. `inventory::submit!(AutoQuery(|registry| registry.register_query::<GetUserQuery>()))`;

SQL table extraction: finds string literals in fn body, parses with sqlparser (PG dialect), extracts FROM/JOIN/INSERT/UPDATE/DELETE tables + SELECT columns. Falls back to regex for unparseable dynamic SQL.

Query macro validates: private queries must filter by `user_id` or `owner_id` in SQL. Compile-time error if missing. `#[query(unscoped)]` opts out for shared/admin data.

Mutation macro validates: detects `dispatch_job()`/`start_workflow()` without `transactional` flag and errors at compile time.

Cron macro validates: cron expression checked at compile time via `cron` crate.

Workflow macro: supports `name`, `version`, `active`/`deprecated`, `timeout`, `public`, `require_role` attributes. Extracts step/wait keys from function body via AST visitor. Derives FNV-1a signature from persisted contract (name, version, step keys, wait keys, timeout, input/output types). Fails if both `active` and `deprecated` are set.

### forge-core: Types and Traits

Handler traits: `ForgeQuery`, `ForgeMutation`, `ForgeJob`, `ForgeCron`, `ForgeWorkflow`, `ForgeDaemon`, `ForgeWebhook`, `ForgeMcpTool`. All follow pattern: `type Args`, `type Output`, `fn info() -> XInfo`, `fn execute(ctx, args) -> Pin<Box<Future>>`.

`WorkflowInfo` fields: `name`, `version`, `signature`, `is_active`, `is_deprecated`, `timeout`, `http_timeout`, `is_public`, `required_role`. `WorkflowStatus` variants: Created, Running, Waiting, Completed, Compensating, Compensated, Failed, BlockedMissingVersion, BlockedSignatureMismatch, BlockedMissingHandler, RetiredUnresumable, CancelledByOperator.

Contexts: `QueryContext` (db pool, auth, env), `MutationContext` (+ conn, http circuit breaker, dispatch, token issuer, outbox buffer), `JobContext` (+ progress channel, saved data, cancellation), `WorkflowContext` (+ step states, compensation handlers, suspend signal, durable sleep), `CronContext`, `DaemonContext` (+ shutdown signal), `WebhookContext`, `McpToolContext`.

ForgeError variants: Config, Database, Function, Job, JobCancelled, Cluster, Serialization, Deserialization, Io, Sql, InvalidArgument(400), NotFound(404), Unauthorized(401), Forbidden(403), Validation(400), Timeout(504), Internal(500), InvalidState, WorkflowSuspended, RateLimitExceeded(429).

Config: `ForgeConfig` loaded from TOML with `${ENV_VAR}` and `${VAR-default}` substitution. Sections: project, database (pool isolation: default/jobs/observability/analytics), gateway, function, worker, cluster, auth, mcp, observability, signals.

Testing module: `TestQueryContext`, `TestMutationContext`, `TestJobContext`, `TestCronContext`, `TestWorkflowContext`, `TestDaemonContext`, `TestWebhookContext`. Builders with `.as_user()`, `.with_role()`, `.with_claim()`, `.with_tenant()`, `.with_pool()`, `.with_env()`, `.mock_http()`. Assertion macros: `assert_ok!`, `assert_err!`, `assert_err_variant!`, `assert_job_dispatched!`, `assert_workflow_started!`, `assert_http_called!`.

### forge-runtime: Server and Workers

**Gateway** (`gateway/`): Axum router. Routes: `/_api/rpc/{function}` (POST), `/_api/rpc/{function}/upload` (POST multipart), `/_api/events` (GET SSE), `/_api/subscribe` (POST), `/_api/health`, `/_api/ready`. Middleware stack: concurrency limit -> timeout -> CORS -> auth (JWT validation) -> tracing.

**RPC dispatch** (`function/`): `FunctionRouter` checks auth/rate limits -> `FunctionExecutor` applies timeout -> calls handler. Queries check cache first. Mutations flush outbox buffer (buffered job/workflow dispatches) after handler returns.

**Reactivity** (`realtime/`): `ChangeListener` (dedicated PG connection, LISTEN on `forge_changes` channel) -> `InvalidationEngine` (debounce 50ms quiet / 200ms max, coalesce by table) -> `SubscriptionManager` (DashMap with 64 shards, dedup by hash of query+args+auth_scope) -> `Reactor` (re-execute affected queries, hash comparison, bounded concurrency 64) -> `SessionServer` (SSE fan-out via mpsc channels). Adaptive tracking: row-level (<100 subs) vs table-level (>100 subs) per table.

**Jobs** (`jobs/`): `JobQueue` (PG-backed, SKIP LOCKED). `Worker` polls with semaphore-bounded concurrency. Claim: `FOR UPDATE SKIP LOCKED` ordered by priority DESC, scheduled_at ASC. `JobExecutor` spawns task, creates progress channel, applies timeout. Stale reclaim after 5min. Status: Pending -> Claimed -> Running -> Completed/Failed/DeadLetter/Cancelled.

**Workflows** (`workflow/`): Versioned, signature-guarded durable workflows. `WorkflowRegistry` keyed by (name, version) with one active version per name. `WorkflowExecutor` persists state to DB, pins new runs to active version+signature. Resume requires exact version+signature match; mismatches mark run as blocked. Steps cached by name (completed steps skip on resume). Compensation runs in reverse. Durable sleep survives restarts. Wait for external events with timeout. Parallel steps via `ParallelBuilder`. On startup, definitions upserted to `forge_workflow_definitions`; signature conflict under same name+version fails startup. `/_api/ready` reports unhealthy if blocked runs exist. Operator terminal actions: cancel_by_operator, retire_unresumable.

**Cron** (`cron/`): Leader-only execution via advisory lock. Exactly-once via UNIQUE constraint on (cron_name, scheduled_time). Catch-up for missed executions.

**Daemons** (`daemon/`): `DaemonRunner` runs all registered daemons concurrently. Leader-elected (advisory lock) or replicated (per-node). Auto-restart on failure. Shutdown signal via `AtomicBool`.

**Cluster** (`cluster/`): `LeaderElection` via `pg_try_advisory_lock`. Lock held by connection. `NodeRegistry` for node discovery and heartbeat.

**Database** (`db/`): `Database` struct with primary + replicas (round-robin, health-checked) + isolated pools (jobs, observability, analytics).

### forge-codegen: Frontend Binding Generation

Pipeline: `parse_project(src_dir)` scans .rs files with syn -> `SchemaRegistry` (tables, enums, functions) -> `BindingSet::from_registry()` (pre-computes has_upload, is_custom_args, sorts deterministically) -> target emitters.

`emit.rs`: Single source of truth for ALL type mapping. `ts_type()` and `dioxus_type()` functions. Rust->TS: String/Uuid/Instant/LocalDate/LocalTime->"string", i32/i64/f64->"number", bool->"boolean", Option<T>->"T|null", Vec<T>->"T[]", Upload->"File|Blob".

TypeScript output: `types.ts` (interfaces + type unions), `api.ts` (RPC functions + subscription factories), `reactive.svelte.ts` (Svelte 5 runes wrappers), `stores.ts` (re-exports from @forge-rs/svelte), `auth.svelte.ts` (if auth enabled).

Dioxus output: `types.rs` (structs + builders), `api.rs` (async fns + use_* hooks + use_*_live subscriptions), `mod.rs`.

Context parameter detection: structural check for types ending with "Context". Not string-based.

### forge (CLI): Commands

`forge new`: Embedded templates (include_dir!), {{placeholder}} substitution, post-setup: generate -> format -> lockfile -> git init. Debug builds patch Cargo.toml with `[patch.crates-io]` pointing to local workspace crates.

`forge generate`: Detect frontend target (SvelteKit: svelte.config.js, Dioxus: Dioxus.toml) -> parse_project -> generate bindings -> svelte-kit sync (SvelteKit only).

`forge test`: cargo test -> start docker compose if needed -> install playwright -> bunx playwright test -> teardown docker if we started it.

`forge check`: Validates forge.toml, project structure, migrations, functions, schema, .sqlx cache, cargo check, clippy, frontend tooling.

`forge migrate`: up/down/status/prepare subcommands. Advisory lock for cluster safety.

Builder: `Forge::builder().config(cfg).auto_register().build()?.run().await`. Auto-registration iterates `inventory::iter::<AutoQuery>` etc.

## Coding Rules (derived from codebase)

### Enforced by Workspace Lints (deny)
- `unsafe_code`: no unsafe anywhere
- `dead_code`: remove all dead code
- `clippy::panic`: no panic! macros
- `clippy::unimplemented`: no unimplemented! macros
- `clippy::unwrap_used`: no .unwrap() calls (use .expect() with reason or proper error handling)
- `clippy::indexing_slicing`: no direct slice indexing (use .get())

### SQL
- Always use `sqlx::query!()` and `sqlx::query_as!()` macros for compile-time checking. Never use runtime `sqlx::query()` or `sqlx::query_as::<_, T>()`.
- Use `"column!"` suffix for non-nullable, `"column?"` for nullable in query macros.
- Type cast enums in SELECT: `role as "role: UserRole"`.
- `SQLX_OFFLINE=true` in CI. `.sqlx/` directory caches query metadata for offline compilation.

### Error Handling
- Chain `.map_err(|e| ForgeError::Variant(e.to_string()))?` for explicit error conversion. Pick the specific ForgeError variant.
- Use `?` operator freely after `.map_err()`.
- Never `.unwrap()`. Never `panic!()`.

### Naming
- Files: snake_case (mod.rs, registry.rs, executor.rs)
- Structs/Enums: PascalCase, variants PascalCase
- Functions: snake_case
- Constants: SCREAMING_SNAKE_CASE

### Derive Order
`Debug, Clone, [Copy], [PartialEq, Eq], [Hash], [Serialize, Deserialize], [Default], [sqlx::Type], [JsonSchema]`

Serde: `#[serde(rename_all = "snake_case")]` on enums. SQLx: `#[sqlx(type_name = "pg_type", rename_all = "lowercase")]`.

### Struct Field Order
Identifiers (id, name) -> simple scalars (port, status) -> collections (roles, capabilities) -> complex types (db_pool, http_client) -> timestamps (created_at) -> optionals (error, last_heartbeat).

### Imports
- Library crates: explicit imports, `use crate::module::Type`. No glob imports in library code.
- Example apps: `use forge::prelude::*` is fine.

### Tests
- All tests use `#[tokio::test]`, never bare `#[test]`.
- Inline in modules: `#[cfg(test)] mod tests { }`.
- DB integration tests gated: `#[cfg(all(test, feature = "testcontainers"))]`.
- Use framework assertion macros: `assert_ok!`, `assert_err!`, `assert_err_variant!`, `assert_job_dispatched!`.

### Async
- `tokio::spawn(async move { })` for background tasks.
- `Arc<AtomicBool>` for cross-thread flags (shutdown signals, leader status).
- `Arc<Mutex<>>` or `Arc<RwLock<>>` for shared async state. `DashMap` for concurrent maps (sharded).
- Bounded channels (`mpsc`, `broadcast`) for inter-task communication.

### Documentation
- Every public struct, enum, trait, function gets `///` doc comment.
- Module-level docs use `//!`.

### Release Profile
LTO enabled, single codegen unit, symbols stripped.

## CI Pipeline

`.github/workflows/ci.yml`:
1. **validate**: fmt check -> clippy (warnings = errors) -> cargo test --workspace (SQLX_OFFLINE)
2. **workspace-integration** (needs validate, runs on PR + main): Postgres 18 service, `cargo test -p forge-svelte-demo-template --features testcontainers`, `cargo test -p todo --features testcontainers`
3. **pr-smoke** (PR only, needs validate): matrix of `with-svelte/demo` + `with-dioxus/demo`, calls the reusable `template-smoke.yml` workflow
4. **integration** (main push only, needs validate): matrix of all 6 templates, calls the reusable `template-smoke.yml` workflow

`.github/workflows/template-smoke.yml` (reusable, `workflow_call`): inputs `template`, `ref` (optional), `timeout-minutes` (default 20). Steps: checkout at ref -> build forge CLI -> install dioxus-cli if dioxus template -> `scripts/ci/test-template.sh` (scaffold with `forge new`, patch deps to local workspace, `forge check`, install Playwright, `forge test`). Concurrency keyed on `(ref or github.ref) + template` so repeated runs on the same template cancel the older one. Uploads Playwright report on failure.

`.github/workflows/chatops.yml`: triggered by PR comments. `/test-template [name|all]` runs the reusable workflow against PR head SHA (30 min timeout). `/squash-merge` arms auto-merge. Gated on `author_association` (OWNER/MEMBER/COLLABORATOR).

Environment: `SQLX_OFFLINE=true`, `CARGO_TERM_COLOR=always`, `CARGO_INCREMENTAL=0`, `FORGE_TEST_ARTIFACT_DIR=/tmp/forge-test-artifacts`. Path-ignore filters skip CI entirely for docs, markdown, and LICENSE changes.

## Testing Examples (E2E)

Playwright tests in `frontend/tests/`. Config: `playwright.config.ts` (chromium only, sequential, globalSetup waits for backend health).

Fixtures from `tests/fixtures.ts`:
- `rpc(fn, args)`: direct HTTP POST to `/_api/rpc/{fn}`
- `gotoReady(path)`: navigate + wait for `/_api/subscribe` 200 response (SSE fully wired)
- `uniqueId(prefix)`: `${prefix}-${Date.now()}-${random}`
- `ACTION_TIMEOUT`: 5s local, 15s CI

Always `gotoReady()` before testing reactive data. Run frontend with `bun run dev` (not Docker build).

Note: Integration testing means running `target/debug/forge test` on each example one by one.

## Key Internal Flows

**Request**: Client -> Axum router -> auth middleware (JWT) -> tracing -> RpcHandler -> FunctionRouter (auth check, rate limit) -> FunctionExecutor (timeout) -> handler -> response.

**Subscription**: Client SSE /events -> SseState (session tracking) -> POST /subscribe -> SubscriptionManager (dedup by hash) -> Reactor. On DB change: ChangeListener -> InvalidationEngine (debounce) -> find affected groups -> re-execute queries (bounded 64) -> hash compare -> SSE push.

**Job dispatch**: Mutation calls dispatch_job() -> buffered in OutboxBuffer -> flushed after transaction commit -> inserted to forge_jobs table. Worker polls with SKIP LOCKED -> claims batch -> semaphore-bounded execution -> complete/fail/retry.

**Macro expansion**: `#[forge::query] pub async fn get_users(ctx: &QueryContext) -> Result<Vec<User>>` -> generates `GetUsersQuery` struct + `ForgeQuery` impl with `FunctionInfo` (name, cache_ttl, table_dependencies extracted from SQL, selected_columns) + `inventory::submit!` for auto-registration.

**Signals** (product analytics + diagnostics): Auto-captures RPC calls in `FunctionExecutor`. Client trackers (`ForgeSignals` in forge-svelte/forge-dioxus) send page views, custom events, and error reports to `/_api/signal/{event,view,user,report}`. Events are buffered via mpsc channel and batch-inserted using UNNEST into `forge_signals_events` (partitioned by month). Daily-rotating visitor ID from `SHA256(ip+ua+daily_salt)` for GDPR-compliant tracking without cookies. Correlation IDs (`x-correlation-id` header) link frontend events to backend RPC calls. Bot detection via UA patterns. Grafana dashboard over PostgreSQL datasource. Config: `[signals]` in forge.toml (enabled by default).

## Extending the Framework

**New handler type**: Add trait in forge-core -> add macro in forge-macros -> add registry + executor in forge-runtime -> add Auto* type in forge/auto_register.rs -> add codegen support in forge-codegen.

**New type mapping**: Add variant to `RustType` enum in forge-core/schema -> add cases to `emit::ts_type()` and `emit::dioxus_type()` in forge-codegen/emit.rs. Both emitters automatically pick it up.

**New macro attribute**: Parse in the macro's attribute handler -> add field to the Info struct in forge-core -> handle in forge-runtime executor.

**New CLI command**: Add variant to `Commands` enum in forge/cli/mod.rs -> implement command module -> wire execute().

## Documentation Policy

Any change to the framework (new feature, config option, endpoint, API, behavior change, bugfix that alters user-facing behavior) **must** update both documentation surfaces in the same changeset:

1. **User docs** (`docs/docs/`): Docusaurus site. Pages organized by category (Start, Build, Connect, Ship, Scale, Reference, Tutorials). Sidebar defined in `docs/sidebars.ts`. This is what end users read.

2. **Skill references** (`docs/skills/forge-idiomatic-engineer/references/`): Token-efficient lookup docs consumed by the forge-idiomatic-engineer AI skill. Four files:
   - `api.md`: macro attributes, context methods, forge.toml config, CLI, endpoints
   - `frontend.md`: stores, reactivity, uploads, errors, signals client API
   - `patterns.md`: backend patterns, auth, integrations, testing, operations, signals
   - `pitfalls.md`: common mistakes and anti-patterns

Source code and docs must stay in sync. If you add a config option, it goes in `docs/docs/ship/configuration.mdx` AND `api.md`. If you add an endpoint, it goes in the relevant `docs/docs/` page AND `api.md`. If you change client behavior, it goes in `docs/docs/connect/` or `docs/docs/ship/` AND `frontend.md`. If you discover a new pitfall, it goes in `pitfalls.md`. Don't ship code without updating both.

## Release Flow

Pre-1.0: never bump to 1.x.x. Changelog-driven pipeline parses version from CHANGELOG.md, validates, builds, publishes.

### Steps

1. `cargo fmt --all` across entire workspace
2. Frontend lint/format: eslint, tscheck, prettier on examples + packages
3. Regenerate `.sqlx/` cache using the steps under [Regenerating .sqlx cache](#regenerating-sqlx-cache), then commit any changes
4. `cargo test --workspace` (SQLX_OFFLINE=true)
5. `target/debug/forge test` on every example (with-svelte/minimal, with-svelte/demo, with-svelte/realtime-todo-list, with-dioxus/minimal, with-dioxus/demo, with-dioxus/realtime-todo-list)
6. If any step 1-5 errors: fix root cause, not bandaids. Iterate until clean.
7. Sub-agents review each commit since last tag (`git log $(git describe --tags --abbrev=0)..HEAD --oneline`), summarize main changes per commit
8. Write CHANGELOG.md: new `## [X.X.X] - YYYY-MM-DD` section under `[Unreleased]` with Added/Changed/Fixed/Removed. Call out breaking changes explicitly. Update comparison links at bottom.
9. Commit: `git commit -m "Prepare release X.X.X"` using user's git config. Single line, no co-author.
10. Push to main, monitor CI (`gh run watch`). All jobs must pass.
11. Trigger release: `gh workflow run release.yml`. Monitor with `gh run watch`. Inspect publish jobs with `gh run view`.
12. Verify all the steps of the release pipelines passed and packages are live on crates.io and npm.

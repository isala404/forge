Vision
FORGE is a framework for building full-stack applications where PostgreSQL is the only infrastructure dependency. Instead of assembling Redis, Kafka, and service meshes, adopters get auth, jobs, crons, workflows, real-time subscriptions, and observability out of the box. Nodes scale horizontally by sharing PostgreSQL as the coordination layer—no gRPC mesh, no gossip protocols. Workers register capabilities (GPU, high-CPU) and the scheduler assigns jobs intelligently via database queries. The framework handles multi-tenancy, rate limiting, and partitioning so adopters focus on business logic, not infrastructure. Target scale: ~100k MAU per deployment, 99% uptime, with the acceptable failure mode that DB down = service down.

Tooling

- Stack: Rust (backend), Svelte 5 + TypeScript (frontend), PostgreSQL
- Package manager: cargo (backend), bun (frontend)
- Test: LIBRARY_PATH="/opt/homebrew/opt/libiconv/lib" cargo test
- Lint: cargo clippy | Format: cargo fmt
- Dev: ./dev.sh [setup|start|db|logs|clean|all]
- CLI install: cargo install --path crates/forge
- Docs: cd website && bun run start (Docusaurus 3.9.2)

Architecture

- Single binary: gateway + functions + workers + scheduler
- PostgreSQL only (no Redis/Kafka) for data + coordination
- WebSocket for real-time, gRPC for inter-node mesh

Crates

- forge: CLI + runtime (ForgeBuilder pattern)
- forge-core: traits, types, contexts (no tokio dep except spawn)
- forge-macros: proc macros (#[forge::model], #[query], #[mutation], etc.)
- forge-runtime: executors, registries, gateway, dashboard
- forge-codegen: TypeScript generator, source parser (syn)

Key Patterns

- Proc macros use forge::forge_core:: paths (re-exported)
- User functions take &QueryContext, &MutationContext (references)
- axum 0.7+ routes: {param} not :param
- FromStr trait for enum parsing (not inherent from_str)
- ForgeConfig.parse_toml() not from_str() (clippy)
- Context: ctx.db() accessor, not ctx.pool
- RPC: {} normalized to null, response uses `data` field

Database

- Advisory locks: leader election (0x464F5247 prefix), migrations (0x464F524745)
- SKIP LOCKED: job claiming without double-processing
- Tables: forge_nodes, forge_leaders, forge_jobs, forge_cron_runs, forge_workflow_runs/steps, forge_sessions/subscriptions, forge_metrics/logs/traces, forge_migrations

Migrations

- Directory: migrations/ with 0001_xxx.sql files
- Up/down markers: `-- @up` and `-- @down`
- MigrationRunner: advisory lock for mesh-safe deploys
- Built-in: 0000_forge_internal (system tables)

Reactivity

- Pipeline: ChangeListener -> InvalidationEngine -> Reactor -> WebSocket
- Triggers: forge_enable_reactivity(table) creates NOTIFY triggers
- Read set: query name patterns (get_X/list_X -> table X)
- Hash updates: must update last_result_hash after sending, not during (lock ordering)

Frontend

- Generated runtime in $lib/forge/ (types, client, stores, api)
- query(): async Promise | subscribe(): Svelte store
- Job/Workflow trackers: createJobTracker(), createWorkflowTracker()
- Svelte 5: no props destructuring at module level, use props.\* in closures
- ForgeProvider: set context immediately (not onMount), const for $state objects

Dashboard

- Routes: /\_dashboard/ (pages), /\_api/ (REST)
- Dispatch: POST /\_api/jobs/{type}/dispatch, /\_api/workflows/{name}/start
- Chart.js via CDN with fallback

Durable Workflows

- Suspend: ctx.sleep(Duration), ctx.sleep_until(DateTime), ctx.wait_for_event(name, timeout)
- Resume: WorkflowScheduler polls suspended_at, wake_at, waiting_for_event
- Events: forge_workflow_events table, EventStore.send_event(), consume_event()
- State: WorkflowState enum (Pending, Running, Suspended, Completed, Failed, Cancelled)

Rate Limiting

- Token bucket: forge_rate_limits table with atomic UPSERT
- Keys: User, Ip, Tenant, UserAction, Global
- Config: requests per window, refill_rate()

Multi-tenancy

- TenantContext: None, Strict(Uuid) isolation modes
- require_tenant(), tenant_id() accessors
- Claims.tenant_id() from JWT custom claims

Parallel Workflows

- ParallelBuilder.step(name, fn).step_with_compensate(name, fn, comp).run()
- Caches completed steps, runs compensation on failure

Table Partitioning

- PartitionManager.ensure_partition(table, granularity)
- PartitionGranularity: Hour, Day, Week, Month
- cleanup_old_partitions() for retention

Adaptive Tracking

- AdaptiveTracker switches Row↔Table based on subscription counts
- row_threshold, table_threshold for hysteresis
- TrackingMode: None, Table, Row, Adaptive

CLI Scaffolding

- Templates in crates/forge/templates/ with .tmpl extension
- template::render() for {{var}} replacement, template_vars! macro
- Template vars: "name" and "project_name" both set to project name
- include_str!() embeds templates at compile time
- Directories: project/, frontend/, runtime/
- new.rs must include_str! and fs::write for each template file
- Single binary: `cargo build --features embedded-frontend` embeds frontend via rust-embed

Template Features Demonstrated

Schema:
- Enum: sqlx::Type with #[sqlx(type_name, rename_all)]
- #[default] for default enum variant

Queries:
- Caching: #[forge::query(cache = "30s")]
- Public endpoint: #[forge::query(cache = "30s", public)]
- Timeout: #[forge::query(timeout = 10)]

Mutations:
- Basic: #[forge::mutation]
- With timeout: #[forge::mutation(timeout = 30)]
- Role-protected (commented): #[forge::mutation(require_auth, require_role("admin"))]

Actions:
- With timeout: #[forge::action(timeout = 60)]
- ctx.http() for external API calls (ZenQuotes API example in template)

Jobs:
- Retry: #[retry(max_attempts = 3, backoff = "exponential")]
- Idempotency: #[idempotent]
- Priority: #[priority = "low"]
- Worker capability: #[worker_capability = "general"]
- ctx.heartbeat() for long-running jobs
- ctx.is_retry(), ctx.is_last_attempt() for retry detection
- ctx.progress(percent, message) for progress reporting

Crons:
- Schedule: #[forge::cron("* * * * *")]
- Timezone: #[timezone = "UTC"]
- Catch-up: #[catch_up], #[catch_up_limit = 5]
- ctx.delay(), ctx.is_late() for delay detection
- ctx.is_catch_up for catch-up run detection
- ctx.log.info/warn/error/debug() for structured logging

Workflows:
- Version: #[version = 1], #[timeout = "24h"]
- Manual step tracking: ctx.is_step_completed(), ctx.record_step_start/complete()
- Durable sleep: ctx.sleep(Duration) - survives server restarts
- Resumption detection: ctx.is_resumed()
- Deterministic time: ctx.workflow_time()
- Advanced patterns (commented): parallel(), fluent step API, wait_for_event()

Testing:
- Per-function-type contexts: TestQueryContext, TestMutationContext, TestActionContext, TestJobContext, TestCronContext, TestWorkflowContext
- Builder pattern: .as_user(), .with_role(), .with_claim(), .with_tenant(), .with_pool()
- MockHttp with pattern matching, request recording, verification (assert_called, assert_called_times, assert_not_called)
- MockJobDispatch, MockWorkflowDispatch for dispatch verification
- Assertion macros: assert_ok!, assert_err!, assert_err_variant!, assert_job_dispatched!, assert_workflow_started!, assert_http_called!
- Helper functions: assert_json_matches(), error_contains(), validation_error_for_field()
- TestDatabase for zero-config database provisioning (uses DATABASE_URL or embedded Postgres)
- Feature flag: forge = { features = ["testing"] } in dev-dependencies
- Macros re-exported at forge crate root and in prelude

Config (forge.toml):
- [project], [database], [gateway], [observability] sections
- Commented: [function], [worker], [auth], [rate_limit], [cluster], [node]

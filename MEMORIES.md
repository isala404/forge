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

Frontend
- Generated runtime in $lib/forge/ (types, client, stores, api)
- query(): async Promise | subscribe(): Svelte store
- Job/Workflow trackers: createJobTracker(), createWorkflowTracker()
- Svelte 5: no props destructuring at module level, use props.* in closures
- ForgeProvider: set context immediately (not onMount), const for $state objects

Dashboard
- Routes: /_dashboard/ (pages), /_api/ (REST)
- Dispatch: POST /_api/jobs/{type}/dispatch, /_api/workflows/{name}/start
- Chart.js via CDN with fallback

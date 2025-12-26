Phase Workflow (CRITICAL - always follow)
1. Read reference files for the phase
2. Implement the phase (read more references if needed)
3. Run: cargo fmt && cargo check && LIBRARY_PATH="/opt/homebrew/opt/libiconv/lib" cargo test
4. Update MEMORIES.md and PROGRESS.md
5. Commit with descriptive message (never git push)
6. Move to next phase

Tooling
- Stack: Rust (backend), Svelte 5 + TypeScript (frontend)
- Package manager: cargo (backend), bun (frontend)
- Test command: LIBRARY_PATH="/opt/homebrew/opt/libiconv/lib" cargo test
- Lint command: cargo clippy
- Format command: cargo fmt
- macOS: brew install libiconv (required for stringprep/sqlx tests)
- Docker: postgres:alpine for local dev database
- CLI install: cargo install --path crates/forge

Architecture
- Single binary containing all components (gateway, functions, workers, scheduler)
- PostgreSQL as sole database and coordination layer (no Redis/Kafka required)
- gRPC mesh for inter-node communication
- WebSocket for real-time client subscriptions

Core Components
- Schema: Rust structs with proc macros (#[forge::model], #[forge::enum])
- Functions: query (read), mutation (write), action (side effects)
- Jobs: background tasks with SKIP LOCKED pattern
- Crons: scheduled tasks via leader-elected scheduler
- Workflows: multi-step durable processes with compensation
- Reactivity: LISTEN/NOTIFY + read set tracking for live queries
- Runtime: Forge struct in crates/forge/src/runtime.rs wires all components
- Builder: ForgeBuilder pattern for configuration before run()
- Testing: TestContext in forge-runtime/src/testing/ for integration tests

Key Patterns
- Dependency injection via context (QueryContext, MutationContext, JobContext)
- PostgreSQL advisory locks for leader election
- SKIP LOCKED for job claiming (no double-processing)
- Table partitioning for high-churn tables (jobs, logs, metrics)
- Proc macros use forge::forge_core:: paths (re-exported from forge crate)
- User functions take &QueryContext and &MutationContext (references)
- axum 0.7+ route syntax uses {param} not :param
- Svelte 5: avoid destructuring $props() at module level, access props.* inside closures
- Function registration: call builder.function_registry_mut().register_query::<FnQuery>() before .config()
- Proc macros: query fn → FnQuery struct, mutation fn → FnMutation struct
- RPC: omit args field for no-arg functions (unit type), response uses `data` field not `result`
- Migrations: use migrations/ directory with numbered SQL files (0001_xxx.sql)
- Migrations: MigrationRunner uses advisory lock for mesh-safe concurrent deploys
- Migrations: built-in FORGE tables versioned as 0000_forge_internal_v1

Frontend
- Auto-generated TypeScript types from Rust schema
- Svelte 5 runes integration for reactivity
- WebSocket subscriptions with automatic reconnect

Database Tables
- forge_nodes: cluster membership
- forge_leaders: leader election state
- forge_jobs: job queue
- forge_cron_runs: cron execution history
- forge_workflow_runs/steps: workflow state
- forge_events: change tracking
- forge_metrics/logs/traces: observability
- forge_sessions/subscriptions: WebSocket state

# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Cargo feature flags for runtime subsystems.** Forge subsystems are now opt-in via Cargo features. `forge-runtime` exposes `gateway`, `jobs`, `workflows`, `cron`, `daemons`, `geoip`, `otel`. The public `forge` crate composes them into presets: `full` (default — everything, transparent for existing apps), `worker` (background subsystems, no HTTP), `api` (gateway + OTel only), `minimal` (gateway only). Use `default-features = false` to opt into a slim build:
  ```toml
  forge = { version = "0.9", default-features = false, features = ["worker"] }
  ```
- **`release-fast` build profile.** Release-quality optimization without LTO or single-codegen-unit. Ideal for local smoke tests and ad-hoc benchmarks. Use with `cargo build --profile release-fast`.
- **`docs/scale/binary-size.mdx`** and an updated `api.md` skill reference cover features, presets, and build-profile/linker tuning.

### Changed

- **Dev profile slimmed.** `[profile.dev]` now uses `debug = "line-tables-only"`, `split-debuginfo = "unpacked"`, `codegen-units = 256`, and disables debug info on dependencies (`[profile.dev.package."*"] debug = false`). Cuts `target/` size and improves incremental rebuild latency.
- **Observability is now a no-op stub when `otel` is disabled.** Call sites (`record_fn_execution`, `record_pool_metrics`, etc.) compile to nothing without the feature; `tracing-subscriber` still emits structured logs to stderr.
- **GeoIP support is opt-in.** Disabling the `geoip` feature skips the build-time `db_ip` database download — unblocks builds in air-gapped environments and shaves several minutes off cold builds.

### Removed

- **`[realtime] change_tracking_row_threshold` config knob** (and its `adaptive_row_threshold` alias). The adaptive row-vs-table tracker that consumed this value was removed earlier in the v2 rewrite, leaving the field as a silent no-op. Operators who set it in `forge.toml` should delete the line.

### Notes

- Existing apps see no behavior change: `default = ["full"]` activates every subsystem just like before.
- Macro/feature mismatch (e.g. `#[forge::job]` without the `jobs` feature) produces a compile error at the generated `forge::AutoJob` reference, pointing users to enable the feature.
- Approximate cold-build savings on the demo template: `worker` -55%/-65% (compile/target), `api` -25%/-30%, `minimal` -65%/-75%.

## [0.9.0] - 2026-04-23

### Added

- Web Vitals ingestion endpoint (`POST /_api/signal/vital`) for LCP, CLS, INP, FCP, TTFB, navigation timing, long tasks, and resource events (up to 50 entries per batch). New `SignalEventType::WebVital` and `SignalEventType::ServerExecution` variants.
- Client SDKs (`@forge-rs/svelte` and `forge-dioxus`) auto-capture Web Vitals, `network.online`/`network.offline` transitions, and persist the pending event queue to `localStorage` so events survive reloads. New config flags: `autoWebVitals`, `autoNetworkEvents`, `respectDnt`, `persistQueue`. Manual `signals.vital(name, value, extra?)` API on both SDKs.
- Auto-emitted `server_execution` signals for every job, cron, workflow step, webhook, and daemon tick, plus `auth.failed` and `rate_limit.exceeded` diagnostic signals from gateway middleware. New `forge_runtime::signals::{emit_server_execution, emit_web_vital, emit_diagnostic, emit_raw}` helpers for handlers that want to emit outside the RPC path.
- GeoIP enrichment on every signal event. Embedded DB-IP Country Lite database ships by default (zero config, ISO country code in new `country` column). Optional `[signals] geoip_db_path = "..."` points at a MaxMind MMDB for city-level resolution (populates new `city` column).
- Webhook signature support for Stripe (`#[webhook(stripe_webhooks("SECRET_ENV"))]` with 5-minute replay window), Shopify (`shopify_webhooks`, HMAC-SHA256 base64), Standard Webhooks (`standard_webhooks`, Polar/Svix/Clerk compatible with `whsec_` and `polar_whs_` prefix handling), and Ed25519 asymmetric signatures (`ed25519("header", "PUBKEY_ENV")`).
- Reactive mutation helpers in generated Svelte bindings: mutations now return a `ReactiveMutation<Args, Result>` with `mutate`, `pending`, and `error` runes state.
- `gateway.max_file_size` config option (default `"10mb"`) separate from `gateway.max_body_size` (default `"20mb"`) so per-file upload caps and full-body RPC caps can be tuned independently.
- Dioxus `ForgeClientConfig` gains a `refresh_token` async provider for handling 401s, matching the Svelte client.
- `forge check` now scans `src/` for direct INSERT/UPDATE/DELETE against `forge_*` system tables and fails with guidance to use `ctx.dispatch_job()`, `ctx.start_workflow()`, or `ctx.issue_token_pair()` instead.
- New SRE Grafana dashboard (`forge-sre.json`) covering service health, jobs, workflows, reactor, crons, security, infra, errors, logs, and traces. Business dashboard expanded with geography, retention, funnel, and feature-adoption panels.

### Changed

- **BREAKING:** Custom routes registered via `custom_routes` now run through the gateway middleware stack (auth, CORS, tracing, concurrency limit, timeouts) and are merged under `/_api`. Handlers that assumed a bare axum router without Forge middleware need updating.
- **BREAKING:** `forge_new` scaffolded projects now pin `[package] version` to `1.0.0` instead of inheriting the forge workspace version, so user projects start their own version history.
- `/_api/signal/{event,view,user,vital}` short-circuit requests carrying `DNT: 1` or `Sec-GPC: 1`. `/_api/signal/report` still accepts reports (crash visibility from opted-out browsers) but drops persistent identifiers. Client SDKs disable themselves automatically when the browser sets DNT/GPC.
- Client SDKs now flush on both `visibilitychange` and `pagehide` (Safari sometimes only fires one) and drain the offline queue on reconnect.
- Generated `reactive.svelte.ts` wires subscription lifecycle to Svelte `$effect` so queries unsubscribe on component destruction without manual cleanup.
- Dioxus bumped to 0.7.5; CI workflows install `dioxus-cli@0.7.5`.
- CI split into a reusable `template-smoke.yml` workflow: PRs run a smoke subset (`with-svelte/demo` + `with-dioxus/demo`) plus a workspace integration job, main-branch pushes run the full 6-template matrix. New `/test-template` and `/squash-merge` chatops commands.

### Fixed

- Mutation transactions could panic on commit because a lingering `Arc<Transaction>` clone in the context prevented `try_unwrap` from succeeding. The context is now dropped before commit/rollback on both success and error paths.
- RPC calls from tokens whose user was deleted now return 401 instead of executing against a phantom identity; non-public functions verify the user still exists before dispatching.
- `start_workflow()` inside a transactional mutation now resolves the active version and signature at call time, so "no active version" errors surface immediately instead of after commit. `PendingWorkflow` carries `workflow_version` and `workflow_signature`; `forge_workflow_runs` inserts include both.
- Startup now rejects configurations where `gateway.max_file_size > gateway.max_body_size` with a clear error instead of silently accepting an impossible combination.
- OTLP endpoint configuration is reliable: `otlp_endpoint = "${FORGE_OTEL_ENDPOINT-http://localhost:4318}"` in forge.toml uses the generic env-var substitution instead of the previous bespoke override path.

## [0.8.4] - 2026-04-11

### Added

- Fire-and-forget mutation helpers (`mutate`, `mutateWith`) with global error routing via `onMutationError` callback in both Svelte and Dioxus clients.
- `anonymize_ip` option in `[signals]` config for GDPR-compliant IP anonymization before visitor ID hashing.
- Per-mutation upload size limits via `max_upload_size` attribute.
- `DbConn` type exposed for direct database access in test contexts.
- `TestMcpToolContext` builder for unit testing MCP tool handlers.
- Performance benchmarking guide in documentation.

### Changed

- Template dependency versions derived from `CARGO_PKG_VERSION` at build time, keeping scaffolded projects in sync automatically.
- Benchmark loadgen rewritten with sharded per-thread metrics, configurable warmup phase, and structured JSON output.
- Test suite trimmed: low-value tests replaced with targeted coverage for security-sensitive paths and edge cases.

### Fixed

- SSE automatically reconnects after token refresh in both Svelte and Dioxus clients, fixing stale subscriptions after silent token rotation.
- Auth errors from subscription registration now propagate to `onAuthError` callback instead of silently retrying with an expired token.
- TOCTOU race conditions in OAuth token exchange and job claim paths where concurrent requests could bypass validation.
- Token binding bypass where a rotated refresh token could be replayed from a different session.
- Input validation gaps in webhook signature verification and signals endpoints.

## [0.8.3] - 2026-04-01

### Added

- Configurable global and per-mutation request body size limits (`max_body_size` in forge.toml and per-function attribute).
- `.env.example` files for all example projects so fresh clones have visible environment setup.

### Changed

- Query scope enforcement (`user_id`/`owner_id` filtering for private queries) moved from runtime checks to compile-time SQL analysis via `sql_extractor`. Invalid scoping now fails at `cargo build` instead of at request time.
- Dioxus frontend dependencies updated to published `forge-dioxus` crate versions, removing path dependency overrides.
- Redundant auth checks removed from benchmark suite.

### Fixed

- `max_body_size` config no longer leaks into JSON RPC endpoints. Multipart size limits are now correctly scoped to upload routes only, restoring HTTP-layer safety for standard RPC calls.

## [0.8.2] - 2026-03-29

### Added

- Product analytics and diagnostics system (`[signals]` in forge.toml): auto-captures all RPC calls, page views, custom events, error reports, and breadcrumb trails with zero configuration. GDPR-compliant visitor tracking via daily-rotating SHA256(IP+UA+salt), bot detection, session management, and Grafana dashboards over PostgreSQL datasource.
- `ForgeSignals` client API for Svelte and Dioxus with event batching, flushing, and page view auto-tracking.
- Correlation IDs (`x-correlation-id`) linking frontend events to backend RPC calls.
- Versioned workflows with signature guards: cryptographic contract signing via FNV-1a hash of persisted shape (name, version, step/wait keys, timeout, types). Mismatched runs block at resume with `BlockedSignatureMismatch`/`BlockedMissingVersion` status instead of silently corrupting.
- Operator controls for blocked workflows: `cancel_by_operator` and `retire_unresumable` terminal actions.
- `/_api/ready` reports unhealthy when blocked workflow runs exist.
- `HOST` and `PORT` environment variables override config at runtime.
- `FORGE_OTEL_TRACES`, `FORGE_OTEL_METRICS`, `FORGE_OTEL_LOGS` for per-signal observability toggle without config file changes.
- Cluster node discovery and improved multi-node coordination.
- MCP SSE streaming for Model Context Protocol tool calls.
- Startup banner on server init.

### Fixed

- SvelteKit example `build.rs` files now track `frontend/.env` for rebuild, fixing `forge test` failures where stale `PUBLIC_API_URL` was embedded after env patching.
- Normalized Playwright `ACTION_TIMEOUT` across all examples to 5s local / 15s CI; job/workflow tests use dedicated 15s timeout.

### Changed

- All internal `sqlx` queries migrated to compile-time checked `sqlx::query!`/`sqlx::query_as!` macros with inline parameters. Runtime dynamic queries removed.
- Security hardened: JWT claims sanitized before trust, state machine transitions validated, RPC input size/rate DoS limits enforced.
- Test infrastructure refactored for improved flexibility across Dioxus and Svelte Playwright configurations.

### Removed

- All legacy compatibility code: deprecated context decorators, old client generation path, obsolete config fields, and unused example functions removed as part of zero tech debt policy.
## [0.7.4] - 2026-03-26

### Added

- OAuth 2.1 authorization server with PKCE support
- Router and layout systems for all frontend templates
- `cargo install forgex` documented as alternative installation method

### Changed

- OAuth implementation refactored with improved type generation and built-in types
- Examples switched to workspace/path dependencies with version rewriting deferred to archive time
- `forge dev` command removed in favor of `docker compose` directly
- Frontend API URLs updated to use port 9081 across all examples and test configurations

### Fixed

- sqlx cache correctly copied into crate directories for publish
- Publish step fails on real errors instead of silently continuing

## [0.7.3] - 2026-03-25

### Added

- HTTP transport for MCP tool access alongside existing SSE/streamable transport
- JWT authentication with refresh token rotation, auto-registration, and embedded frontend auth provider
- Demo components for auth, stats, MCP tools, and live data across both Dioxus and Svelte frontends
- Comprehensive e2e test suite for demo project covering all feature sections with isolated test data

### Changed

- Default backend port changed from 8080 to 9081 to avoid conflicts with common dev servers
- Default frontend port standardized to 9080 across all templates and configurations
- CORS origins now include both `localhost` and `127.0.0.1` variants by default
- Removed `kanban-board` and `support-desk-with-mcp` example projects (functionality consolidated into demo templates)

### Fixed

- Template scaffolding hardened for standalone project builds with correct dependency versions
- CI auto-format step now runs before `forge check` to prevent generated code lint failures
- Dioxus frontend dependency resolution and webhook test timeouts

## [0.7.2] - 2026-03-20

### Added

- `ForgeDb` executor wrapper providing automatic `db.query` tracing spans on all database operations
- Benchmark suite with RPC latency, realtime propagation, and subscription scaling measurements
- Load generator (`loadgen`) for simulating concurrent users with SSE connections and RPC workloads
- Dioxus codegen: query-first API with `Mutation` struct and builder DTOs for cleaner frontend bindings
- Environment configuration files (`.env`) committed for all examples to simplify local development

### Changed

- Codegen internals refactored into unified `binding` and `emit` modules shared across Svelte and Dioxus generators
- Dioxus and Svelte runtime packages updated with improved realtime messaging and client libraries
- CI test isolation improved for Dioxus WASM targets with timer fixes
- Documentation refined across build guides, configuration, and skill references

### Fixed

- Runtime wiring for cluster heartbeat, gateway request handling, and realtime subsystem initialization
- Clippy `indexing_slicing` warnings in `ForgeDb` SQL operation detection
- Loadgen `while_let_loop` and argument count lint issues

## [0.7.1] - 2026-03-14

### Added

- Template catalog system (`.forge-template.toml`) with bundled project templates replacing dynamic scaffolding
- Non-interactive skill install support for CI environments
- Dioxus frontend lockfile generation for reproducible builds

### Changed

- `forge new` uses bundled template catalogs instead of dynamic file-by-file scaffolding
- Examples reorganized by frontend framework (`with-svelte/`, `with-dioxus/`) with minimal, demo, and feature-specific variants
- Dioxus frontend development moved to native builds outside Docker
- Release workflow refactored into reusable CI scripts (`scripts/ci/`)
- Crate publish made idempotent with dirty check fixes for forge-dioxus

### Fixed

- Clippy warnings (`collapsible_if`, `needless_borrows`, `explicit_auto_deref`) across crates
- CI template builds using unchecked sqlx macros to avoid requiring database at compile time
- Dioxus test suite gracefully skipped when `dx` CLI is unavailable

## [0.7.0] - 2026-03-12

### Added

- Dioxus frontend support with template-driven project creation, codegen, and runtime client (`forge new --template with-dioxus/demo`)
- `forge test` command wrapping Playwright with prerequisite checks, `--ui` and `--headed` flags
- `forge prepare` command for sqlx compile-time query checking with offline cache support
- Published `@forge-rs/svelte` npm package and `forge-dioxus` crate as standalone runtime packages

### Changed

- Frontend runtimes extracted from embedded CLI templates into published packages (`@forge-rs/svelte`, `forge-dioxus`)
- `forge generate` no longer writes runtime files to `.forge/`; projects depend on published packages instead
- Runtime config and docker-compose template defaults simplified
- Playwright test suites run sequentially by default for reliability
- Example docker-compose switched from named volumes to bind mounts for host LSP visibility
- RPC error handling improved in test fixtures

### Removed

- Embedded frontend runtime templates (`.forge/svelte/`, `.forge/dioxus/`); replaced by published packages

## [0.6.0] - 2026-03-09

### Added

- `ctx.issue_token()` on all context types for generating HMAC-signed JWTs without external auth providers
- Generated file checksums (`.forge/checksums.json`) to detect manual modifications to forge-managed frontend files
- Per-layer trace filtering for fine-grained observability control per tracing target
- PostgreSQL `application_name` connection parameter for identifying forge connections in database monitoring tools
- `has_input_args` flag on `FunctionInfo` to distinguish functions that accept user input from context-only functions

### Changed

- Identity scope enforcement skipped for functions with no input parameters (only `ctx`), removing the need for dummy input structs
- `forge check` recognizes standard `#[derive(Serialize, FromRow)]` patterns alongside `#[forge::model]`
- Observability log levels upgraded: RPC request logs demoted to debug, function args demoted to debug, removed redundant success field
- Forge-idiomatic-engineer skill reference docs consolidated from 12 files into 8 topic-focused references
- Fluent builder registration methods (`register_query()`, etc.) now used in scaffolded `main.rs` templates

### Fixed

- 53 documentation discrepancies found via comprehensive code-to-docs audit across all doc pages

## [0.5.1] - 2026-03-07

### Added

- `${VAR-default}` and `${VAR:-default}` syntax in config env var substitution for fallback values when variables are unset
- Per-function metrics: `fn.executions_total` counter and `fn.duration_seconds` histogram with function name, kind, and status labels
- `db.query` tracing spans on `DbConn` methods (`fetch_one`, `fetch_all`, `fetch_optional`, `execute`) so database calls appear in traces
- `db.transaction` tracing span around transactional mutation lifecycle (BEGIN, handler, COMMIT)
- SSE connection tracking via `active_connections` gauge (increment on connect, decrement on disconnect)
- Per-signal env var control for observability: `FORGE_OTEL_TRACES`, `FORGE_OTEL_METRICS`, `FORGE_OTEL_LOGS`

### Changed

- OTLP telemetry export disabled by default; enabled via `FORGE_OTEL_ENABLED=true` env var (docker compose sets this automatically)
- RPC request log demoted to debug level since `fn.execute` already logs at info with richer context
- Function input args demoted from info to debug level to reduce log noise and avoid PII exposure
- Removed redundant `success` field from function execution logs (message already distinguishes executed vs failed)
- Config templates use env var defaults (`${FORGE_OTEL_ENABLED-false}`) instead of hardcoded `enabled = true`

## [0.5.0] - 2026-03-06

### Added

- Observability instrumentation: tracing spans on RPC handlers, job workers, and cron ticks with structured fields; Prometheus-style metrics for request count, latency, and queue depth; slow query logging with configurable threshold; startup summary banner
- Consistent query routing via `#[forge::query(consistent)]` attribute to force reads from primary, bypassing replicas for read-after-write consistency
- Health-aware replica selection: background monitor pings replicas every 15s, automatically skips unhealthy replicas and falls back to primary
- Workload-isolated connection pools (`pools.default`, `pools.jobs`, `pools.observability`, `pools.analytics`) with independent size and timeout configuration
- Coalesced real-time subscriptions: identical query subscriptions share a single re-execution instead of running per-client
- Hybrid rate limiting combining in-memory token bucket with PostgreSQL-backed sliding window for cluster-wide consistency
- Cluster-aware cache invalidation via `forge_invalidations` table so nodes only re-execute queries affected by changes on other nodes
- `forge-idiomatic-engineer` Claude Code skill shipped with scaffolded projects for AI-assisted development

### Changed

- Removed 20 direct dependencies by reimplementing minimal usages inline (async-stream, hex, regex, regex-lite, walkdir, darling, dialoguer, indicatif, hostname, arc-swap, sysinfo, slab, smallvec, once_cell, futures, axum-extra, tonic, prost, hyper, and gRPC features from opentelemetry-otlp)
- Switched OTLP telemetry transport from gRPC (port 4317) to HTTP (port 4318), eliminating duplicate transitive dependency trees (axum 0.7, tower 0.4, matchit 0.7)
- Dropped bundled `AGENTS.md` from project templates in favor of the installed skill

## [0.4.1] - 2026-02-28

### Added

- `db_conn()` method on all context types for shared helper functions across queries, mutations, jobs, webhooks, crons, MCP tools, and daemons
- Daemon contexts can now dispatch jobs and start workflows via `dispatch_job()` and `start_workflow()`
- Testcontainers support: `from_env()` auto-starts a PostgreSQL container when `TEST_DATABASE_URL` is unset and the `testcontainers` feature is enabled
- `IsolatedTestDb::setup()` convenience method combining env detection, isolation, internal SQL, and migrations in one call
- Builder API: `register_query()`, `register_mutation()`, `register_job()`, `register_cron()`, `register_workflow()`, `register_daemon()`, `register_webhook()`, `register_mcp_tool()` methods on `ForgeBuilder`
- Webhook and MCP documentation improvements

### Changed

- Simplified example `main.rs` files to use new builder registration methods
- Test database helpers cleaned up with less verbose error formatting

## [0.4.0] - 2026-02-22

### Added

- Custom HTTP handler support via `#[forge::handler]` for raw request/response control
- Prebuilt Svelte runtime shipped with the CLI (no more regenerating on every codegen)
- Configurable log level with auto-initialized tracing subscriber
- Kanban board example (renamed from trellix) with redesigned UI
- Comprehensive Playwright test suites for all examples
- MCP and custom handler documentation
- Examples README with overview of all example projects

### Changed

- `forge dev` is now docker-only, removed embedded PostgreSQL support
- Examples moved into workspace with shared workspace dependencies
- Release pipeline overhauled for docker-only dev workflow
- Example UIs redesigned, replaced JS dialogs with inline UI
- Auth middleware fixes for edge cases
- AGENTS template updated for new context methods
- Docs updated to reflect docker-only dev and new context API

### Removed

- Embedded PostgreSQL support (use Docker Compose instead)
- Standalone `Cargo.lock` files from examples (now workspace members)

## [0.3.0] - 2026-02-20

### Added

- MCP server support with `#[mcp_tool]` macro for exposing functions as MCP tools
- MCP tool registry with JSON-RPC transport over stdio and SSE
- MCP configuration in `forge.toml` (`[mcp]` section)
- MCP security documentation
- Support-desk example project demonstrating MCP integration
- Enum variant description support in `#[forge_enum]` macro via `#[description]` attribute

### Changed

- Codegen parser extracts MCP tool metadata alongside API types

## [0.2.1] - 2026-02-09

### Added

- Example project (todo) e2e testing in CI release pipeline
- Playwright test suite for the todo example

### Changed

- Reactor invalidation uses periodic flush interval instead of inline check per change
- Todo example updated to `ctx.db()` transaction-aware query API
- Docker Compose template simplified: shorter cargo-watch command, PG 18 volume path fix

### Fixed

- `forge dev` crash when `.env` file doesn't exist (cargo-watch canonicalize error)
- `forge dev` now copies `frontend/.env.example` to `frontend/.env` when missing (fixes fresh clones)

### Removed

- Stale dashboard references from templates, docs, and config

## [0.2.0] - 2026-02-07

### Added

- OTLP-based observability with tracing, metrics, and database instrumentation
- Principal ownership tracking for jobs and workflows
- Job heartbeats for stale job detection
- Configuration validation at parse time for database, cluster, and auth settings
- Error reference page and contexts reference in documentation
- Stricter clippy lints across all crates

### Changed

- `forge dev` revamped with strict-by-default ports, takeover mode, and scoped reloads
- Gateway defaults hardened for production readiness
- Logging levels reconfigured for cleaner defaults
- Macro utilities extracted into shared `forge-macros/utils.rs`
- Documentation rewritten for conciseness and consistent tone

### Fixed

- Webhook idempotency race condition in concurrent request handling
- Advisory lock session pinning for leader election reliability
- Integration tests using local workspace path instead of published crates
- Bare `unwrap()` calls replaced with `expect()` for better panic diagnostics

## [0.1.0] - 2026-02-04

### Added

- Webhook support with signatures and macro generation
- Daemon support for long-running background processes
- Job cancellation with save/saved API and TTL cleanup
- Circuit Breaker pattern for HTTP client
- Multipart file uploads with unified duration parsing
- Read replica routing
- Upload type handling in TypeScript code generation
- Mutation transaction wrapping with outbox pattern
- Playwright e2e tests for project templates
- Full-stack todo example application

### Changed

- API routes now use consistent `/_api` prefix

### Removed

- Observability system and dashboard

## [0.0.7] - 2026-01-30

### Added

- Built-in JWT auth store generation for Svelte with localStorage persistence
- Svelte 5 runes-native reactive query bindings with automatic subscription management

### Changed

- Authentication required by default, removed `allow_anonymous` config option
- Job macro validates `priority` and `backoff` attributes at compile time
- Mutation macro enforces `transactional` attribute when dispatching jobs or workflows

### Fixed

- Workflow macro validation with better error messages for `tokio::sleep()` usage

## [0.0.6] - 2026-01-29

### Added

- Inline syntax for macro attributes (e.g., `#[forge::cron("0 9 * * *", timezone = "America/New_York")]`)

### Changed

- Authentication required by default for queries, mutations, and jobs
- Null arguments normalized to empty object for proper struct deserialization

### Fixed

- Null args handling in function, job, and workflow registries

## [0.0.5] - 2026-01-24

### Added

- Token change detection for automatic SSE reconnection
- Async JWT validation with reconnection handling
- JWKS caching and external RSA provider support (Firebase, Auth0, Clerk, Supabase)

### Changed

- Auth config moved to top level in `forge.toml`
- JWT field naming prefixed (`algorithm` → `jwt_algorithm`, etc.)
- Frontend env vars renamed to `PUBLIC_API_URL` following SvelteKit conventions

### Fixed

- Docker PostgreSQL volume path corrected
- Cargo watch polling in containerized environments
- TypeScript type checking in project template

## [0.0.4] - 2026-01-20

### Added

- Datetime types: `Instant`, `LocalDate`, `LocalTime` for type-safe date/time handling
- File upload type with multipart form data support
- Auth attributes for jobs and workflows (`#[public]`, `#[require_role]`)
- Server-Sent Events (SSE) gateway for real-time communication

### Changed

- Replaced WebSocket gateway with SSE for simpler deployment
- TypeScript codegen refactored with improved type inference

### Fixed

- TypeScript codegen for single-argument functions

## [0.0.3] - 2026-01-18

### Added

- WebSocket authentication with JWT support
- RS256/JWKS asymmetric algorithm support
- Role-based access control with `require_role` attribute
- Client IP and user agent in request metadata
- Flexible JWT subject handling for non-UUID values

### Changed

- Consolidated `#[forge::action]` into `#[forge::mutation]`
- System migrations use version-based naming (v001, v002, etc.)
- Added cargo-watch hot reload to `forge dev`

### Fixed

- Authentication verification order in router
- ESLint configuration in TypeScript scaffolding

## [0.0.2] - 2026-01-11

### Added

- Per-function logging with configurable levels (trace, debug, info, warn, error, off)
- Bare metal development mode without Docker dependency
- DTO struct parsing in TypeScript codegen

### Changed

- `forge dev` runs natively by default, Docker Compose via `--docker` flag
- Dockerfile template optimized with frontend build before embedding

### Fixed

- Dockerfile build order for frontend embedding

## [0.0.1] - 2026-01-09

### Added

- Full-stack framework compiling backend into single binary with PostgreSQL
- Query and mutation system with `/rpc/` endpoints and automatic caching
- Background job queue with retry logic and exponential backoff
- Cron scheduler with timezone support and leader election
- Durable workflows with compensation logic and version tracking
- Real-time subscriptions via PostgreSQL LISTEN/NOTIFY
- Type-safe environment variable access for all function contexts
- Built-in observability dashboard with metrics, logs, and traces
- TypeScript code generation from Rust models
- CLI tool for scaffolding and codegen (`forge new`, `forge codegen`)
- Svelte 5 frontend runtime library
- Automated testing framework with TestContext

### Changed

- Library renamed from `forgex` to `forge` for cleaner imports

### Fixed

- Rust 2024 edition unsafe block compatibility
- Release workflow cargo-edit installation

[unreleased]: https://github.com/isala404/forge/compare/v0.9.0...HEAD
[0.9.0]: https://github.com/isala404/forge/compare/v0.8.4...v0.9.0
[0.8.4]: https://github.com/isala404/forge/compare/v0.8.3...v0.8.4
[0.8.3]: https://github.com/isala404/forge/compare/v0.8.2...v0.8.3
[0.8.2]: https://github.com/isala404/forge/compare/v0.7.4...v0.8.2
[0.7.4]: https://github.com/isala404/forge/compare/v0.7.3...v0.7.4
[0.7.3]: https://github.com/isala404/forge/compare/v0.7.2...v0.7.3
[0.7.2]: https://github.com/isala404/forge/compare/v0.7.1...v0.7.2
[0.7.1]: https://github.com/isala404/forge/compare/v0.7.0...v0.7.1
[0.7.0]: https://github.com/isala404/forge/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/isala404/forge/compare/v0.5.1...v0.6.0
[0.5.1]: https://github.com/isala404/forge/compare/v0.5.0...v0.5.1
[0.5.0]: https://github.com/isala404/forge/compare/v0.4.1...v0.5.0
[0.4.1]: https://github.com/isala404/forge/compare/v0.4.0...v0.4.1
[0.4.0]: https://github.com/isala404/forge/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/isala404/forge/compare/v0.2.1...v0.3.0
[0.2.1]: https://github.com/isala404/forge/compare/v0.2.0...v0.2.1
[0.2.0]: https://github.com/isala404/forge/compare/v0.1.0...v0.2.0
[0.1.0]: https://github.com/isala404/forge/compare/v0.0.7...v0.1.0
[0.0.7]: https://github.com/isala404/forge/compare/v0.0.6...v0.0.7
[0.0.6]: https://github.com/isala404/forge/compare/v0.0.5...v0.0.6
[0.0.5]: https://github.com/isala404/forge/compare/v0.0.4...v0.0.5
[0.0.4]: https://github.com/isala404/forge/compare/v0.0.3...v0.0.4
[0.0.3]: https://github.com/isala404/forge/compare/v0.0.2...v0.0.3
[0.0.2]: https://github.com/isala404/forge/compare/v0.0.1...v0.0.2
[0.0.1]: https://github.com/isala404/forge/releases/tag/v0.0.1

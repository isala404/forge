# PLAN: Critical SaaS Features for Release

Overall Goal: Implement the most critical features that 90% of SaaS applications need but Forge currently lacks, fix clippy issues, and ensure scaffolded projects pass all linting.

---

## Analysis Summary

Based on exploration of 12 areas via sub-agents, these are the features that need implementation for a production-ready SaaS framework targeting ~100k MAU:

### Already Implemented (No Action Needed)
- `#[require_auth]` / `#[require_role]` - Security attributes work
- `#[encrypted]` field attribute - Works
- CORS configuration - Works
- Rate limiter infrastructure - Token bucket exists
- Query caching attribute parsing - Parsed but not enforced
- Adaptive tracking - Row/table mode switching works
- Parallel workflows - ctx.parallel() works
- Wait for events - ctx.wait_for_event() works
- Read replica routing - Works

### Critical Gaps for SaaS Release

| Priority | Feature | Impact | Effort |
|----------|---------|--------|--------|
| P0 | Rate limit attribute enforcement | Prevent abuse | Medium |
| P0 | Query cache runtime | Performance | Medium |
| P0 | Dockerfile + docker-compose | Deployment | Low |
| P0 | /ready endpoint | K8s/LB health | Low |
| P0 | GIN indexes on JSONB | Query performance | Low |
| P0 | Soft delete attribute | Data recovery | Medium |
| P1 | Connection status store | UX | Low |
| P1 | Store.reset() method | UX cleanup | Low |

---

## Step 1: Rate Limiting Attribute Enforcement

Goal: Make `#[rate_limit(requests = 100, per = "1m", key = "user")]` actually work on queries/mutations/actions

Files:
- `crates/forge-macros/src/query.rs` - Parse rate_limit attribute
- `crates/forge-macros/src/mutation.rs` - Parse rate_limit attribute
- `crates/forge-macros/src/action.rs` - Parse rate_limit attribute
- `crates/forge-runtime/src/function/router.rs` - Enforce rate limits before execution
- `crates/forge-core/src/function/traits.rs` - Add rate limit to FunctionInfo

Verify: Add rate limit to scaffolded template, run project, verify rate limits enforced

---

## Step 2: Query Cache Runtime

Goal: Actually cache query results when `#[cache = "30s"]` is specified

Files:
- `crates/forge-runtime/src/function/router.rs` - Add cache layer
- `crates/forge-runtime/src/function/cache.rs` - New file for in-memory cache
- `crates/forge-runtime/src/function/mod.rs` - Export cache module

Verify: Query with cache attribute returns cached result on second call

---

## Step 3: Soft Delete Attribute

Goal: `#[soft_delete]` on models adds `deleted_at` column and filters queries automatically

Files:
- `crates/forge-macros/src/model.rs` - Parse soft_delete attribute
- `crates/forge-core/src/schema/model.rs` - Handle soft_delete in TableDef
- `crates/forge-codegen/src/parser.rs` - Generate deleted_at column

Verify: Model with #[soft_delete] generates proper migration and filtering

---

## Step 4: Dockerfile and docker-compose.yml

Goal: Production-ready containerization

Files:
- `Dockerfile` - Multi-stage build for Rust binary
- `docker-compose.yml` - Forge + PostgreSQL setup
- `.dockerignore` - Exclude unnecessary files

Verify: `docker-compose up` starts working Forge instance

---

## Step 5: Readiness Endpoint

Goal: `/ready` endpoint for K8s/load balancer health checks

Files:
- `crates/forge-runtime/src/gateway/server.rs` - Add /ready route
- `crates/forge-runtime/src/gateway/health.rs` - New file for health checks

Verify: GET /ready returns 200 when DB connected, 503 otherwise

---

## Step 6: GIN Indexes on JSONB Columns

Goal: Add GIN indexes to all JSONB columns for query performance

Files:
- `crates/forge-runtime/migrations/0000_forge_internal.sql` - Add indexes

Verify: Run migration, verify indexes exist via psql

---

## Step 7: Connection Status Store (Frontend)

Goal: Reactive Svelte store for WebSocket connection state

Files:
- `crates/forge/templates/runtime/client.ts.tmpl` - Add connectionStatus store
- `crates/forge/templates/runtime/stores.ts.tmpl` - Export connection store
- `frontend/src/lib/client.ts` - Add connectionStatus store

Verify: Connection state changes reflected in UI

---

## Step 8: Store.reset() Method

Goal: Reset store to initial loading state

Files:
- `crates/forge/templates/runtime/stores.ts.tmpl` - Add reset() method
- `frontend/src/lib/stores.ts` - Add reset() to SubscriptionStore

Verify: Calling store.reset() clears data and sets loading

---

## Step 9: Fix Clippy Issues

Goal: Zero clippy warnings in entire codebase

Files:
- All .rs files with warnings

Verify: `cargo clippy --all-targets --all-features` returns no warnings

---

## Step 10: Validate Scaffolded Project

Goal: Generated project passes all linting

Commands:
- `./dev.sh setup`
- `cd test-project && cargo clippy`
- `cd test-project/frontend && bun run lint && bun run format --check`

Verify: All commands pass with zero errors

---

## Final Step: Cleanup & Validation

Goal: Run formatters and linters on entire codebase

Commands:
- `cargo fmt --all`
- `cargo clippy --all-targets --all-features`
- `cd frontend && bun run lint && bun run format`

Verify: All commands succeed without errors

---

*Plan created: 2026-01-02*

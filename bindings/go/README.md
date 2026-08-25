# Forge for Go

This directory contains Forge's native, pure-Go implementation:

```text
github.com/isala404/forge/bindings/go
```

The module implements Forge in pure Go with native pgx PostgreSQL access, Argon2id password hashing, native Go workers and subscriptions, and explicit process-local memory mode. It requires no cgo, Rust toolchain, C compiler, or shared library.

Run its database-free suite with:

```sh
go test ./...
go test -race ./...
```

Set `TEST_DATABASE_URL` to include PostgreSQL conformance, schema-drift, shared-client, and lifecycle tests. Memory mode is process-local and non-durable; it is rejected in production unless `AllowMemoryInProd` is explicitly set.

`InitDefault`, `InitFrom`, and `InitFromString` use the same strict runtime-profile keys and naming as the other packages. The normal profile requires PostgreSQL; `[forge] mode = "memory"` is the explicit database-free profile. Go does not download an embedded PostgreSQL server. Pass application-owned contexts to operations and `Close`; Forge installs no signal handlers.

For deterministic tests, construct a `ManualClock` and `SeededReader`, pass them through `NewMemoryForTesting`, and call `AdvanceTestClock` instead of sleeping. The seeded reader is deliberately not cryptographically secure.

Application-owned names can use `ScopeKVKey("billing", tenantID, userID, invoiceID)`. `ParseScopedName` reverses the v1 length-prefixed encoding. This is a naming aid only; authorize every component before calling Forge.

`ScheduleInspect`, `SchedulePause`, `ScheduleResume`, and `SchedulerDiagnostics` expose UTC scheduler state without a control plane. `ScheduleOptions` chooses `skip`, `run_once` (default), or `catch_up`; catch-up is capped at 100 and occurrence job IDs remain deterministic.

`EncodeCloudEvent`/`DecodeCloudEvent` and `ImportEnvConfig`/`ExportEnvConfig` are state-free interoperability helpers. They add no HTTP framework, protocol transport, or process-environment mutation to the module; the integration recipes cover Go `net/http`, deployment, MCP boundaries, and authenticated diagnostics.

Measured pool/server bulkheads use the shared `[databases.<primitive>]` tables. Each target receives the canonical migration history, while operations for that primitive route through its dedicated native pgx pool.

Deployment jobs call `forge.Migrate(ctx)` (or `MigrateFrom`/`MigrateFromString`) and require every structured report to have `State == "applied"`. Production configuration defaults `auto_migrate` off; `InitFrom` validates the schema without changing it. `MigrationStatus` reports lock ownership, and `ValidateSchema` checks migration history without locking.

The complete durable worker example is in [`examples/worker`](./examples/worker).

## OpenFeature, bulk reads, and snapshots

Package `openfeatureprovider` implements the official Go OpenFeature `FeatureProvider` over a Forge handle. It preserves typed values, variants, and standard reasons, registers no globals, and exposes the official OpenTelemetry trace hook for client-scoped registration. `ConfigGetMany` and `FlagDetailsMany` preserve input order and use one PostgreSQL query per operation. `forgeClient.ConfigSnapshot` captures only explicit keys and pre-evaluated flag requests; `forgeClient.EncodeConfigSnapshot`/`DecodeConfigSnapshot` enforce a 1 MiB portable form, expiry, unique identifiers, and an explicit `no_secrets` or `application_protected` declaration.

# Go worker example

Set `DATABASE_URL`, then run `go run .` from this directory. The example loads `forge.toml`, writes a durable key, registers a UTC cron with bounded catch-up, ticks the scheduler with lag diagnostics, runs a bounded native Go worker, handles structured errors, and closes Forge on `SIGINT` or `SIGTERM`.

For feature flags, package `openfeatureprovider` plugs this same handle into the official Go OpenFeature SDK and exposes the official OpenTelemetry hook without registering global state. `ConfigGetMany`/`FlagDetailsMany` avoid per-key startup queries; `ConfigSnapshot` is only for bounded disconnected reads with an explicit expiry and secret-handling declaration.

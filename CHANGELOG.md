# Changelog

All notable changes to this project are documented here. The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.1.0] - 2026-08-25

Forge 1.1 is the one declared compatibility reset from the unused 1.0 API. It freezes one contract across Rust, JavaScript, Python, and the new pure-Go implementation, centers production operation on PostgreSQL, and adds the explicit database-free memory test profile.

### Added

- Native Go, S3-compatible blob storage, reliable managed workers, transactional outbox delivery, live health probes, per-instance metrics, and W3C trace propagation.
- Deterministic memory test factories with manual time and seeded randomness in every language.
- Deployment-safe migration APIs, strict schema gates, and stable cross-language errors.

### Changed

- Reset the supported 1.x contract as documented in the 1.0-to-1.1 migration guide. Normal semantic-versioning discipline resumes with this release.
- Made PostgreSQL 18 the required durable production profile; memory is explicit, process-local, non-durable, and intended for tests and local development.

### Fixed

- Rechecked Node and Python managed-worker shutdown after long-poll dequeue and released a newly leased job instead of starting fresh work after a stop signal.

Full Changelog: [https://github.com/isala404/forge/compare/v1.0.1...v1.1.0](https://github.com/isala404/forge/compare/v1.0.1...v1.1.0)

## [1.0.1] - 2026-07-14

### Changed

- Modernized the Rust stack to SQLx 0.9, OpenTelemetry 0.32, `toml` 1.1, `hmac` 0.13, and `sha2` 0.11, including the required API migrations.
- Upgraded the Node native binding to napi-rs 3 and moved both binding crates to Rust 2024 edition.
- Refreshed the required SQLx offline metadata and example lockfiles for the updated core dependency graph.
- Added complete package metadata for crates.io, npm, and PyPI and reduced the crates.io source package to its intentional publish surface.
- Updated release defaults and the API compatibility baseline for the coordinated `v1.0.1` release.

### Fixed

- Rejected empty or whitespace-only blob signing secrets during configuration validation and before generating presigned URLs.
- Preserved safe dynamic migration and test SQL under SQLx 0.9's explicit `AssertSqlSafe` contract.
- Restored full-document Forge configuration parsing under `toml` 1.x.
- Hardened release packaging to use locked, reproducible installs and verified package builds instead of skipping Cargo's package verification.

Full Changelog: [https://github.com/isala404/forge/compare/v1.0.0...v1.0.1](https://github.com/isala404/forge/compare/v1.0.0...v1.0.1)

## [1.0.0] - 2026-07-10

Forge 1.0.0 is a complete rewrite of the earlier application framework.

Forge is now a library built around one Rust crate, `forgelib`. It provides the backend primitives most applications need through stable traits, with Node and Python bindings backed by the same Rust implementation.

The `1.x` API is now stable. Breaking API changes will be reserved for `2.0`.

### Added

- Backend primitives for key/value storage, queues, pub/sub, blobs, authentication, rate limiting, scheduling, and configuration with feature flags.
- PostgreSQL and in-memory backends for every primitive. Blob storage also supports the local filesystem.
- Embedded PostgreSQL 17 support through `[postgres] embedded = true`. Node and Python packages include this by default, so local development does not require a separate PostgreSQL installation.
- A shared `forge.toml` configuration file for Rust, Node, and Python.
- One-time authentication tokens for password resets, email verification, and magic links. Tokens are single-use, purpose-scoped, stored as hashes, and expire automatically.
- Managed queue workers for Node and Python. Workers heartbeat active leases, report lease loss, and recover from temporary dequeue failures.
- A cross-language conformance suite that checks behavior, data shapes, time units, defaults, and error codes across the Rust core and both bindings.
- Retryable error information in the Node and Python bindings.
- `ConfigKey.flag()` and `Topic.channel()` in Python.
- An `on_error` hook for Python queue workers.

### Changed

- Python now uses PyO3 and `pyo3-async-runtimes` 0.29.
- Python exception names now include the `Error` suffix, such as `NotFoundError`, `InvalidError`, and `BackendError`. `forge_error_code()` continues to return the original error code.
- PostgreSQL 17 is now the minimum supported version.
- The minimum supported Rust version is now 1.94.1.
- Prebuilt macOS packages are published for Apple silicon only.
- Node's `jsonCodec` now uses strict JSON encoding and decoding. Values written in Node behave the same when read from Python, and the reverse is also true.
- Rate-limit windows shorter than one second now return `INVALID`.
- PostgreSQL pub/sub is reported as non-durable because it uses `LISTEN` and `NOTIFY`.
- `maintain()` now runs every backend's cleanup task even if one backend fails. It returns the first error after the remaining tasks have run.

### Fixed

- `forgeErrorCode()` and `forgeErrorRetryable()` now read the error information produced by the Node binding correctly.

Full Changelog: [https://github.com/isala404/forge/compare/v0.10.2...v1.0.0](https://github.com/isala404/forge/compare/v0.10.2...v1.0.0)

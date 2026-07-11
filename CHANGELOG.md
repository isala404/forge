# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.0.0] - 2026-07-10

Forge 1.0.0 is a complete rewrite of the earlier application framework.

Forge is now a library built around one Rust crate, `forgelib`. It provides eight
infrastructure primitives through stable traits, with Node and Python bindings
backed by the same Rust implementation.

The `1.x` API is now stable. Breaking API changes will be reserved for `2.0`.

### Added

- Eight infrastructure primitives covering key/value storage, queues, pub/sub,
  blobs, authentication, rate limiting, scheduling, and configuration with
  feature flags.
- PostgreSQL and in-memory backends for every primitive. Blob storage also
  supports the local filesystem.
- Embedded PostgreSQL 17 support through `[postgres] embedded = true`. Node and
  Python packages include this by default, so local development does not require
  a separate PostgreSQL installation.
- A shared `forge.toml` configuration file for Rust, Node, and Python.
- One-time authentication tokens for password resets, email verification, and
  magic links. Tokens are single-use, purpose-scoped, stored as hashes, and
  expire automatically.
- Managed queue workers for Node and Python. Workers heartbeat active leases,
  report lease loss, and recover from temporary dequeue failures.
- A cross-language conformance suite that checks behavior, data shapes, time
  units, defaults, and error codes across the Rust core and both bindings.
- Retryable error information in the Node and Python bindings.
- `ConfigKey.flag()` and `Topic.channel()` in Python.
- An `on_error` hook for Python queue workers.

### Changed

- Python now uses PyO3 and `pyo3-async-runtimes` 0.29.
- Python exception names now include the `Error` suffix, such as
  `NotFoundError`, `InvalidError`, and `BackendError`. `forge_error_code()`
  continues to return the original error code.
- PostgreSQL 17 is now the minimum supported version.
- The minimum supported Rust version is now 1.94.
- Prebuilt macOS packages are published for Apple silicon only.
- Node's `jsonCodec` now uses strict JSON encoding and decoding. Values written
  in Node behave the same when read from Python, and the reverse is also true.
- Rate-limit windows shorter than one second now return `INVALID`.
- PostgreSQL pub/sub is reported as non-durable because it uses `LISTEN` and
  `NOTIFY`.
- `maintain()` now runs every backend's cleanup task even if one backend fails.
  It returns the first error after the remaining tasks have run.

### Fixed

- `forgeErrorCode()` and `forgeErrorRetryable()` now read the error information
  produced by the Node binding correctly.

### Package availability

This release is published as `forgelib 1.0.0` on npm and PyPI.

The `forgelib 1.0.0` version on crates.io belongs to an unrelated package. It is
yanked, but crates.io versions cannot be replaced. Rust users should depend on
the `v1.0.0` Git tag for this release.

Forge `1.0.1` will restore matching releases across crates.io, npm, and PyPI.

[1.0.0]: https://github.com/isala404/forge/compare/v0.0.1...v1.0.0

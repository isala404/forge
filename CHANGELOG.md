# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.0.0] - 2026-07-10

Complete rewrite from the earlier application framework into a library: one Rust
crate (`forgelib`) exposing eight infrastructure primitives behind stable traits.

**Distribution note:** crates.io already contains an unrelated, yanked, immutable
`forgelib 1.0.0`. This release publishes 1.0.0 to npm and PyPI and tags the matching
GitHub source, but does not publish or unyank a Rust crate. Rust users can depend on
the `v1.0.0` Git tag; version 1.0.1 will restore synchronized crates.io,
npm, and PyPI publication.

### Added

- One-time tokens on the auth primitive: `create_token(user_id, purpose, ttl)` /
  `consume_token(token, purpose)` (`createToken`/`consumeToken` in Node). Tokens
  are single-use, purpose-scoped, and stored hashed with a hard expiry, covering
  password reset, email verification, and magic links without an email service —
  your app sends the message, Forge mints and checks the token. Expired tokens
  are reclaimed by `maintain()`.
- Embedded Postgres: `[postgres] embedded = true` provisions a real PG 17 server
  on demand (binaries download once per machine, data persists in
  `.forge/pg`), behind the `embedded` cargo feature. The Node and Python
  packages ship with it enabled, so local dev needs no Postgres install. A
  non-empty `url` wins over the flag, so `url = "${VAR:-}"` + `embedded = true`
  deploys against `$VAR` and runs embedded otherwise; the example apps use
  exactly that. `postgres_url()` / `postgresUrl()` expose the resolved DSN for
  an app's own tables (Rust also has `forge.pool()`).
- Every raised Python exception carries a `retryable` attribute, and Node
  prefixes retryable backend errors as `BACKEND(retryable): ...`, so the core's
  per-error retryable flag survives the FFI boundary in both bindings
  (`forge_error_retryable` / `forgeErrorRetryable` report it).
- Eight primitives: kv, queue, pubsub, blob, auth, ratelimit, schedule, and
  config/flags, each with a Postgres backend and an in-memory backend for tests
  and local runs. Blob also has a filesystem backend.
- Node bindings (napi-rs) and Python bindings (PyO3) over the same Rust core, so
  all three languages share one implementation and one contract.
- Cross-language conformance suite: a declarative scenario matrix in
  `src/conformance/scenarios/` run by a native runner in each language, so the
  bindings cannot drift on units, shapes, defaults, or error codes.
- `forge.toml` as the single source of configuration.
- Node managed worker (`runWorker` / `forge.worker`) now auto-heartbeats the
  lease while the handler runs and survives transient dequeue errors with a
  backoff, matching the Python worker; `QueueJob.leaseLost` reports a lost lease.
- Python worker `on_error` hook (parity with Node's `onError`): dequeue errors,
  undecodable payloads, and handler/ack failures are reported instead of
  swallowed silently.
- Python `ConfigKey.flag()` and `Topic.channel()` (parity with Node).

### Changed

- The Python binding now uses PyO3 and `pyo3-async-runtimes` 0.29, removing the
  vulnerable 0.23 dependency line and adopting PyO3's current interpreter-attachment API.
- Python exceptions are named canonical code + `Error` (`NotFoundError`,
  `InvalidError`, `LimitError`, `PreconditionError`, `UnavailableError`,
  `ConfigError`, `BackendError`); the old bare names (`Limit`, `Config`,
  `Backend`, …) collided with common identifiers under
  `from forgelib import *`. `forge_error_code()` still returns the bare code.
- Supported Postgres floor raised from 14 to 17; init refuses older servers.
- Prebuilt packages for macOS are Apple-silicon only (`darwin-arm64`); the
  `darwin-x64` npm package and wheel are no longer published.
- MSRV raised from 1.92 to 1.94 (required by the embedded-Postgres dependency).
- Node `jsonCodec` is now strict JSON on both sides (`JSON.stringify` /
  `JSON.parse`), matching Python's `json.dumps`/`json.loads` defaults. A bare
  string is stored quoted; a payload one language writes decodes identically in
  the other. Previously strings were passed through unencoded and undecodable
  payloads fell back to the raw string, so `set("true")` came back as `true`.
- Rate limits shorter than 1 second are rejected as `INVALID` instead of being
  silently reinterpreted as 1 second (a stricter limit than requested).
- The backend report now lists Postgres pubsub as non-durable (LISTEN/NOTIFY
  stores nothing), agreeing with its own caveat text.
- `maintain()` runs every backend's sweep even when one fails, then surfaces the
  first error; previously one failing backend starved the rest of maintenance.

### Fixed

- Node `forgeErrorCode()` / `forgeErrorRetryable()` now parse the `CODE: message`
  prefix as documented. They previously read `error.code`/`error.retryable`,
  which napi never sets, so every error reported `GenericFailure`/not-retryable.

[1.0.0]: https://github.com/isala404/forge/compare/v0.0.1...v1.0.0

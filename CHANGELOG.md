# Changelog

All notable changes to this project are documented here. The format is based on
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/), and this project
adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

Complete rewrite from the earlier application framework into a library: one Rust
crate (`forgelib`) exposing eight infrastructure primitives behind stable traits.

### Added

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

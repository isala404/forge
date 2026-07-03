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
- Embedded Postgres: set `[postgres] url = "embedded"` to have Forge run its own
  native Postgres (downloaded once and cached, started on an auto-assigned free
  port, data persisted across restarts) so there is no external database to
  provision. Behind the crate's `embedded-postgres` feature; the Node and Python
  packages ship with it enabled. `FORGE_EMBEDDED_POSTGRES_DIR` points at a
  pre-installed Postgres for air-gapped or download-blocked hosts.

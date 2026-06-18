# Forge

The standard library for agent-built SaaS: one crate, one Postgres connection, and every backend primitive an app needs, hardened once and built on interfaces the industry already trusts.

AI agents build whole SaaS backends now, and each one re-implements the same plumbing: a key-value store, a job queue, scheduled tasks, file storage, rate limiting, config and flags, and auth. That regenerated plumbing is exactly where the subtle correctness and security bugs live. Forge implements these primitives once, behind a small frozen interface an agent targets and reuses everywhere.

Each primitive mirrors a design the industry (and every agent's training data) already knows, so generated code is correct on day one:

| Primitive        | Mirrors                               |
| ---------------- | ------------------------------------- |
| `kv`             | Redis                                 |
| `queue`          | AWS SQS                               |
| `schedule`       | cron + Unix `at`                      |
| `blob`           | AWS S3                                |
| `config` / flags | 12-factor + OpenFeature               |
| `ratelimit`      | token bucket + IETF RateLimit headers |
| `auth`           | OWASP + PHC                           |
| `pubsub`         | Postgres LISTEN/NOTIFY + Redis pub/sub |

Postgres backs every primitive by default, with zero extra infrastructure. Dedicated backends (Redis, S3, and so on) plug in later behind the same interface, without changing application code.

## Examples & bindings

One chat app, three interchangeable backends, lives under [`examples/chatapp/`](examples/chatapp/) — a **Rust/axum** backend ([`rust-be`](examples/chatapp/rust-be)), a **TypeScript/Node** backend (GraphQL Yoga, [`node-be`](examples/chatapp/node-be)), and a **Python/FastAPI** backend (Strawberry, [`python-be`](examples/chatapp/python-be)). The same **React** SPA ([`react-fe`](examples/chatapp/react-fe)) is built once per backend. Each backend is a full chat app on Forge + GraphQL that exercises **every** primitive: opaque multi-device sessions (`auth`), direct-to-storage media (`blob`), presence/typing/unread (`kv`), off-request fan-out (`queue`), abuse limits (`ratelimit`), send-later + disappearing messages (`schedule`), feature flags (`config`), and live GraphQL subscriptions over `pubsub` (Postgres LISTEN/NOTIFY). The Rust app uses the `forge` crate directly; the others go through the [`forge-node`](bindings/forge-node) and [`forge-py`](bindings/forge-py) bindings. One Playwright suite in [`examples/chatapp/react-fe/e2e/`](examples/chatapp/react-fe/e2e) runs against all three and proves they behave identically. Each primitive's semantic contract lives in [`docs/contracts/`](docs/contracts/).

**Status:** pre-v1, under active rewrite. The previous full-stack framework remains in this repository's git history. The public surface freezes at 1.0; until then it changes freely.

**Stability policy (from 1.0):** the primitive traits (`Kv`, `Queue`, `Blob`, …) are **sealed** — public to call, but implementable only inside this crate. This keeps each trait a one-way contract, so methods can be added on point releases without breaking downstream code; backends are added inside Forge (see `src/backend.rs`), and a deliberate, versioned provider SPI can come later. The semantics in [`docs/contracts/`](docs/contracts/) are normative; the shipped trait signatures are normative for shape.

MIT.

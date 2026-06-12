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

Three self-contained, individually-runnable chat apps live under [`examples/`](examples/) — **Rust/axum** ([`rs-chatapp`](examples/rs-chatapp)), **TypeScript/SvelteKit** ([`ts-chatapp`](examples/ts-chatapp)), and **Python/FastAPI** ([`py-chatapp`](examples/py-chatapp)). Each is a full chat backend on Forge + GraphQL that exercises **every** primitive: opaque multi-device sessions (`auth`), direct-to-storage media (`blob`), presence/typing/unread (`kv`), off-request fan-out (`queue`), abuse limits (`ratelimit`), send-later + disappearing messages (`schedule`), feature flags (`config`), and live GraphQL subscriptions over `pubsub` (Postgres LISTEN/NOTIFY). The Rust app uses the `forge` crate directly; the others go through the [`forge-node`](bindings/forge-node) and [`forge-py`](bindings/forge-py) bindings. One Playwright suite in [`e2e/`](e2e/) runs against all three and proves they behave identically. Each primitive's semantic contract lives in [`docs/contracts/`](docs/contracts/).

**Status:** pre-v1, under active rewrite. The previous full-stack framework remains in this repository's git history. The public surface freezes at 1.0; until then it changes freely.

MIT.

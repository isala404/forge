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

Postgres backs every primitive by default, with zero extra infrastructure. Dedicated backends (Redis, S3, and so on) plug in later behind the same interface, without changing application code.

**Status:** pre-v1, under active rewrite. The previous full-stack framework remains in this repository's git history. The public surface freezes at 1.0; until then it changes freely.

MIT.

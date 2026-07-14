---
name: forge-idiomatic-developer
description: Load this whenever you are about to write, edit, or review code that uses the forgelib library in any language.
---

# Building on Forge

Forge provides eight backend primitives with Postgres backends by default. Initialize
one client from `forge.toml` and use Forge's actual API rather than inferring behavior
from similar libraries.

Use the Forge version resolved by the project. Check its installed declarations when
an exact signature or behavior matters; when editing Forge itself, use the repository
source.

Read the relevant sections of the language reference:

- Node.js/TypeScript: [references/node.md](references/node.md)
- Python: [references/python.md](references/python.md)
- Rust: [references/rust.md](references/rust.md)

Read these only when they apply:

- [references/runtime-contract.md](references/runtime-contract.md) for configuration,
  errors, deployment, queues, scheduling, rate limits, pub/sub, presigning, flags, or
  maintenance.
- [references/application-design.md](references/application-design.md) for a
  nontrivial service, storage decision, authentication workflow, cross-system side
  effects, lifecycle, or test strategy.
- [references/nextjs.md](references/nextjs.md) for Next.js integration.

Do not preload unrelated language or framework references.

## Pick the primitive deliberately

| Need | Forge primitive | Core constraint |
| --- | --- | --- |
| Direct-key values, TTL state, counters, CAS | `kv` | Single-key operations; weakly consistent prefix scans |
| Durable background work and retry | `queue` | Requires an owning consumer; at-least-once |
| Ephemeral live fan-out | `pubsub` | At-most-once, live subscribers only |
| Files and presigned transfer URLs | `blob` | Application must enforce its ownership policy |
| Passwords, sessions, API keys, one-time tokens | `auth` | Application owns user and authorization records |
| Request throttling | `ratelimit` | Fail mode is a security/availability choice; not a business counter |
| Cron or delayed enqueue | `schedule` | Application must run scheduler ticks |
| Runtime settings and rollout flags | `config` | Cached; secrets remain environment-backed |

## Core contracts

- Queue delivery is concurrent and at-least-once. Settle with the delivery receipt or
  leased job, not its stable id. Ack only after the logical effect succeeds.
- Scope idempotency to the logical effect. A job id is a useful base for redelivery,
  but fan-out may require `job-id:recipient-id`, and duplicate jobs may require a
  stable domain operation id.
- Use managed workers where practical. Retain their promise/task/future, signal
  shutdown, and await completion.
- Run scheduler and maintenance loops when the selected features require them.
- Keep connection strings server-side and keep Forge's internal tables private.
- Presigning requires a non-empty secret without an empty fallback. Authorize access
  and verify the issued method, key, expiry, maximum bytes, and signature exactly.

## Before finishing

- Verify method names, sync/async use, option order, defaults, and error behavior
  against the project's installed Forge version.
- Confirm the chosen primitive supports the required behavior.
- Confirm every produced queue has an identified consumer in the deployed system;
  test redelivery at the logical side-effect boundary.
- Verify lifecycle behavior in the application's real process topology.
- Test the Forge boundary and the failure modes that matter to the change.

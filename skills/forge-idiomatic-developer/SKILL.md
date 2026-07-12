---
name: forge-idiomatic-developer
description: Load this whenever you are about to write, edit, or review code that uses the forgelib library in any language.
---

# Building on Forge

Forge is one library that gives an app eight backend primitives on a single Postgres
connection, with the same behavior in Rust, Node, and Python. Call `init()` once — it
reads `forge.toml` — and every primitive hangs off the returned client.

The API is small and consistent, but the exact names are not in any model's training
data. **Do not invent method names.** Use the tables in this skill, or read
`bindings/node/client.d.ts` and `bindings/python/src/lib.rs` in the repo — those
files are the contract.

## Pick the primitive first

Match the job to the primitive before writing anything.

| You need to… | Use | Never use it for |
| --- | --- | --- |
| Cache a value, store small app-owned records (user rows, indexes), count, dedup, compare-and-swap | **kv** | Large blobs; use blob |
| Run work in the background, retry on failure, delay, dead-letter | **queue** | Fire-and-forget notifications |
| Fan a live event out to connected clients (presence, typing, dashboards) | **pubsub** | Anything that must not be lost — it is at-most-once |
| Store and serve files, hand out presigned upload/download URLs | **blob** | Small hot values; use kv |
| Hash passwords, issue and validate sessions, mint API keys, one-time tokens (password reset, magic links, invites) | **auth** | Rolling your own crypto; sending the email itself |
| Throttle by key (login attempts, per-user API calls) | **ratelimit** | Durable counters; use kv |
| Run a cron, or enqueue one job at a future time | **schedule** | Immediate work; enqueue directly |
| Store runtime settings and evaluate feature flags with rollout | **config** | Secrets that belong in the environment |

Two rules that catch people: anything that must survive a disconnect or restart
belongs in queue/kv, with pub/sub only nudging live clients to reconcile from the
durable side (write the durable record first, publish the nudge second). And a queue
needs a worker — enqueuing does nothing until something dequeues, processes, and acks.

## Shape the data before writing handlers

Design the key/queue/topic layout first and write it down (a `STATE.md` naming every
key pattern and its owner). Derive it from **access paths, not entities**: every
endpoint/screen must map to one tenant-bounded prefix scan or a multi-get. A read
path with no owning prefix is a schema bug — add a secondary index at write time, or
it becomes a global scan in production.

Then pick each record's shape from how it is *written*, not how it is displayed:

| Write pattern | Shape |
| --- | --- |
| Many users each own a piece (votes, reactions, RSVPs) | One key per user, e.g. `vote:{tenant}:{item}:{user}` — the race disappears by design |
| Append-only shared list (comments, activity, audit) | One key per entry, never a JSON array |
| One shared value, several writers | Compare-and-swap with a bounded retry loop |
| One conceptual writer (a user's own profile) | Plain set |

Read-modify-write of a shared JSON record is the default *bug*, not the default
pattern — under concurrency it silently loses writes. Restructure keys so writes
cannot conflict; reach for CAS only when writers genuinely share one value. This
applies to request handlers exactly as much as to queue workers.

## The three bindings at a glance

Same primitives, three surface styles. The raw contract methods carry strings/bytes
1:1 across languages; Node and Python also expose native JSON handles on the main
client so app payloads are real objects.

**Rust** — namespaced accessors plus option-struct builders; `?` the `Result`s.

```rust
use std::time::Duration;
use forgelib::{Forge, SetOpts};

let forge = Forge::init().await?;                 // reads ./forge.toml
forge.kv().set("k", "v".into(), SetOpts::new().with_ttl(Duration::from_secs(60))).await?;
let n = forge.kv().incr("hits", 1).await?;
```

**Node** — flat camelCase methods, positional arguments, `null` to skip optional
trailing args. Everything is `async`.

```ts
import { ForgeClient } from "forgelib";

const forge = await ForgeClient.init();           // reads ./forge.toml
await forge.kvSet("k", "v", 60);                  // ttlSeconds
const n = await forge.kvIncr("hits", 1);
```

**Python** — flat snake_case methods, every one awaitable, optional args `None`.

```python
import forgelib

forge = await forgelib.ForgeClient.init()         # reads ./forge.toml
await forge.kv_set("k", "v", 60)                  # ttl_seconds
n = await forge.kv_incr("hits", 1)
```

Full per-language method tables and idioms:

- **[references/rust.md](references/rust.md)** — accessors, option builders, worker.
- **[references/node.md](references/node.md)** — raw `ForgeClient` methods + native JSON handles.
- **[references/python.md](references/python.md)** — raw client methods + native JSON handles.
- **[references/application-invariants.md](references/application-invariants.md)** —
  read whenever you build or review a whole service: multi-key consistency, idempotent
  workers, auth boundaries, scans and caps, shutdown, and validation.

Two idioms to reach for by default (full examples in the references):

- **Native JSON handles for app payloads.** Bind a codec once instead of stringifying
  at every call site: `forge.queue(name)` / `forge.kv(key)` / `forge.config(key, default)`
  / `forge.topic(name)` return typed handles in Node and Python; Rust re-exports typed
  handles from the crate root.
- **The managed worker instead of a hand-rolled dequeue loop.** Node
  `forge.worker(queue, handler, { signal })` (abort to drain), Python
  `forge.worker(queue, handler, stop=event)`, Rust `forge.worker(queue).run(handler)`.
  It dequeues, heartbeats at a third of the visibility window, acks on success, nacks
  on error, abandons on a lost lease. Keep and await the returned promise/task/future
  during shutdown — the stop signal begins the drain, not finishes it. If you must
  hand-roll, heartbeat before the lease expires or the job is redelivered mid-flight.

## forge.toml conventions

One file at the project root configures the whole runtime. `init()` reads it, applies
production-safe defaults, and migrates its own tables. An unknown key is a startup
error, not a silent typo.

```toml
[postgres]
# A set url wins; when it resolves empty, embedded = true downloads and runs a
# local PG 17 (data persists in .forge/pg) — no Postgres install needed.
url = "${DATABASE_URL:-}"
embedded = true

[backends]
default = "${FORGE_BACKEND:-postgres}"   # set FORGE_BACKEND=memory in tests

[blob]
signing_secret = "${FORGE_BLOB_SIGNING_SECRET:-}"   # required for presigned URLs
```

- `${VAR}` / `${VAR:-default}` interpolation runs on string values only. A `${VAR}`
  with no value and no default is a hard error, so a missing secret stops startup.
- `backends.default` is the memory-vs-postgres switch; both pass the same conformance
  suite. Even all-memory, `init()` still needs a reachable Postgres (or
  `embedded = true`) for Forge's own system database.
- Presigned blob URLs need `[blob].signing_secret`; CRUD works without it.
- `[forge].namespace` prefixes every key/queue/topic so apps can share one database.
  It must not contain a colon.

## Error taxonomy

Every failure maps to one canonical code, same set across languages.

| Code | Meaning | Retryable |
| --- | --- | --- |
| `NOT_FOUND` | The entity does not exist | No |
| `INVALID` | Caller bug: bad argument, malformed key, out-of-range option | No |
| `LIMIT` | A size/length/quota ceiling was exceeded | No |
| `PRECONDITION` | CAS mismatch, lost lease, unknown receipt — re-read state and decide | No |
| `UNAVAILABLE` | Transient backend outage (pool timeout, dropped connection) | **Yes** |
| `CONFIG` | Misconfiguration; only ever raised from `init()` | No |
| `BACKEND` | A backend error that is none of the above | Sometimes |

Node prefixes the code onto the thrown `Error`'s message (`"PRECONDITION: ..."`; a
retryable backend error reads `"BACKEND(retryable): ..."`) — parse with
`forgeErrorCode(err)` / `forgeErrorRetryable(err)`. Python raises typed exceptions
(`InvalidError`, `UnavailableError`, …, all subclassing `ForgeError`) carrying a
`retryable` attribute. Rust returns `Err(forgelib::ForgeError)`; match the variant or
call `.is_retryable()`.

Only `UNAVAILABLE` (and retryable `BACKEND`) is worth retrying. Wrap Forge errors at
your service boundary into your app's own codes (`DUPLICATE_EMAIL`, `THROTTLED`);
callers should never parse `"PRECONDITION: …"` strings.

## Pitfalls (verified, not folklore)

Each of these cost a real agent real time. Ordered by expense.

- **CAS: `old = null`/`None` means "expect absent", and nothing else matches a missing
  key.** Passing a default (like `[]` from `getOrDefault`) as `old` when the key
  doesn't exist yet fails forever — a create-or-update loop spins silently. Seed the
  key first (set with if-not-exists) or branch on a null get.
- **A duplicate `dedupId` is NOT an error.** Enqueue with a dedup id seen in the last
  5 minutes (configurable) silently returns the *existing* job id — SQS semantics,
  and the dedup outlives the ack. Compare returned ids if you need to detect it.
- **Rate limit is a token bucket that starts full**: "20 per 60s" allows 20
  immediately, then refills continuously — a sustained-rate shaper, not a hard
  per-window cap. `algo: "sliding_window"` tracks a hard cap closely but can still
  slightly overshoot at a window rollover; an exact cap needs your own kv counter.
  `remaining` hits 0 on the last *allowed* call. Limiter state lives in Postgres and
  persists across restarts and test runs.
- **`kvIncr` returns a JS `number` (f64) in Node**, so a counter past 2^53 loses
  precision (Python/Rust are exact ints). It auto-creates missing keys at 0; the
  stored value reads back as a decimal string via `kvGet`.
- **String getters are lossy UTF-8** (bytes decode with replacement). For binary use
  the byte variants: Node `kvGetBytes` / `kvSetBytes` / `blobGetBytes` /
  `blobPutBytes`; Python `kv_get_bytes` / `kv_set_bytes` (`blob_put` / `blob_get` are
  already bytes-native; `blob_put_object` when you also need metadata).
- **Queue receipts are opaque and process-local.** Settle (ack/nack/heartbeat) with
  the `receipt` (delivery-unique), never the `id` (stable across redeliveries — that
  is your idempotency key), and only from the client that leased it. Retries back off
  exponentially with jitter; after `maxAttempts` the job moves to `"<queue>.dlq"`.
  Delivery is at-least-once: every handler must tolerate the same job again.
- **pub/sub is at-most-once, live-subscribers-only.** Nothing published before
  `subscribe()` resolves is delivered (no replay). Never the system of record.
  Payloads publish as strings but arrive as bytes — decode before parsing.
- **The scheduler and maintenance sweep only run when you tick them.** Forge starts
  no threads: call `runSchedulerOnce` / `run_scheduler_once` and `maintain` on an
  interval (e.g. 30s); in Rust use the maintenance loop. No tick → crons never fire,
  expired rows never swept.
- **There is no client `close()`/shutdown** — the client cleans up at process exit;
  only pubsub subscriptions have `close()` (Node) / `aclose()` (Python), which also
  interrupt a pending `next()`. Still stop and await workers, subscriptions, tick
  loops, and the HTTP server before exiting.
- **Postgres state persists across test runs** (rows, rate-limit buckets, dedup ids).
  Make fixtures unique per run — suffix emails/IPs/slugs with a run id — or a green
  first run turns into a red second run.

## Write it clean

Forge apps are small. Keep them small.

- **Build the minimum that solves the stated problem.** No speculative features,
  flags, or wrappers around the client until a second caller shares real shape.
  Wrong abstractions calcify; duplication is fixable.
- **One way per concern, used everywhere.** When you write a helper — an
  authorization gate, a validation parser, a scan utility — migrate every call site
  to it; a helper half the routes bypass is worse than none. Authorization
  especially: one gate taking a minimum role, called from the resource loader, never
  hand-rolled comparisons per route.
- **Loaders fetch what their callers need, nothing more.** A request-context loader
  that scans a full roster for routes that never read it is a hidden N+1.
- **No dead code ships**: no commented-out blocks, unused imports (or `void x`
  suppression hacks), unreachable branches, or always-true predicates.
- **Comments explain why** — the invariant, the race being closed — never what the
  call does. A comment that restates the code gets deleted.
- **Idiomatic beats clever.** Match the host codebase's style; a fluent reader
  should find the code boring.

## Before you finish

Check the application, not just whether it compiles:

- Every method exists in the binding — grep `bindings/node/client.d.ts` or
  `bindings/python/src/lib.rs` (`tools/skill-check` guards this skill against them).
- Every enqueued queue has a running worker; handlers survive concurrent
  redelivery; shutdown signals, then awaits, the drain.
- Multi-key transitions document canonical record, write order, and compensation;
  deletes run in reverse creation order.
- Scans are tenant-bounded and paginated end to end; caps truncate and return a
  cursor — never throw — and the HTTP layer exposes the cursor.
- Credential rate limits fail closed, run before expensive work, and key on the
  socket address unless a trusted proxy is configured; no-op conditions are checked
  before consuming single-use tokens or limiter budget.
- Bodies are type-checked without coercion; cross-field invariants hold on every
  write path, updates included; money is integer minor units, grouped by currency.
- Durable state commits before any pub/sub nudge; subscribers reconcile from it.
- The state-model doc matches the store code; any contradiction is a bug in one.
- Tests use real forgelib (memory, plus Postgres where persistence or concurrency
  matters), run twice with unique fixtures. Pure computation (splits, balances, date
  math) gets direct unit tests — e2e won't catch a wrong number that renders fine.
  Web apps add a browser smoke test: signup/login, invalid payloads, tenant
  isolation, hard-reload session restore, queue completion, console/network errors.

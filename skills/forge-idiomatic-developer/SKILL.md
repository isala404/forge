---
name: forge-idiomatic-developer
description: >-
  Load this whenever you are about to write, edit, or review code that uses the
  forgelib library in any language — the Rust crate `forgelib`, the Node package
  `forgelib`, or the Python package `forgelib`. Forge's API is not in your training
  data, so guessing method names produces code that does not compile or run. This
  skill maps each task to the right primitive and gives the verified, idiomatic API
  for every binding. Triggers: a `forge.toml` in the tree; imports of `forgelib`,
  `ForgeClient`, or `Forge`; any request to add caching, a queue, pub/sub, blob
  storage, auth, rate limiting, scheduling, or feature flags to a Forge app.
---

# Building on Forge

Forge is one library that gives an app eight backend primitives on a single Postgres
connection, with the same behavior in Rust, Node, and Python. You call `init()` once,
which reads a `forge.toml`, and every primitive hangs off the returned client.

The API is small and consistent, but the exact names differ per language and are not
in any model's training data. **Do not invent method names.** Use the tables in this
skill, or read `bindings/node/client.d.ts` and `bindings/python/src/lib.rs` in the
repo — those files are the contract.

## Pick the primitive first

Match the job to the primitive before writing anything. Reaching for the wrong one
(pub/sub for durable work, a queue for a cache) is the most common mistake.

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
belongs in queue/kv, with pub/sub only nudging live clients to refresh (it is
at-most-once, no replay). And a queue needs a worker — enqueuing does nothing until
something dequeues, processes, and acks; use the managed worker helper (below).

## The three bindings at a glance

Same primitives, three surface styles. The raw contract methods carry strings/bytes
1:1 across languages; Node and Python also expose native JSON handles on the main
client so app payloads are real objects without a second import path.

**Rust** — namespaced accessors plus option-struct builders. Fallible calls return
`Result`, so `?` them.

```rust
use std::time::Duration;
use forgelib::{Forge, SetOpts};

let forge = Forge::init().await?;                 // reads ./forge.toml
forge.kv().set("k", "v".into(), SetOpts::new().with_ttl(Duration::from_secs(60))).await?;
let n = forge.kv().incr("hits", 1).await?;
```

**Node** — flat camelCase methods on the client, plain positional arguments, optional
trailing args passed as `null` to skip. Everything is `async`.

```ts
import { ForgeClient } from "forgelib";

const forge = await ForgeClient.init();           // reads ./forge.toml
await forge.kvSet("k", "v", 60);                  // ttlSeconds
const n = await forge.kvIncr("hits", 1);
```

**Python** — flat snake_case methods, every one awaitable, optional args default to
`None`.

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
  read this whenever you build or review a whole service: multi-key consistency,
  idempotent workers, auth boundaries, shutdown, scans, and end-to-end validation.

Two idioms to reach for by default (full examples live in the references above):

- **Native JSON handles for app payloads.** Bind a codec once instead of
  stringifying at every call site: `forge.queue(name)` / `forge.kv(key)` /
  `forge.config(key, default)` / `forge.topic(name)` return typed handles in Node and
  Python; Rust re-exports typed handles from the crate root.
- **The managed worker instead of a hand-rolled dequeue loop.** Node
  `forge.worker(queue, handler, { signal })` (abort to drain), Python
  `forge.worker(queue, handler, stop=event)`, Rust `forge.worker(queue).run(handler)`.
  It dequeues, heartbeats at a third of the visibility window, acks on success, nacks
  on a thrown error, and abandons the job if the lease is lost. Keep and await the
  returned promise/task/future during shutdown: sending the stop signal begins the
  drain; it does not mean the drain has finished. If you must hand-roll, heartbeat
  before the lease expires or the job is redelivered mid-flight.

## forge.toml conventions

One file at the project root configures the whole runtime. `init()` reads it, applies
production-safe defaults for anything omitted, and migrates its own tables. An unknown
key is a startup error, not a silent typo.

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

- **`${VAR}` / `${VAR:-default}` interpolation** runs on string values only (numbers
  and booleans stay literal). A `${VAR}` with no value and no default is a hard error,
  so a missing secret stops startup instead of resolving to `""`.
- **`backends.default` is the memory-vs-postgres switch.** Drive it from the
  environment (`${FORGE_BACKEND:-postgres}`) so the same file runs primitives on
  `memory` in tests and `postgres` in production. Both pass the same conformance
  suite, so behavior matches. Even all-memory, `init()` still needs a reachable
  Postgres (or `embedded = true`) for Forge's own system database.
- **Presigned blob URLs need `[blob].signing_secret`.** CRUD works without it; the
  presign methods fail without it.
- `[forge].namespace` prefixes every key/queue/topic so several apps can share one
  database. It must not contain a colon.

## Error taxonomy

Every failure maps to one canonical code. Same set across languages; the surface
differs.

| Code | Meaning | Retryable |
| --- | --- | --- |
| `NOT_FOUND` | The entity does not exist | No |
| `INVALID` | Caller bug: bad argument, malformed key, out-of-range option | No |
| `LIMIT` | A size/length/quota ceiling was exceeded | No |
| `PRECONDITION` | CAS mismatch, lost lease, unknown receipt — re-read state and decide | No |
| `UNAVAILABLE` | Transient backend outage (pool timeout, dropped connection) | **Yes** |
| `CONFIG` | Misconfiguration; only ever raised from `init()` | No |
| `BACKEND` | A backend error that is none of the above | Sometimes |

- **Node** prefixes the code onto the thrown `Error`'s message, e.g.
  `"PRECONDITION: ..."` (a retryable backend error reads `"BACKEND(retryable): ..."`).
  Parse it with `forgeErrorCode(err)` / test retryability with
  `forgeErrorRetryable(err)` from `forgelib`.
- **Python** raises a typed exception hierarchy named code + `Error`
  (`InvalidError`, `UnavailableError`, …, all subclassing `ForgeError`), each
  carrying a `retryable` attribute. Use `forge_error_code(exc)` /
  `forge_error_retryable(exc)` from `forgelib`.
- **Rust** returns `Err(forgelib::ForgeError)`; match the variant, or call
  `.is_retryable()`.

Only `UNAVAILABLE` (and a `BACKEND` error flagged retryable) is worth retrying.
Retrying an `INVALID` or `PRECONDITION` just fails again.

## Pitfalls (verified, not folklore)

Each of these cost a real agent real time. Ordered by expense.

- **CAS: `old = null`/`None` means "expect absent", and nothing else matches a
  missing key.** Passing a default (like `[]` from `getOrDefault`) as `old` when the
  key doesn't exist yet fails forever — a create-or-update loop then spins silently.
  Seed the key first (`set` with if-not-exists) or branch on `get() === null`.
- **A duplicate `dedupId` is NOT an error.** Enqueue with a dedup id that was seen in
  the last 5 minutes (configurable window) silently returns the *existing* job id —
  SQS semantics, and the dedup outlives the job being acked. Don't wait for a
  `PRECONDITION` that never comes; compare returned ids if you need to detect it.
- **Rate limit is a token bucket that starts full**: "20 per 60s" allows 20
  immediately, then refills continuously at 20/60 per second — a sustained-rate
  shaper, not a hard per-window cap. `algo: "sliding_window"` (weighted prior
  window, the standard approximation) tracks a hard cap much more closely but can
  still admit slightly over it at a window rollover; an exact "never more than N
  in any window" needs your own kv counter. `remaining` hits 0 on the last
  *allowed* call. Limiter state lives in Postgres: it persists across restarts
  and test re-runs.
- **`kvIncr` returns a JS `number` (f64) in Node**, so a counter past 2^53 loses
  precision (Python/Rust are exact ints). It auto-creates missing keys at 0; the
  stored value reads back as a decimal string via `kvGet`.
- **String getters are lossy UTF-8.** `kvGet` / `blobGet` (and Python `kv_get`) decode
  bytes with replacement. For binary use the byte variants: Node `kvGetBytes` /
  `kvSetBytes` / `blobGetBytes` / `blobPutBytes`; Python `kv_get_bytes` /
  `kv_set_bytes`, and Python `blob_put` / `blob_get` are already bytes-native (no
  `blob_*_bytes`; `blob_put_object` when you also need metadata).
- **Queue receipts are opaque and process-local in the bindings.** Settle
  (ack/nack/heartbeat) with the `receipt` (delivery-unique), never the `id` (stable
  across redeliveries — that is your idempotency key), and only from the client that
  leased it. Retries back off exponentially with jitter; after `maxAttempts` the job
  moves to `"<queue>.dlq"` (inspect it with `queueDepth`/`dequeue` on that name).
  Delivery is at-least-once: a process can finish the side effect and die before ack,
  so every handler must tolerate the same job again.
- **pub/sub is at-most-once, live-subscribers-only.** A publish after `subscribe()`
  resolves is delivered to that subscriber; anything published before it is gone (no
  replay). Never use it as the system of record. Payloads publish as strings but
  arrive as bytes (`Buffer`/`bytes`) — decode before parsing.
- **The scheduler and maintenance sweep only run when you tick them.** Forge starts no
  threads. From Node/Python, call `runSchedulerOnce` / `run_scheduler_once` and
  `maintain` on an interval (e.g. every 30s) — the first fires due crons/one-shots
  onto their queues, the second is housekeeping. In Rust use the maintenance loop.
  Without a tick, crons never fire and expired rows never get swept.
- **There is no client `close()`/shutdown.** The client cleans up at process exit;
  only pubsub subscriptions have `close()` (Node) / `aclose()` (Python), which also
  interrupt a pending `next()`. This does not make immediate process exit safe: stop
  and await workers, subscriptions, scheduler/maintenance loops, and the HTTP server.
- **Postgres state persists across test runs** (rows, rate-limit buckets, dedup
  ids). Make fixtures unique per run — suffix emails/IPs/slugs with a run id — or a
  green first run turns into a red second run.

## Writing good Forge code

Habits that separated the best clean-room implementations from the rest:

- **Design the key/queue/topic layout first and write it down** — a short comment
  block naming each key pattern and what owns it. Most rework traces to a bad
  layout, not a bad call.
- **Cheap guard before expensive work**: rate-limit before hashing or verifying a
  password. For uniqueness spanning a primary record and an index, choose an
  explicit write order and compensate every later failure; the two writes are not a
  transaction. Use app SQL when an unrecoverable partial state is unacceptable.
- **Model queue handlers as concurrent and repeatable.** Prefer one deterministic
  record per event/job over read-modify-write of a shared JSON list. Use the stable
  job id as an idempotency key, and use a transaction/outbox or a downstream
  idempotency key when the side effect cannot be one atomic Forge write.
- **Bound and paginate scans.** Partition keys by owner/tenant, give event-like data
  a retention policy, follow every cursor, and batch reads. A global scan in a request
  path is not a database query plan.
- **Wrap Forge errors at your service boundary** into your app's own error codes
  (`DUPLICATE_EMAIL`, `THROTTLED`). Callers should never parse `"PRECONDITION: …"`
  strings, and only `UNAVAILABLE`/retryable-`BACKEND` is worth retrying.
- **Don't mock forgelib.** Run tests against the real thing (`memory` backends, or a
  scratch Postgres) — the conformance-tested behavior is the point of the library.
- **Durable live-data pairing**: write the durable record (kv/queue) first, then
  publish the pub/sub nudge; readers reconcile from the durable side.
- Comments explain *why* (the invariant, the race being closed), not what the call
  does. No speculative wrappers around the client until a second caller shares
  real shape.

## Before you finish

Check the application, not just whether it compiles:

- Every method exists in the binding. If unsure, grep `bindings/node/client.d.ts`,
  `bindings/python/src/lib.rs`, `src/lib.rs`, or the per-language reference. The
  repo's `tools/skill-check` guard verifies names in this skill against those files.
- Every enqueued queue has a running worker; every handler is safe under concurrent
  delivery and redelivery, and shutdown signals then awaits its drain.
- Every multi-key state transition documents its canonical record, partial-failure
  recovery, and whether it actually needs an app SQL transaction.
- Every scan is paginated and bounded by tenant/owner and retention; bulk values use
  the binding's multi-get instead of serial reads.
- Security-sensitive rate limits fail closed, run before expensive auth work, and
  request bodies are type-checked rather than coerced.
- Durable state is committed before a pub/sub nudge, and subscribers reconcile from
  that durable state because live messages can be lost.
- Tests use real forgelib, run twice with unique fixtures, and cover the relevant
  failure modes against memory plus Postgres when persistence or concurrency matters.
- For a web app, exercise signup/login, invalid payload types, tenant isolation,
  session restoration after a hard reload, queue completion/redelivery, graceful
  shutdown, and browser console/network errors.

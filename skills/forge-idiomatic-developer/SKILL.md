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
| Cache a value, hold session-ish data, count, dedup, compare-and-swap | **kv** | Large blobs; use blob |
| Run work in the background, retry on failure, delay, dead-letter | **queue** | Fire-and-forget notifications |
| Fan a live event out to connected clients (presence, typing, dashboards) | **pubsub** | Anything that must not be lost — it is at-most-once |
| Store and serve files, hand out presigned upload/download URLs | **blob** | Small hot values; use kv |
| Hash passwords, issue and validate sessions, mint API keys | **auth** | Rolling your own crypto |
| Throttle by key (login attempts, per-user API calls) | **ratelimit** | Durable counters; use kv |
| Run a cron, or enqueue one job at a future time | **schedule** | Immediate work; enqueue directly |
| Store runtime settings and evaluate feature flags with rollout | **config** | Secrets that belong in the environment |

Two rules that catch people:

- **pub/sub is at-most-once and only reaches currently-connected subscribers.** If a
  message must survive a disconnect or a restart, it belongs in a queue (durable) or
  kv (the value of record). The idiomatic pattern is to write the durable state to
  kv/queue and use pub/sub only to nudge open clients to refresh.
- **A queue needs a worker.** Enqueuing does nothing on its own; something has to
  dequeue, process, and ack. Use the managed worker helper (below) rather than a
  hand-rolled loop, so heartbeating and acking are correct.

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

Two idioms to reach for by default (full examples live in the references above):

- **Native JSON handles for app payloads.** Bind a codec once instead of
  stringifying at every call site: `forge.queue(name)` / `forge.kv(key)` /
  `forge.config(key, default)` / `forge.topic(name)` return typed handles in Node and
  Python; Rust re-exports typed handles from the crate root.
- **The managed worker instead of a hand-rolled dequeue loop.** Node
  `forge.worker(queue, handler, { signal })` (abort to drain), Python
  `forge.worker(queue, handler, stop=event)`, Rust `forge.worker(queue).run(handler)`.
  It dequeues, heartbeats at a third of the visibility window, acks on success, nacks
  on a thrown error, and abandons the job if the lease is lost. If you must hand-roll,
  heartbeat before the lease expires or the job is redelivered mid-flight.

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
  environment (`${FORGE_BACKEND:-postgres}`) so the same file runs on `memory`
  in tests (in-process, no database) and `postgres` in production. Both pass the same
  conformance suite, so behavior matches.
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
| `PRECONDITION` | CAS mismatch, lost lease, duplicate dedup id — re-read state and decide | No |
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

- **`kvIncr` returns a JS `number` (f64) in Node**, so a counter past 2^53 loses
  precision. Real counters never get there; if yours might, read it back losslessly
  with `forge.kvGetBytes()`. Python's `kv_incr` returns an exact int, Rust's an `i64`.
- **String getters are lossy UTF-8.** `kvGet` / `blobGet` (and Python `kv_get`) decode
  bytes as UTF-8 with replacement. For binary values use the byte variants:
  Node `forge.kvGetBytes()` / `forge.kvSetBytes()` / `forge.blobGetBytes()` /
  `forge.blobPutBytes()`; Python `forge.kv_get_bytes()` / `forge.kv_set_bytes()`, and
  in Python `forge.blob_put()` / `forge.blob_get()` are already bytes-native (there is
  no `blob_*_bytes`; use `forge.blob_put_object()` when you also need metadata).
- **Queue receipts are opaque and process-local in the bindings.** Ack/nack/heartbeat a
  job with its `receipt` (delivery-unique), never its `id` (stable across
  redeliveries — that is your idempotency key). A receipt only settles from the same
  client that leased it.
- **pub/sub is at-most-once.** No acks, no replay, delivered only to live subscribers.
  Never use it as the system of record.
- **The scheduler and maintenance sweep only run when you tick them.** Forge starts no
  threads. From Node/Python, call `runSchedulerOnce` / `run_scheduler_once` and
  `maintain` on an interval (e.g. every 30s). In Rust use the maintenance loop.
  Without a tick, crons never fire and expired rows never get swept.
- **`memory` backends are per-process.** For pub/sub and rate limit that means no
  cross-replica delivery, so `memory` is for tests only — Forge logs a warning if you
  run those two on `memory` outside tests. Use `postgres` in any multi-replica deploy.

## Before you finish

Every method you call must exist in the binding. If you are unsure of a name, grep the
contract (`bindings/node/client.d.ts`, `bindings/python/src/lib.rs`, `src/lib.rs`) or
the per-language reference here. The repo's `tools/skill-check` guard verifies the
names in this skill against those files, so a name here is real for the committed API.

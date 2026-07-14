# Forge runtime contract

Read this reference for configuration, deployment, shared primitive behavior, error
handling, and lifecycle work. Then use the selected language reference for exact
method names and return types.

## `forge.toml`

`init()` reads `forge.toml`, validates unknown keys, initializes the system database,
and runs Forge's own migrations. A normal Postgres configuration creates a pool
(ten connections by default); Postgres pub/sub can also hold a listener connection.
Do not budget infrastructure as though Forge always uses one connection.

```toml
[postgres]
# A non-empty URL wins. Otherwise embedded mode runs local Postgres 17 and persists
# its data in .forge/pg.
url = "${DATABASE_URL:-}"
embedded = true

[backends]
default = "${FORGE_BACKEND:-postgres}"

[blob]
# Include only when presigned URLs are used. No empty fallback.
signing_secret = "${FORGE_BLOB_SIGNING_SECRET}"
```

- `${VAR}` and `${VAR:-default}` interpolation applies to strings. A missing `${VAR}`
  without a default is a configuration error.
- Never write `signing_secret = "${SECRET:-}"`. Treat an empty or whitespace-only
  secret as invalid. Ordinary blob CRUD needs no signing secret.
- `backends.default = "memory"` is useful in tests. Normal Forge initialization still
  needs its system Postgres database or embedded mode even when primitives use memory.
- `[forge].namespace` prefixes Forge keys, queues, and topics and may not contain `:`.
- Presigned URLs use `/api/files` by default; `[blob].base_url` can make the base
  relative or absolute. Mount a matching route and verify every issued field.
- Initialize one `ForgeClient`/`Forge` instance per process rather than per request.

Embedded Postgres needs one owner for a local data directory. When web, worker,
migration, and test commands are separate processes, start or reuse one embedded
server and pass its resolved DSN to the others. Keep DSN handoff files private and
gitignored; never print credentials. Production needs a shared, operationally owned
PostgreSQL service reachable by every Forge process; it may be managed or responsibly
self-hosted.

The resolved system DSN contains credentials. `postgresUrl()` / `postgres_url()` is
primarily how another process reaches embedded Postgres and can also seed an
application-owned pool when intentionally sharing that database. Production should
choose database/schema isolation deliberately. Never expose or log the DSN, write
domain tables through Rust's Forge system pool accessor, or modify Forge's internal
tables.

## Persistence depends on the selected backend

Postgres-backed KV, queues, rate-limit buckets, config, sessions, and other Forge
state persist across processes and restarts. Memory-backed state does not. Use unique
fixtures and test persistence-sensitive claims against Postgres. A primitive is
durable only when its selected backend is durable.

Configuration reads and flag evaluations can remain stale in a process for up to
about 30 seconds because of the in-process cache. Do not use them for instantaneous
revocation or a strongly consistent coordination boundary.

## Error semantics

Bindings expose the same canonical categories with language-specific names.

| Code | Meaning | Retry? |
| --- | --- | --- |
| `NOT_FOUND` | Required entity does not exist | No |
| `INVALID` | Bad argument, malformed key/hash, unsupported method, invalid option | No |
| `LIMIT` | Size, length, or quota ceiling | No |
| `PRECONDITION` | Lost/unknown queue lease or failed state precondition | Re-read and decide |
| `UNAVAILABLE` | Transient backend outage | Yes |
| `CONFIG` | Initialization/runtime misconfiguration | No |
| `BACKEND` | Other backend failure | Only when marked retryable |

A KV compare-and-swap mismatch returns `false`; it is not an exception. Presign
verification returns `false` for expiry or a bad signature, while an unsupported HTTP
method is `INVALID`. Retry only `UNAVAILABLE` or a backend error explicitly marked
retryable. Map Forge failures to application-level codes at the service boundary.

## Queue, pub/sub, and scheduler behavior

- A repeated `dedupId`/`dedup_id` inside `[queue].dedup_window_secs` returns the
  existing job id. The default window is 300 seconds; it is configurable, and dedup
  state can outlive acknowledgement.
- Settle with the receipt/leased job, never the stable job id. Delivery is
  at-least-once; poison jobs eventually move to `<queue>.dlq`.
- Queue depth is an approximate point-in-time estimate, not a transactional count.
- Managed workers heartbeat, ack on handler success, nack on handler error, and
  abandon a lost lease. Await the worker after signaling shutdown. For Node/Python,
  check whether the installed helper rechecks shutdown after a long-poll dequeue;
  helpers that do not may begin one final job.
- Pub/sub is at-most-once and live-subscriber-only. When a publication announces a
  durable change, publish after commit and have subscribers reconcile. Purely
  ephemeral events may publish directly.
- Built-in pub/sub accepts valid UTF-8 payloads up to 7,000 bytes. Raw Node/Python
  publishers take strings and raw subscribers yield bytes; Rust exposes bytes but the
  same UTF-8 backend contract applies. Typed topics encode/decode application values.
- `pubsubChannel()` / `pubsub_channel()` exposes the backend channel mapping. External
  PostgreSQL `LISTEN` use is meaningful only with the Postgres pub/sub backend.
- Forge does not start application scheduler or housekeeping threads. Drive the
  scheduler and `maintain` on an interval when the selected behavior needs them.

## Rate limiting and counters

The default token bucket starts full, permits a burst, then refills continuously.
Sliding-window mode approximates a hard window and can slightly overshoot at a
boundary. A rate limiter is not a transactional business counter.

Run credential limiters before expensive password hashing or verification. Choose
the fail mode explicitly: fail closed when bypass creates unacceptable security or
financial risk; fail open with monitoring and defense in depth when availability
takes precedence. Signup, login, invite, reset, and API-key routes can have different
tradeoffs. Key IP limits from the socket address unless a trusted proxy overwrites
forwarding headers.

Node's raw KV increment returns an f64-backed JavaScript `number`, so values above
2^53 lose precision. Python and Rust return exact integers. Use a transactional store
when a counter participates in multi-record business invariants.

## Authentication behavior

Password verification raises/returns `INVALID` for a malformed stored hash rather
than returning false. `needsRehash()` reports true for a malformed hash. Successful
session validation advances the sliding idle deadline up to the absolute deadline.

One-time token consumption is atomic and destructive. There is no peek, reservation,
or rollback API. A workflow that consumes a token and then writes another datastore
needs an application-level recovery/claim design when losing the post-consume write
is unacceptable.

## Blob presigning

Authorize ownership before issuing a URL and where appropriate when serving it. Pass
method, key, expiry, maximum bytes, and signature exactly as issued; downloads carry
`max_bytes=0`. Expiry or a bad signature yields `false`; an unsupported method is
`INVALID`.

Forge object metadata and key structure may be sufficient for simple ownership and
retention models. Use application metadata when authorization, relationships, search,
workflow, or retention needs more. Prefer unguessable tenant/owner-scoped keys. For
partial-failure-safe upload/delete patterns, read application design guidance.

## Shutdown

There is no general client `close()` method. Stop new work owned by the process,
signal workers and scheduler/maintenance loops, close subscriptions, and await their
completion before exit. An outer timeout is acceptable when required by the platform;
log the forced path and make the timeout longer than the intended grace period.

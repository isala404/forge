# kv — lineage: Redis command set

## Lineage

Mirrors the **Redis** string command set: `GET`, `SET` (+ `NX`/`XX`/`EX`), `DEL`,
`EXISTS`, `INCRBY`, `EXPIRE`, `SCAN MATCH`. Method names are Rust-idiomatic; behavior
matches Redis. `compare_and_swap` is borrowed from **memcached `cas`** / **etcd txn**
(a documented deviation — Redis has no single-key CAS primitive). Cursor pagination
follows Redis `SCAN`. Byte-value and TTL handling align with **wasi-keyvalue**.

Purpose: caching, sessions, counters, ephemeral state. Not a blob store, not a
relational store, not a message bus.

## Trait (Rust sketch — directional; this doc wins on conflict)

```rust
#[async_trait]
pub trait Kv: Send + Sync {
    async fn get(&self, key: &str) -> Result<Option<Bytes>>;                 // GET
    async fn mget(&self, keys: &[&str]) -> Result<Vec<Option<Bytes>>>;       // MGET
    async fn set(&self, key: &str, value: Bytes, opts: SetOpts) -> Result<bool>; // SET / NX / XX
    async fn delete(&self, key: &str) -> Result<bool>;                       // DEL
    async fn exists(&self, key: &str) -> Result<bool>;                       // EXISTS
    async fn incr(&self, key: &str, by: i64) -> Result<i64>;                 // INCRBY
    async fn expire(&self, key: &str, ttl: Duration) -> Result<bool>;        // EXPIRE
    async fn compare_and_swap(                                               // memcached cas / etcd txn
        &self, key: &str, old: Option<Bytes>, new: Bytes,
    ) -> Result<bool>;
    async fn scan(                                                           // SCAN MATCH prefix*
        &self, prefix: &str, cursor: Option<Cursor>, limit: u32,
    ) -> Result<(Vec<String>, Option<Cursor>)>;
}

#[non_exhaustive]
pub struct SetOpts { pub ttl: Option<Duration>, pub mode: SetMode }

pub enum SetMode { Always, IfNotExists, IfExists } // SET / SET NX / SET XX

pub struct Cursor(/* opaque, backend-owned */);
```

`Bytes` is the value type (opaque bytes, never interpreted except by `incr`/CAS).
`Cursor` is opaque: callers pass back exactly what `scan` returned, never construct it.

## Semantics

| op | behavior |
|----|----------|
| `get` | Returns `Some(value)` if present and unexpired, else `None`. Expired key returns `None`, guaranteed (see TTL). |
| `mget` | Batch `GET` in one round-trip. Returns a vec with **one slot per input key, in input order**: `Some(value)` for a live key, `None` for an absent/expired one (same per-key rule as `get`). Duplicate input keys repeat their value at each position; empty input → empty vec. Not a transaction — the keys are read in one statement but carry no cross-key snapshot guarantee (see Delivery). Avoids the N-round-trip loop a per-key `get` fan-out costs. |
| `set(Always)` | Unconditional write; overwrites value and replaces TTL with `opts.ttl` (or clears it if `None`). Returns `true` always. |
| `set(IfNotExists)` | Writes only if key absent or expired. Returns `true` if written, `false` if a live key blocked it. (Redis `SET NX`.) |
| `set(IfExists)` | Writes only if a live key exists. Returns `true` if written, `false` if absent/expired. (Redis `SET XX`.) |
| `delete` | Removes the key. Returns `true` if a key was removed, `false` if absent (or already expired). |
| `exists` | Returns `true` iff a live, unexpired key is present. Never resurrects an expired key. |
| `incr` | Atomic. Missing/expired key starts from `0`, so result is `by`. Non-numeric existing value → `Invalid`. Overflow of `i64` → `Limit`. TTL is preserved across `incr` (an existing TTL is not reset; a counter created by `incr` has no TTL until `expire`/`set` sets one). |
| `expire` | Sets/replaces TTL on a live key. Returns `true` if applied, `false` if the key is absent or already expired. Does not create keys. |
| `compare_and_swap` | Atomic single-key swap. Writes `new` iff current state equals `old` (`old = None` means "expected absent/expired"). Returns `true` on swap, `false` on mismatch. TTL is cleared by a successful swap unless the backend extends CAS with TTL (Forge does not). |
| `scan` | Returns up to `limit` keys whose name starts with `prefix`, plus a `Cursor` for the next page (`None` when iteration is complete). Cursor-based only — no offset. |

A value and a counter share one keyspace: `incr` on a key written by `set` with a
non-numeric value errors; `get` on a key written by `incr` returns the decimal ASCII
encoding of the integer as `Bytes` (Redis semantics — counters *are* string values).

## Delivery / consistency guarantees

- **Last-write-wins.** Concurrent `set`s resolve to whichever commits last; no merge.
- **Single-key atomicity only.** `incr`, `compare_and_swap`, and each `set` mode are
  atomic with respect to one key. There are **no multi-key transactions** and no
  cross-key snapshot.
- Reads observe committed writes (read-committed). A `get` may race a concurrent `set`
  and see either the old or new value, never a torn value.

## Ordering

No global ordering across keys. Per key, operations are linearizable: a write that
returns success is visible to every subsequent read of that key. `scan` is a weakly
consistent iteration (Redis `SCAN` semantics): keys present for the whole scan are
returned at least once; keys added or removed mid-scan may or may not appear; a key
may be returned more than once across pages. Callers must tolerate duplicates.

## TTL / expiry

- **Precision: seconds.** Sub-second `ttl` rounds up to the next whole second; a
  positive `ttl` never rounds to 0.
- **Lazy + background sweep.** Expiry is enforced at read time (every `get`/`exists`/
  `incr`/`scan`/CAS treats an expired key as absent) and reclaimed by a background
  sweep. A `get` after expiry returns `None`, **guaranteed**, regardless of sweep
  timing.
- A `set(Always)` with `ttl: None` persists the key with no expiry. `expire` is the
  only way to add a TTL without rewriting the value.

## Limits

| thing | limit | over-limit error |
|-------|-------|------------------|
| key | ≤ 512 bytes, UTF-8 | `Limit` |
| value | ≤ 1 MiB (1 048 576 bytes) | `Limit` |
| `incr` result | `i64` range `[-2^63, 2^63 − 1]` | `Limit` |
| `scan` limit | advisory; backend may return fewer | — |
| TTL | seconds precision; `>= 1s`, `<=` a fixed ~100-year ceiling | `Invalid` if zero/negative; `Limit` if over the ceiling |

Keys MAY contain `:` (the Redis `entity:id:field` convention). The configured
namespace is itself forced colon-free, so a stored key `<namespace>:<key>` is decoded
by splitting on the first `:` — the rest of the key, colons included, is preserved.
Apps sharing one database for isolation should each use a distinct, non-empty
namespace. Keys are UTF-8; the 512-byte cap is on the encoded byte length, not
character count. The TTL ceiling is a fixed Forge constant
of ~100 years (a relative expiry beyond a century is effectively always a bug, and a
fixed cross-vendor ceiling keeps the Postgres and Redis backends in agreement); a TTL
above it is `Limit`, not silently clamped.

## Error mapping

| condition | variant | retryable |
|-----------|---------|-----------|
| key absent on a read path | returns `None`/`false`, not an error | — |
| `set NX` blocked / `set XX` missed | returns `false`, not an error | — |
| `compare_and_swap` mismatch | returns `false`, not an error | — |
| key > 512 B, value > 1 MiB, counter overflow, TTL over max | `Limit` | no |
| non-numeric `incr` target, zero/negative TTL | `Invalid` | no |
| transient backend outage (pool timeout, dropped conn, `08xxx`/`57014`) | `Unavailable` | yes |
| other vendor/SDK error | `Backend` (carries retryability flag) | per flag |

`NotFound` and `Precondition` are **never** produced by this surface. Absence on a read
path is `None`/`false`, matching Redis (`GET` on a missing key is a nil reply, not an
error). A CAS or `set NX`/`set XX` miss is also a `false` return, not a `Precondition`
error — the contract has no "expected present, hard-fail on miss" operation. Error
messages never contain key bytes or value contents.

> Salvage note: the old `KvStore` mapped overflow (`22003`) to `InvalidArgument`; under
> this taxonomy a range overflow is `Limit`, not `Invalid`. Non-numeric `incr` (a caller
> bug, not a size) stays `Invalid`.

## Deviations from lineage

- **`compare_and_swap` is not Redis.** Redis has no single-key CAS; the closest is a
  `WATCH`/`MULTI`/`EXEC` transaction. We expose the simpler memcached/etcd CAS shape
  because it is the one primitive sessions and leases actually need, and it maps
  cleanly onto a Postgres conditional `UPDATE`.
- **Seconds-only TTL precision.** Redis supports `PEXPIRE` (ms). Forge fixes precision
  at seconds so the Postgres-backed and Redis-backed vendors agree on the lowest common
  denominator. Sub-second TTLs round up.
- **`set` returns `bool`, not Redis's `OK`/nil.** The bool reports whether the write
  happened, unifying `SET`, `SET NX`, and `SET XX` under one return shape.
- **`scan` matches a prefix, not a glob.** Redis `SCAN MATCH` takes a glob pattern;
  Forge takes a literal prefix (implemented as `MATCH prefix*`). No value filtering.
- **Counters and values share a keyspace by contract.** The old implementation kept
  `BYTEA` values and `BIGINT` counters in separate tables; that is an implementation
  detail. The contract presents one keyspace where a counter is a string value, so
  `incr` then `get` is well-defined (Redis behavior).
- **512-byte key cap (Redis allows 512 MB).** Redis keys may be up to 512 MB; Forge caps
  keys at 512 **bytes** so a key fits a btree index entry without TOAST and a Postgres
  vendor can index `key` directly. Over => `Limit`.
- **1 MiB value cap (Redis allows 512 MB).** Redis values may be up to 512 MB; Forge caps
  values at 1 MiB. This is a string/counter/session store, not a blob store — multi-MB
  values are almost always a misuse and round-trip the protocol per call. Over => `Limit`.
- **Namespace decoding, not a key restriction.** Redis keys are binary-safe and `:` is a
  convention. Forge keeps the Redis convention — keys may contain `:` — and reserves it only
  as the *namespace* separator: the configured namespace is colon-free, so the physical key
  `<namespace>:<key>` decodes by splitting on the first `:`. (The old draft forbade `:` in
  keys; that contradicted the lineage and is dropped.)
- **`incr` numeric parsing is slightly more lenient than Redis.** Redis's integer parser
  rejects surrounding whitespace and a leading `+`; Forge parses the stored value with
  Postgres's `text::bigint`, which accepts `" 5 "`, `"+5"`, and a trailing newline. Such a
  value increments rather than erroring. A value that is not an integer at all (including
  non-UTF-8 bytes) is still `Invalid`, matching Redis.

## Observability

Span `forge.kv.<op>` (e.g. `forge.kv.get`, `forge.kv.set`, `forge.kv.incr`,
`forge.kv.cas`, `forge.kv.scan`). Fields:

| field | notes |
|-------|-------|
| `kv.op` | operation name |
| `kv.key_hash` | stable hash of the key — never the raw key |
| `kv.namespace` | configured namespace prefix |
| `kv.hit` | `get`/`exists`: whether a live value was found |
| `kv.wrote` | `set`/`cas`: whether the write committed |
| `kv.ttl_secs` | resolved TTL in seconds, if any |
| `kv.value_bytes` | value size on write paths |
| `kv.scan_returned` | `scan`: count of keys in this page |
| `kv.outcome` | `ok` / error variant |

Key bytes and value contents are **never** emitted — only hashes, sizes, and counts.

## Non-goals

- **No keyspace notifications / pub-sub on expiry or change.**
- **No value-filtered scans** — `scan` matches key prefixes only, never inspects values.
- **No multi-key transactions** and no cross-key atomicity or snapshots.
- **No Redis data structures** — lists, sets, sorted sets, hashes, streams, bitmaps,
  HyperLogLog are out of scope. This is a string/counter keyspace only.
- **No millisecond TTL precision**, no `PERSIST`-style separate API (clear TTL via
  `set(Always, ttl: None)`).
- **Not a blob store** — the 1 MiB cap is deliberate; large objects belong elsewhere.

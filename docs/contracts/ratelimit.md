# ratelimit — lineage: token bucket / GCRA + IETF RateLimit fields

Throttle requests per subject. Atomic check-and-consume — no peek.

## Lineage

Mirrors the classic **token bucket** (continuous refill) and **GCRA**-style
sliding window used by Stripe's rate limiter and the standard nginx/Envoy limiters.
The single op `check` is an **atomic check-and-consume**: one call both tests the
limit and, if allowed, consumes one unit — the same shape as Stripe's limiter and
GCRA's "consume on admit." `Decision` is laid out to map 1:1 onto the
**IETF `RateLimit` header fields** (draft-ietf-httpapi-ratelimit-headers): `limit`,
`remaining`, `reset`, and `Retry-After`, so a handler emits the response headers
straight off the struct. Counters use the same atomic per-row machinery as `kv.incr`:
a small per-`(bucket, key)` row carrying a window/refill timestamp.

Purpose: protecting endpoints, login throttling, fair-use quotas, abuse mitigation.
Not a billing meter, not a job scheduler, not a global cross-region quota service.

> **There is deliberately NO peek.** A separate "is there room?" read followed by a
> "consume" write is the classic TOCTOU race — two callers both read room, both
> consume, both pass. `check` collapses the two into one atomic step. Test and consume
> are the same call, always.

## Trait (Rust sketch — directional; this doc wins on conflict)

```rust
#[async_trait]
pub trait RateLimit: Send + Sync {
    /// Atomic check-and-consume of one unit against the policy `limit` for
    /// subject `key` under namespace `bucket`. A *denied* request is
    /// `Ok(Decision { allowed: false, .. })`, NOT an `Err`. See Error mapping.
    async fn check(&self, bucket: &str, key: &str, limit: Limit)
        -> Result<Decision, ForgeError>;
}

#[non_exhaustive]
pub struct Limit {
    /// Max units admitted per window / bucket capacity. Must be > 0.
    pub max: u32,
    /// Window length (SlidingWindow) / refill period for `max` tokens
    /// (TokenBucket). Must be > 0. Seconds precision.
    pub per: Duration,
    pub algo: Algo,
}

#[non_exhaustive]
pub enum Algo { TokenBucket, SlidingWindow }

#[non_exhaustive]
pub struct Decision {
    /// Whether this call was admitted (and consumed one unit).
    pub allowed: bool,
    /// Echoes `Limit.max`. -> RateLimit-Limit.
    pub limit: u32,
    /// Units left in the current window after this call. -> RateLimit-Remaining.
    pub remaining: u32,
    /// Time until the limit fully resets / a unit frees up. -> RateLimit-Reset.
    pub reset_after: Duration,
    /// Set iff `!allowed`: earliest retry. -> Retry-After. `None` when allowed.
    pub retry_after: Option<Duration>,
}
```

`bucket` namespaces a **policy** (e.g. `"login"`, `"api.write"`); `key` is the
**subject** (e.g. a user id, API key, or IP). The pair `(bucket, key)` selects one
counter row. `Limit` is passed per call — the policy lives in caller code, not in
server config — so the same `bucket` may be checked with different `max`/`per` from
different call sites; the row tracks consumption, the `Limit` parametrizes the math.

## Semantics

| op | behavior |
|----|----------|
| `check` (TokenBucket, room) | Refills the bucket continuously at `max / per` since its last touch (capped at `max`), consumes one token, returns `allowed = true`, `remaining = floor(tokens_left)`, `reset_after =` time for the bucket to refill to full, `retry_after = None`. |
| `check` (TokenBucket, empty) | Bucket has < 1 token after refill. Consumes nothing, returns `allowed = false`, `remaining = 0`, `retry_after =` time until one token accrues (`per / max`), `reset_after =` time to full. |
| `check` (SlidingWindow, room) | Admits if the approximate count over the trailing `per` window is `< max`. Consumes one, returns `allowed = true`, `remaining = max − count`, `reset_after =` time until the window edge advances enough to free a unit. |
| `check` (SlidingWindow, full) | Approximate count `>= max`. Consumes nothing, returns `allowed = false`, `remaining = 0`, `retry_after = reset_after =` time until the oldest counted unit ages out of the window. |
| `check` (first ever for `(bucket,key)`) | Creates the row lazily, full bucket / empty window, admits, consumes one. No pre-provisioning. |
| `check` (backend error) | Per the configured failure mode: **fail-open** (default) returns a synthetic allow `Decision` and logs a warning; **fail-closed** returns `Err`. See *Failure mode*. |

`check` always consumes **exactly one** unit per admitted call — there is no
variable-cost (`n` units) consume in v1. A denied call consumes nothing.

## Delivery / consistency guarantees

- **Single-DB accurate, not globally precise.** Each `check` is atomic against its one
  Postgres row (same conditional-`UPDATE` / `incr` machinery as `kv`), so concurrent
  `check`s on the same `(bucket, key)` serialize correctly within one database — no
  TOCTOU, no double-spend. Across **separate** databases or regions there is no shared
  counter; the limit is enforced per backend, not globally. (See Non-goals.)
- **Atomicity is per `(bucket, key)`.** No cross-key or cross-bucket atomicity. A burst
  spread across many keys is many independent decisions.
- **Last-touch refill.** The row stores the last refill/window timestamp; refill is
  computed on read at `check` time, not by a background ticker. A key never checked
  holds no row and costs nothing.

## Ordering

No ordering guarantee across keys. Per `(bucket, key)`, `check` calls are linearizable:
the unit a successful `check` consumes is visible to every subsequent `check` on that
key. Concurrent `check`s on one key resolve to a serial order (whichever commits first
consumes first); none observe a torn count.

## Algorithm accuracy

- **TokenBucket** refills **continuously** at `max / per` tokens per second (computed
  from elapsed time since last touch), capped at `max`. It admits short bursts up to the
  full bucket and then throttles to the steady rate. Token math is fractional internally;
  `remaining` is reported as `floor(tokens)`.
- **SlidingWindow** is **approximate within one sub-window**. It is the standard
  fixed-window-with-weighting approximation (current + decayed prior window), not an
  exact per-event log. Worst-case it admits up to ~`max` extra over a window boundary
  versus an idealized exact sliding count. Choose TokenBucket when smooth burst control
  matters; SlidingWindow when a simple "N per period" cap is enough.
- **Time precision: seconds.** `per` is seconds-precision (sub-second rounds up to 1s, a
  positive `per` never rounds to 0). `reset_after` / `retry_after` are computed in
  seconds and are themselves approximate within a second.

## IETF RateLimit header mapping

`Decision` maps 1:1 onto the draft IETF fields; a handler emits them directly:

| header | from |
|--------|------|
| `RateLimit-Limit` | `decision.limit` |
| `RateLimit-Remaining` | `decision.remaining` |
| `RateLimit-Reset` | `decision.reset_after.as_secs()` (seconds until reset) |
| `Retry-After` | `decision.retry_after.map(|d| d.as_secs())` — set only on a 429 |

```rust
// axum example (illustrative — Forge ships no HTTP middleware; see Non-goals)
let d = forge.ratelimit().check("login", &client_ip, login_policy).await?;
let mut res = if d.allowed { next.run(req).await } else { StatusCode::TOO_MANY_REQUESTS.into_response() };
let h = res.headers_mut();
h.insert("RateLimit-Limit", d.limit.into());
h.insert("RateLimit-Remaining", d.remaining.into());
h.insert("RateLimit-Reset", d.reset_after.as_secs().into());
if let Some(ra) = d.retry_after { h.insert("Retry-After", ra.as_secs().into()); }
```

## Failure mode (fail-open / fail-closed)

On a transient/backend error inside `check`, the per-call policy is **configurable**:

- **`fail-open` (DEFAULT):** `check` returns a synthetic **allow** `Decision`
  (`allowed = true`, `remaining = limit`, `reset_after = per`, `retry_after = None`) and
  logs a **warning**. The request proceeds; the limiter degrades to a no-op rather than
  taking the endpoint down with its dependency. This means `check` may return an *allow*
  on backend error instead of `Err`.
- **`fail-closed`:** `check` does **not** swallow the error — it returns `Err`
  (`Unavailable` / `Backend` per the taxonomy below), and the caller decides (typically
  reject with 503/429).

The mode is set once at init for the limiter. The default is **fail-open** because a
broken limiter blocking all traffic is usually worse than briefly unlimited traffic;
flip to fail-closed for buckets where over-admission is the greater harm (e.g. payment
or abuse-sensitive paths). `Invalid` (caller bug) is **never** subject to the failure
mode — a bad `Limit` always returns `Err` regardless of mode.

## Limits

| thing | limit | over-limit error |
|-------|-------|------------------|
| `bucket` | ≤ 128 bytes, UTF-8, non-empty | `Limit` (empty => `Invalid`) |
| `key` | ≤ 512 bytes, UTF-8, non-empty | `Limit` (empty => `Invalid`) |
| `Limit.max` | `1 ..= u32::MAX` | `Invalid` if `0` |
| `Limit.per` | `>= 1s` (sub-second rounds up); `<=` ~100-year ceiling | `Invalid` if `0`; `Limit` if over ceiling |
| consume amount | always exactly `1` per `check` | — (no n-unit consume in v1) |

`bucket` is colon-free where the backend uses `:` as the namespace separator (as `kv`
does); the physical counter key is `<namespace>:<bucket>:<key>`. Caps are on encoded
byte length, not character count. The `per` ceiling is the same fixed ~100-year Forge
constant as `kv` TTL, for cross-vendor agreement; over it is `Limit`, not clamped.

## Error mapping

| condition | variant | retryable |
|-----------|---------|-----------|
| request denied (over limit) | returns `Ok(Decision { allowed: false, .. })`, **not** an error | — |
| `Limit.max == 0` or `Limit.per == 0` | `Invalid` | no — caller bug |
| empty `bucket` / empty `key` | `Invalid` | no — caller bug |
| `bucket` > 128 B, `key` > 512 B, `per` over ceiling | `Limit` | no |
| transient backend outage (pool timeout, dropped conn, `08xxx`/`57014`) | **fail-open:** swallowed -> allow `Decision` + warn; **fail-closed:** `Unavailable` | yes (when surfaced) |
| other vendor/SDK error | **fail-open:** swallowed -> allow `Decision` + warn; **fail-closed:** `Backend` (carries retryability flag) | per flag |
| misconfiguration (bad DSN, missing migration, bad failure-mode config) at `Forge::init()` | `Config` | no — init only |

`allowed = false` is the normal denied outcome and is **never** an `Err` — a throttled
request is a successful `check` returning a deny `Decision`, so callers branch on
`decision.allowed`, not on `Result`. `NotFound` and `Precondition` are **never**
produced: there is no peek to miss and no registry to look a bucket up in (rows are
created lazily). The fail-open/fail-closed switch governs only the transient/`Backend`
rows above; an `Invalid` always errors. Error messages and log fields never contain the
raw `key` or `bucket` — hashes only.

## Deviations from lineage

- **No peek, by design.** GCRA/token-bucket libraries often expose a non-consuming
  "remaining" query; Forge omits it deliberately to make the TOCTOU race unrepresentable.
  Read the count off the `Decision` of the call you were going to make anyway.
- **Single-Postgres accurate, not distributed.** Stripe and other production limiters run
  on a shared Redis with a single authoritative counter. Forge's counter is per database;
  it is exact within one DB but does not coordinate across regions/databases. The contract
  promises single-DB accuracy only.
- **Two algorithms only.** `TokenBucket` and `SlidingWindow`. No leaky-bucket,
  fixed-window-counter, or concurrency/in-flight limiter variants in v1.
- **Fail-open default.** Many limiters fail closed (deny on backend error). Forge defaults
  to fail-open with a warning log — a limiter outage should not become a service outage —
  and makes the choice explicit and per-limiter, not silent.
- **Policy is a per-call argument, not server config.** `Limit` travels with each `check`,
  so policy lives in the caller's code (versioned, testable) rather than in limiter setup.
  The row stores only consumption state.

## Observability

Span `forge.ratelimit.check`. Fields:

| field | notes |
|-------|-------|
| `ratelimit.bucket` | policy namespace (low-cardinality; safe to emit) |
| `ratelimit.key_hash` | stable hash of the subject key — **never** the raw key |
| `ratelimit.algo` | `token_bucket` / `sliding_window` |
| `ratelimit.allowed` | whether this call was admitted |
| `ratelimit.limit` | resolved `Limit.max` |
| `ratelimit.remaining` | units left after this call |
| `ratelimit.reset_after_secs` | seconds to reset |
| `ratelimit.retry_after_secs` | seconds to retry, on a deny |
| `ratelimit.fail_open` | emitted only when a backend error was swallowed by fail-open |
| `ratelimit.outcome` | `allowed` / `denied` / `fail_open` / error variant |

A fail-open swallow emits a **WARN** with `ratelimit.fail_open = true` so degraded
operation is alertable. Subject keys and any payload are **never** emitted — only the
bucket name (low-cardinality policy id), hashes, and counts.

## Non-goals

- **Globally precise distributed limits across regions/databases.** Single-DB accurate
  only; no cross-region coordination, no shared global counter.
- **Built-in HTTP middleware.** Forge ships a short axum *example* (above), not a
  framework integration or a `tower::Layer` — wiring `check` into a stack is the app's job.
- **Algorithm variants beyond TokenBucket/SlidingWindow** — no leaky-bucket, no
  concurrency/in-flight limiter, no fixed-window-counter.
- **A non-consuming peek.** Removed by design (TOCTOU). Read state off the `Decision`.
- **Variable-cost (`n`-unit) consume.** Each `check` consumes exactly one unit in v1.
- **Distributed clock synchronization.** Refill/window math uses the backend DB's clock;
  Forge does not reconcile clocks across nodes.
- **A billing meter or usage-analytics store.** This caps request rate; it is not a
  durable record of consumption for invoicing.

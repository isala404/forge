# Emitting IETF RateLimit headers

`ratelimit().check` / `check_with` runs one atomic check-and-consume and hands back a `Decision` whose fields line up 1:1 with the draft IETF `RateLimit` header fields. A throttled request is *not* an error: a denied call is `Ok(Decision { allowed: false, .. })`, so you branch on `decision.allowed`, never on `Result`. This recipe maps a `Decision` onto `RateLimit-Limit`, `RateLimit-Remaining`, `RateLimit-Reset`, and `Retry-After`, and shows when to fail open versus closed.

## The `Decision` shape

```rust
pub struct Decision {
    pub allowed: bool,                    // admitted this call (and consumed one unit)
    pub limit: u32,                       // -> RateLimit-Limit   (echoes Limit.max)
    pub remaining: u32,                   // -> RateLimit-Remaining
    pub reset_after: Duration,            // -> RateLimit-Reset    (seconds to full reset)
    pub retry_after: Option<Duration>,    // -> Retry-After, set iff !allowed
}
```

`reset_after` is always present; `retry_after` is `Some` only when the call was denied.

## Example: a guard that returns headers either way

This is framework-agnostic on purpose — Forge ships no HTTP middleware. The pattern: call `check_with`, build the four header values off the `Decision`, and return them whether the request was admitted or throttled.

```rust
use forge::{Decision, FailMode, Forge, Limit, RateLimit};
use std::time::Duration;

/// One header set, emitted on both the 2xx and the 429 path.
struct RateLimitHeaders {
    limit: u32,
    remaining: u32,
    reset_secs: u64,
    retry_after_secs: Option<u64>,
}

impl RateLimitHeaders {
    fn from(d: &Decision) -> Self {
        Self {
            limit: d.limit,
            remaining: d.remaining,
            reset_secs: d.reset_after.as_secs(),
            // present only on a deny
            retry_after_secs: d.retry_after.map(|d| d.as_secs()),
        }
    }

    /// Write onto an http::HeaderMap (or whatever your stack uses).
    fn apply(&self, h: &mut http::HeaderMap) {
        h.insert("RateLimit-Limit", self.limit.into());
        h.insert("RateLimit-Remaining", self.remaining.into());
        h.insert("RateLimit-Reset", self.reset_secs.into());
        if let Some(ra) = self.retry_after_secs {
            h.insert("Retry-After", ra.into());
        }
    }
}

/// Login throttle: 5 attempts per minute per subject, fail CLOSED.
async fn login_guard(
    forge: &Forge,
    client_ip: &str,
) -> forge::Result<Result<RateLimitHeaders, RateLimitHeaders>> {
    let policy = Limit::per_duration(5, Duration::from_secs(60));

    // FailMode::Closed: a backend outage surfaces as Err here rather than a free pass —
    // right for an abuse-sensitive bucket. (`?` propagates it.)
    let d = forge
        .ratelimit()
        .check_with("login", client_ip, policy, FailMode::Closed)
        .await?;

    let headers = RateLimitHeaders::from(&d);
    // Ok(headers) -> proceed and attach; Err(headers) -> respond 429 and attach.
    Ok(if d.allowed { Ok(headers) } else { Err(headers) })
}
```

Wiring it into a handler is just branching on the inner `Result` and attaching `headers.apply(...)` to whichever response you return. The headers go on *both* paths — clients want their budget even on a 200.

`Limit::per_duration(max, per)` is the token-bucket default; `Limit::per_duration(..).with_algo(Algo::SlidingWindow)` switches to the sliding-window approximation. `Limit` is also `const`-constructible, which matters for the typed wrapper below.

## Choosing `FailMode::Open` vs `Closed`

The fail mode governs only what happens when the *backend* errors — never how a normal deny behaves (a deny is always `Ok(Decision { allowed: false })`).

- **`FailMode::Open`** (the instance default): on a soft/transient backend error, `check` returns a synthetic allow (`allowed: true`, `remaining == limit`, `reset_after == per`, `retry_after: None`) and logs a WARN. Use it for high-volume best-effort buckets where a limiter outage blocking everything is worse than briefly-unlimited traffic (sending a chat message).
- **`FailMode::Closed`**: any backend error surfaces as `Err` (`Unavailable`/`Backend`). Use it where over-admission is the greater harm (login, OTP, presign minting, payments).
- **`FailMode::Default`**: defer to the instance default. `check(...)` is exactly `check_with(..., FailMode::Default)`.

The instance default is `ForgeConfig.ratelimit_fail_open` (defaults to `true` = open), set with `.with_ratelimit_fail_open(false)` or the `FORGE_*` config env. One `RateLimit` can mix both modes per call, so you don't need a second instance. Caller bugs (`Invalid` — empty bucket/key, `max == 0`, `per == 0`) always surface as `Err` regardless of mode; only transient errors are ever swallowed by fail-open.

## Typed buckets with `RateBucket<S>`

Scattering the bucket name, the `Limit`, and the fail-mode across call sites is exactly the kind of duplication the typed layer removes. `RateBucket<S>` binds all three to one `const`, and its `check` takes any subject `S: RateSubject` (blanket-implemented for every `Display` type, so a `UserId` newtype works directly).

```rust
use forge::{Algo, FailMode, Limit, RateBucket};
use std::time::Duration;

// Declared once. Bucket name + policy + fail mode live together.
static LOGIN: RateBucket<str> = RateBucket::new(
    "login",
    Limit::per_duration(5, Duration::from_secs(60)),
    FailMode::Closed,
);

// A UserId newtype is a valid subject as long as it's Display.
static SEND: RateBucket<UserId> = RateBucket::new(
    "send",
    Limit::per_duration(5, Duration::from_secs(10)).with_algo(Algo::TokenBucket),
    FailMode::Open,
);

async fn check_login(forge: &forge::Forge, ip: &str) -> forge::Result<()> {
    // RateBucket::check(&self, rl: &dyn RateLimit, subject: &S) -> Result<Decision>
    let d = LOGIN.check(forge.ratelimit(), ip).await?;
    if !d.allowed {
        // build Retry-After from d.retry_after, RateLimit-Reset from d.reset_after, ...
    }
    Ok(())
}
```

`RateBucket::check` returns the same `Decision`, so the header-mapping code above is unchanged. Note the subject argument is `&S`: pass `ip` (a `&str`) for `RateBucket<str>`, or `&user_id` for `RateBucket<UserId>`.

## Gotchas and contract guarantees

- **No peek, by design.** There is exactly one operation, `check`, an atomic check-and-consume. Read your remaining budget off the `Decision` of the call you were already going to make — a separate "is there room?" read is the classic TOCTOU race and is deliberately unrepresentable.
- **Exactly one unit per admitted call.** No variable-cost (`n`-unit) consume in v1. A denied call consumes nothing.
- **Seconds precision.** `per` is rounded up to whole seconds (a positive `per` never becomes 0). `reset_after` / `retry_after` are computed in seconds and approximate within a second — fine for the integer-seconds header values, but don't treat them as sub-second timers.
- **Single-DB accurate, not global.** Each `(bucket, key)` is one atomic Postgres row; concurrent checks on the same key serialize correctly. Across separate databases/regions there is no shared counter — the limit is per backend.
- **Limits.** `bucket` ≤ 128 bytes, `key` ≤ 512 bytes, both non-empty UTF-8; `max ≥ 1`; `per ≥ 1s`. Empty bucket/key or `max == 0`/`per == 0` is `Invalid`; over the byte/`per` ceilings is `Limit`. These are caller bugs and always `Err`, never swallowed by fail-open.
- **Privacy.** Logs and spans (`forge.ratelimit.check`) emit the bucket name and a hash of the key, never the raw subject.

## Node / Python bindings

Both bindings expose the check but flatten it. The fail mode is an optional boolean (`failOpen` / `fail_open`): omit for the instance default, `true` for open, `false` for closed.

- **Node** — `rate_limit_check(bucket, key, max, perSeconds, failOpen?)` returns `{ allowed, limit, remaining, retryAfterSeconds }`.
- **Python** — the equivalent check returns a `(allowed, remaining, retryAfterSeconds)` tuple.

Heads-up for this recipe specifically: neither binding currently surfaces `reset_after`, so you **cannot** emit `RateLimit-Reset` from the bindings the way you can in Rust — you only get `RateLimit-Limit` (Node), `RateLimit-Remaining`, and `Retry-After`. Both also expose only token-bucket via `perSeconds` (no algorithm switch). If you need `RateLimit-Reset` or `SlidingWindow`, that path is Rust-only today.

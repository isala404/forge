# Forge — Rust reference

The crate is `forgelib`. Primitives hang off namespaced accessors on `Forge` (`forge.kv()`, `forge.queue()`, …), and calls take option-struct builders instead of long positional argument lists. Fallible calls return `Result`, so `?` them. Bodies are `Bytes`; use `.into()` from `&str`/`Vec<u8>`. Check the crate source resolved by `Cargo.lock` when exact behavior matters.

```rust
use std::time::Duration;
use forgelib::{Forge, Bytes, SetOpts, SetMode, EnqueueOpts, DequeueOpts, NackOpts,
    PutOpts, ScheduleOpts, SessionOpts, Limit, Algo, FailMode, FlagRule, EvalCtx};

let forge = Forge::init().await?;                    // reads ./forge.toml
// Forge::init_from(path) / Forge::init_from_str(toml) also exist.
// `[postgres] embedded = true` needs the `embedded` cargo feature in Rust
// (the Node/Python packages ship with it built in). If app-owned tables intentionally
// share that database, build a separate application pool from forge.postgres_url().
// forge.pool() is the Forge system pool and is not a home for domain tables.
```

## Key/value — `forge.kv()`

```rust
forge.kv().set("k", "v".into(),
    SetOpts::new().with_ttl(Duration::from_secs(60))).await?;   // NX: .with_mode(SetMode::IfNotExists)
let v: Option<Bytes> = forge.kv().get("k").await?;
let many = forge.kv().mget(&["a", "b"]).await?;
let n = forge.kv().incr("hits", 1).await?;                      // exact i64
forge.kv().expire("k", Duration::from_secs(30)).await?;
forge.kv().compare_and_swap("k", Some("v".into()), "w".into()).await?; // old = None means "expect absent"
let (keys, next) = forge.kv().scan("user:", None, 100).await?; // cursor pagination
forge.kv().delete("k").await?;
forge.kv().exists("k").await?;
```

## Queue — `forge.queue()`

```rust
let id = forge.queue().enqueue("emails", payload.into(),
    EnqueueOpts::new().with_max_attempts(5).with_dedup_id("once")).await?;
if let Some(job) = forge.queue().dequeue("emails", DequeueOpts::new()).await? {
    // ... settle by the whole `job`, using its delivery-unique lease
    forge.queue().heartbeat(&job).await?;             // extend the lease mid-flight
    forge.queue().ack(&job).await?;                  // or .nack(&job, NackOpts::retry_in(d))
}
let depth = forge.queue().depth("emails").await?;    // approximate point-in-time counts
```

The lease token settles the delivery. `Job::id()` is stable across redelivery and is useful as an idempotency-key base, but scope the final key to the logical effect or use a domain operation id when duplicate jobs are possible. A repeated `dedup_id` inside the configured window returns the existing id (no error); the default window is 300 seconds. Prefer the managed worker below to a hand-rolled loop.

## Managed worker — `forge.worker(name)`

```rust
forge.worker("emails")
    .concurrency(8)
    .visibility_timeout(Duration::from_secs(30))
    .run(|job| async move { handle(job).await })     // Ok => ack, Err => nack
    .await;
// .run_until(shutdown_future, handler) to drain on a shutdown signal.
```

The builder dequeues, heartbeats within the visibility window, acks on `Ok`, nacks on `Err`, and abandons the job if the lease is lost. `.grace(d)` and `.poll_wait(d)` tune shutdown drain and long-poll. Await `.run_until(...)` to completion before process exit; resolving the shutdown future begins the drain rather than completing it.

## Pub/sub — `forge.pubsub()`

```rust
use futures::StreamExt;                                             // Subscription is a Stream

forge.pubsub().publish("user.created", Bytes::from(event)).await?;   // at-most-once
let mut sub = forge.pubsub().subscribe("user.created").await?;
while let Some(payload) = sub.next().await { let payload = payload?; }
let channel = forge.pubsub().channel_for("user.created")?;          // raw LISTEN channel
```

Built-in pub/sub backends require valid UTF-8 payload bytes of at most 7,000 bytes. `channel_for` is the backend mapping; external PostgreSQL `LISTEN` use applies when the Postgres pub/sub backend is selected.

## Blob — `forge.blob()`

```rust
forge.blob().put("exports/x.csv", body,
    PutOpts::new().with_content_type("text/csv")).await?;
let bytes: Option<Bytes> = forge.blob().get("exports/x.csv").await?;
let info = forge.blob().head("exports/x.csv").await?;               // Option<BlobInfo>
let page = forge.blob().list("exports/", None, 100).await?;
let url = forge.blob().presign_download("exports/x.csv",
    Duration::from_secs(3600)).await?;                             // needs signing_secret
forge.blob().presign_upload("in/y", Duration::from_secs(600), 5_000_000).await?;
forge.blob().delete("exports/x.csv").await?;
```

## Auth — `forge.auth()`

```rust
let hash = forge.auth().hash_password(&plain).await?;
if forge.auth().verify_password(&plain, &hash).await? {
    if forge.auth().needs_rehash(&hash) { /* re-hash and persist */ }
}
let token = forge.auth().create_session(&user_id, SessionOpts::new()
    .with_idle_timeout(Duration::from_secs(3600))).await?;
let session = forge.auth().validate_session(&token).await?;        // Option<Session>
forge.auth().revoke_session(&token).await?;
forge.auth().revoke_all_sessions(&user_id).await?;
let key = forge.auth().create_api_key(&owner_id, "ci").await?;     // key.secret shown once
let info = forge.auth().verify_api_key(key.secret.as_str()).await?;
forge.auth().revoke_api_key(&key.id).await?;
let reset = forge.auth().create_token(&user_id, "password-reset", Duration::from_secs(900)).await?;
let owner = forge.auth().consume_token(reset.as_str(), "password-reset").await?; // Option<String>, single use
```

A malformed stored password hash is `ForgeError::Invalid`, not `Ok(false)`; `needs_rehash` returns true for malformed hashes. Successful session validation slides the idle deadline up to the absolute deadline.

## Rate limit — `forge.ratelimit()`

```rust
let d = forge.ratelimit().check("login", &email,
    Limit::per_duration(20, Duration::from_secs(60))).await?;   // per < 1s is Invalid
if !d.allowed { /* d.retry_after */ }
// per-call fail mode override:
forge.ratelimit().check_with("login", &email,
    Limit::per_duration(20, Duration::from_secs(60)).with_algo(Algo::SlidingWindow),
    FailMode::Closed).await?;
```

Run credential limits before password work and choose the fail mode deliberately. Use `FailMode::Closed` when bypass creates unacceptable security/financial risk; use `FailMode::Open` with monitoring and defense in depth when availability takes precedence.

## Schedule — `forge.schedule()`

```rust
forge.schedule().cron("nightly", "0 0 * * *", "reports", Bytes::new(),
    ScheduleOpts::new()).await?;
let id = forge.schedule().at(when, "reports", Bytes::new(), ScheduleOpts::new()).await?;
forge.schedule().cancel("nightly").await?;
forge.schedule().cancel_at(id).await?;
let page = forge.schedule().list(None, 100).await?;

// Nothing ticks on its own: drive these from your own loop.
forge.run_scheduler_once().await?;                   // or forge.run_scheduler().await (loops)
forge.maintain().await?;                             // housekeeping sweep
```

## Config and flags — `forge.config()`

```rust
use forgelib::ConfigExt;                              // the JSON get/set<T> helpers

forge.config().set_raw("theme", "dark").await?;
let raw = forge.config().get_raw("theme").await?;    // Option<String>
let cfg: Option<Settings> = forge.config().get("settings").await?;   // JSON into T

forge.config().set_flag("new-ui", FlagRule::Percent(25)).await?;     // On / Off / Percent / AllowList
let on = forge.config().flag("new-ui", false, &EvalCtx::user(user_id)).await; // never errors
```

## Typed handles

The crate root also re-exports typed handles (`KvKey`, `QueueName`, `Topic`, `ConfigKey`, …) that bind a serde codec to a key/queue/topic so you pass real structs instead of `Bytes`. Reach for them when a payload is a `#[derive(Serialize)]` type.

## Custom backends — `Forge::builder()`

```rust
let forge = Forge::builder().kv(my_redis_kv).build().await?;  // one primitive swapped, rest on Postgres
```

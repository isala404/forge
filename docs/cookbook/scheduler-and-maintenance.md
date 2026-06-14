# Running the scheduler and maintenance loops

Forge's `schedule` primitive only decides *when* to enqueue; it never delivers work itself. Something has to drive its ticks, and something has to run the periodic backend sweeps. Both are explicit, single-method loops you own: `run_scheduler` (or `run_scheduler_once` for a custom loop) fires due cron/`at` schedules into the queue, and `maintain` runs every backend's housekeeping sweep. Neither uses leader election — run them on every replica, because a tick enqueues exactly once fleet-wide regardless of how many tickers are racing.

## The managed loop (Rust)

The simplest setup: register your schedules, then hand the loop to `run_scheduler`. It ticks every 30s and stops on SIGINT/SIGTERM.

```rust
use std::time::{Duration, SystemTime};
use forge::{Bytes, Forge, ForgeConfig};

#[tokio::main]
async fn main() -> forge::Result<()> {
    let forge = Forge::init(ForgeConfig::new("postgres://localhost/myapp")).await?;

    // Recurring: upsert by name. Re-running this on every deploy is idempotent —
    // it replaces the existing row rather than creating a duplicate.
    forge
        .schedule()
        .cron("nightly-report", "0 3 * * *", "reports", Bytes::from_static(b"{}"))
        .await?;

    // One-shot: fires once at an absolute instant, returns the future queue JobId
    // so you can correlate it via the queue once it lands.
    let when = SystemTime::now() + Duration::from_secs(3600);
    let job_id = forge
        .schedule()
        .at(when, "reminders", Bytes::from_static(br#"{"kind":"trial-end"}"#))
        .await?;
    let _ = job_id;

    // Drive ticks AND sweep backends. Two concerns, two loops — spawn maintain
    // separately so a slow sweep never delays a tick.
    let f = forge.clone();
    tokio::spawn(async move {
        loop {
            if let Err(e) = f.maintain().await {
                tracing::warn!(error = %e, "forge maintain failed");
            }
            tokio::time::sleep(Duration::from_secs(300)).await;
        }
    });

    forge.run_scheduler().await; // blocks until SIGINT/SIGTERM
    Ok(())
}
```

### Variants for tests and custom cadence

- `run_scheduler_until(shutdown)` — same 30s tick, but stops when your `shutdown` future resolves instead of on a signal. Use it when you manage lifecycle yourself.
- `run_scheduler_with(interval, shutdown)` — pick the tick `interval` (e.g. a 50ms tick in an integration test so you don't wait 30s for a fire). Both still enqueue every due schedule exactly once per tick, safely across replicas.

```rust
// In a test: tick fast, stop on a oneshot.
let (tx, rx) = tokio::sync::oneshot::channel::<()>();
let handle = tokio::spawn({
    let f = forge.clone();
    async move {
        f.run_scheduler_with(Duration::from_millis(50), async { let _ = rx.await; }).await;
    }
});
// ... assert the job landed in its queue ...
let _ = tx.send(());
handle.await.unwrap();
```

### Folding ticks into your own housekeeping loop

If you already have a periodic loop (to reconcile your own tables, say), call `run_scheduler_once` instead of taking over the thread. It runs one pass and returns how many jobs it enqueued.

```rust
async fn housekeeping(forge: Forge, shutdown: impl std::future::Future<Output = ()>) {
    let mut shutdown = std::pin::pin!(shutdown);
    loop {
        if let Err(e) = forge.run_scheduler_once().await {
            tracing::warn!(error = %e, "scheduler tick failed");
        }
        if let Err(e) = forge.maintain().await {
            tracing::warn!(error = %e, "forge maintain failed");
        }
        tokio::select! {
            _ = tokio::time::sleep(Duration::from_secs(30)) => {}
            _ = &mut shutdown => break,
        }
    }
}
```

This is exactly what the chatapp example does (`examples/chatapp/rust-be/src/main.rs`), because its app tables live on a pool it shares with Forge and it wants to run a reconciliation sweep on the same cadence.

## What `maintain` actually does

`maintain()` walks every backend's `BackendLifecycle::maintain` hook and runs each one's idempotent sweep: purge expired kv rows, drop old completed jobs and reclaim leases orphaned by crashed workers, expire dead sessions, drop stale dedup and rate-limit rows, and — for the filesystem blob backend — reclaim orphaned files. Backends with nothing to sweep (config, schedule, pubsub) inherit a no-op. It returns on the first backend error, so a failed sweep is worth logging and retrying next pass rather than treated as fatal.

This is deliberately *not* the same loop as the scheduler. `run_scheduler*` fires due schedules; `maintain` cleans up. You call both, on whatever intervals suit you (the contract just asks that `maintain` run "on a schedule").

## Cron and `at` semantics worth knowing

- **5-field cron, UTC only.** `"min hour dom mon dow"`, evaluated in UTC — no seconds field, no timezone, no DST. A sub-minute or malformed expression is rejected as `Invalid` at `cron()` time, never silently at tick time. Minimum resolution is one minute.
- **`cron` is upsert.** Re-registering the same name replaces `expr`/`queue`/`payload` atomically. Redeploying with the same schedule definitions is idempotent, not a conflict.
- **`at` accepts now/past.** A `when` already in the past (or now) is *not* an error — it fires on the next tick if it's within the missed-tick grace (under 1 hour late), otherwise it's skipped and logged. A `when` more than ~100 years out (`MAX_AT_HORIZON_DAYS = 36525`) is `Limit`; a century-out absolute time is effectively always a bug, and the fixed ceiling keeps backends in agreement (same rationale as the kv TTL ceiling).
- **`at` returns the future `JobId`.** The job isn't visible in the queue until the tick fires; the returned id lets you correlate/inspect/`ack` it through the queue once it lands.
- **Missed ticks fire at most once, no backfill.** If all tickers were down across many missed ticks, only the single most-recent one within the 1h deadline fires; the rest are dropped and logged. Forge does not replay a backlog.

## Guarantees and gotchas

- **Exactly one enqueue per tick, fleet-wide.** Each due row is claimed with `FOR UPDATE SKIP LOCKED` and, in the *same transaction*, the queue job is inserted and the row's `next_run` advanced (or the one-shot deleted). A concurrent replica sees it already claimed and never re-fires. No leader, no coordinator — run the scheduler on every replica. A crash between claim and enqueue loses nothing, because they commit together.
- **One enqueue is not one delivery.** The enqueued job then rides the queue, which is *at-least-once*. Lease expiry, a worker crash, or redelivery can produce more than one delivery from a single tick. Your consumer must be idempotent on the message. This is the queue contract showing through, not a scheduler bug.
- **No overlap policy in v1.** A tick fires whether or not the previous tick's job is still being consumed. If overlap matters, the consumer handles it.
- **`run_scheduler` blocks until a signal.** It only returns on SIGINT/SIGTERM. If you need it to coexist with an HTTP server, spawn it (or the server) on its own task, as the example does.

## Node and Python

The bindings expose the single-pass `runSchedulerOnce()` / `run_scheduler_once()` and `maintain()`, plus `scheduleAt`/`scheduleCron` (Node) and `at`/`cron` (Python) for registration. They do **not** expose the managed `run_scheduler` loop — drive ticks yourself on an interval (e.g. `setInterval` every 30s in Node, an `asyncio` loop in Python) and call `maintain()` alongside it. The exactly-one-enqueue-per-tick guarantee holds the same way, since it lives in Postgres, not in the loop. Note `scheduleAt` takes `whenEpochMs` (epoch milliseconds), not a `SystemTime`.

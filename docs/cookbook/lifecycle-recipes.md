# Deployment lifecycle recipes (web, workers, scheduler, multi-replica)

A Forge app is one process holding one `Forge` handle, plus whatever background loops it needs. There is no separate worker daemon, no scheduler service, no leader election — you compose the pieces yourself from four methods: `forge.worker(queue).run_until(...)` for consumers, `forge.run_scheduler(...)` (or `run_scheduler_once`) for cron/`at` ticks, and `forge.maintain()` for the housekeeping sweep. Everything is safe to run on every replica. This page shows the canonical shapes, from a web-only API up to a multi-replica deploy, and the host-loop pattern the Node and Python bindings use because they lack the managed worker.

## Web-only: just init and serve

The minimum. Build the handle once, hand it to your HTTP framework, serve until a signal. No background tasks at all — fine for an app with no queues and no schedules.

```rust
use anyhow::Result;
use forge::{Forge, ForgeConfig};

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}

#[tokio::main]
async fn main() -> Result<()> {
    let forge = Forge::init(
        ForgeConfig::new(
            std::env::var("FORGE_POSTGRES_URL")
                .unwrap_or_else(|_| "postgres://postgres:forge@127.0.0.1/myapp".into()),
        )
        .with_blob_signing_secret(std::env::var("FORGE_BLOB_SIGNING_SECRET").unwrap())
        .with_blob_base_url("/_forge/blob"),
    )
    .await?;

    // axum 0.8: mount the presigned-blob router where the URLs point.
    let app = axum::Router::new()
        .route("/healthz", axum::routing::get(|| async { "ok" }))
        .nest("/_forge/blob", forge.blob_router()?);

    let listener = tokio::net::TcpListener::bind("0.0.0.0:8080").await?;
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown())
        .await?;
    Ok(())
}
```

`Forge::init` validates the config, connects to Forge's system database, and runs the embedded migrations before returning. Misconfiguration fails here with `ForgeError::Config`, never lazily on first use. `forge.blob_router()` errors with `Config` unless `blob_signing_secret` is set. `Forge` is `Clone` (an `Arc` inside) and `Send + Sync`, so clone it freely into handlers and tasks.

## Web + workers: a managed consumer per queue

`forge.worker(name)` returns a `WorkerBuilder`. Configure concurrency and timeouts, then call `run_until(shutdown, handler)`. The managed worker dequeues up to `concurrency` jobs, auto-heartbeats at roughly a third of the visibility timeout while a handler runs, acks on `Ok(())` / nacks on `Err` or panic, and drains in-flight work on shutdown (bounded by `grace`, default 30s — anything still running is aborted and its lease expires for redelivery).

```rust
use std::time::Duration;
use forge::{Forge, Job};

async fn shutdown() {
    let _ = tokio::signal::ctrl_c().await;
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let forge = Forge::init(/* … */ forge::ForgeConfig::new("postgres://localhost/myapp")).await?;

    // One task per queue. Each handler MUST be idempotent on the job id —
    // the queue is at-least-once.
    let f = forge.clone();
    tokio::spawn(async move {
        f.worker("emails")
            .concurrency(4)
            .visibility_timeout(Duration::from_secs(30))
            .poll_wait(Duration::from_millis(200))
            .run_until(shutdown(), move |job: Job| async move {
                let payload: MyJob = job.payload_json()?;
                send_email(payload).await?;
                Ok::<_, anyhow::Error>(())
            })
            .await;
    });

    // … build and serve your HTTP app here, also under shutdown() …
    Ok(())
}
```

Builder defaults: `concurrency(1)`, `visibility_timeout(30s)`, `poll_wait(20s)` (the SQS maximum), `grace(30s)`. The handler's error type only needs `Display + Send` — return any error and the worker nacks. The plain `run(handler)` variant waits on `shutdown_signal()` (SIGINT, plus SIGTERM on unix) internally; use `run_until` when you control shutdown yourself, e.g. to share one shutdown future across the worker and the HTTP server.

If a handler's lease is lost mid-run (heartbeat hits `Precondition` because another worker claimed it), the worker aborts that handler and lets the new owner settle the job. This is normal under redelivery and is why idempotency is non-negotiable.

## Web + scheduler + maintenance

`schedule` does not deliver jobs; each tick *enqueues* into a queue, which is then consumed by a worker like the one above. Two loops drive the cadence:

- `forge.run_scheduler()` — runs the tick loop until SIGINT/SIGTERM, firing due schedules every ~30s. For tests or a custom cadence, call `run_scheduler_once()` yourself on whatever interval you like.
- `forge.maintain()` — one idempotent housekeeping sweep across every backend: purge expired kv rows and old completed jobs, reclaim leases orphaned by crashed workers, drop stale dedup and rate-limit rows, expire dead sessions, and (filesystem blob) reclaim orphaned files. It is *not* on a built-in timer; call it yourself on a schedule.

`run_scheduler` has no maintenance built in, so the common shape is one task for the scheduler and a separate periodic loop for `maintain`:

```rust
let f = forge.clone();
tokio::spawn(async move { f.run_scheduler().await }); // ~30s ticks, until signal

let f = forge.clone();
tokio::spawn(async move {
    loop {
        if let Err(e) = f.maintain().await {
            tracing::warn!(error = %e, "forge maintain failed");
        }
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(60)) => {}
            _ = shutdown() => break,
        }
    }
});
```

Register schedules once, anywhere you hold the handle, before or after the loop starts (`cron` is an upsert by name, so re-registering on every boot is idempotent):

```rust
forge.schedule()
    .cron("nightly-digest", "0 3 * * *", "digests", b"{}".to_vec().into())
    .await?;
```

If you want one loop instead of two — fire the tick *and* sweep on the same cadence — drive it by hand with `run_scheduler_once` (this is exactly what chatapp's rust-be does in its `housekeeping` task):

```rust
async fn housekeeping(forge: Forge) {
    loop {
        if let Err(e) = forge.run_scheduler_once().await {
            tracing::warn!(error = %e, "scheduler tick failed");
        }
        if let Err(e) = forge.maintain().await {
            tracing::warn!(error = %e, "forge maintain failed");
        }
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(30)) => {}
            _ = shutdown() => break,
        }
    }
}
```

`run_scheduler_once` runs one pass and returns how many jobs it enqueued.

## Multi-replica

Run the exact same binary — workers, scheduler, and maintenance loops included — on every replica. No special "scheduler replica," no flag, no coordinator.

- **Scheduler.** A given tick produces exactly one `enqueue` fleet-wide. Each due row is claimed with `FOR UPDATE SKIP LOCKED` and, in the *same transaction*, the job is inserted into the queue and the row's `next_run` advanced (or the one-shot deleted). A concurrent replica sees the row already claimed and never re-fires it. A crash between claim and enqueue loses nothing — they commit together. So: every replica calls `run_scheduler()`; you still get one enqueue per tick.
- **Workers.** The queue is at-least-once and leased, so N replicas consuming the same queue is the normal case — each job is delivered to one worker at a time, and redelivery on lease expiry is expected. Handlers must be idempotent on the job id regardless of replica count.
- **`maintain()`.** Idempotent; safe to run from every replica concurrently. No harm in overlap.

The one thing one enqueue per tick does *not* buy you is one *delivery* per tick. The enqueued job rides the at-least-once queue, so lease expiry or a worker crash can still hand it to your consumer more than once. This is the queue contract showing through, not a scheduler bug. Idempotent consumers, always.

Filesystem blob backend is the exception to "just scale out": object bytes live on local disk, so multi-replica needs a shared mount (the Postgres blob backend, the default, has no such constraint).

## Node / Python: the host-loop pattern

The Node and Python bindings expose `runSchedulerOnce()` / `run_scheduler_once()` and `maintain()`, but **no managed worker and no `run_scheduler` loop** — there is no binding equivalent of `WorkerBuilder` or the blocking tick loop. The host language owns the loops instead. You write the dequeue/ack/nack cycle and the scheduler tick yourself, and the same multi-replica guarantees hold (the one-enqueue-per-tick claim is enforced in the database, not the loop).

A worker in Node is a plain `while (!stopped())` loop over `queueDequeue` → handler → `queueAck` / `queueNack`:

```ts
async function runFanoutWorker(app: AppCtx, stopped: () => boolean): Promise<void> {
  const VISIBILITY = 30; // seconds
  const WAIT = 1;        // long-poll seconds
  while (!stopped()) {
    let job;
    try {
      job = await app.forge.queueDequeue("fanout", VISIBILITY, WAIT);
    } catch (e) {
      await new Promise((r) => setTimeout(r, 200));
      continue;
    }
    if (!job) continue; // long-poll returned empty
    try {
      await handleFanout(app, job.payload);
      await app.forge.queueAck(job.id);
    } catch {
      try { await app.forge.queueNack(job.id); } catch { /* redelivery is the queue's job */ }
    }
  }
}
```

The scheduler is a timer that calls `runSchedulerOnce()` then `maintain()` each tick — exactly the two methods the managed Rust loop would call for you:

```ts
function runScheduler(app: AppCtx, stopped: () => boolean): void {
  const tick = schedulerMs();
  void (async () => {
    while (!stopped()) {
      try {
        await app.forge.runSchedulerOnce();
        await app.forge.maintain();
      } catch (e) {
        console.warn("scheduler tick failed:", (e as Error).message);
      }
      await new Promise((r) => setTimeout(r, tick));
    }
  })();
}
```

Python is the same shape over asyncio. Workers are coroutines looped on an `asyncio.Event`, and the scheduler loop awaits `run_scheduler_once()` then `maintain()`:

```python
async def scheduler_loop(forge, stop: asyncio.Event, interval: float) -> None:
    while not stop.is_set():
        try:
            await forge.run_scheduler_once()
            await forge.maintain()
        except forge_py.ForgeError:
            pass
        try:
            await asyncio.wait_for(stop.wait(), timeout=interval)
        except TimeoutError:
            pass
```

Binding notes: `forge_py.ForgeClient.connect(postgresUrl, signingSecret?)` and Node's `ForgeClient.connect(postgresUrl, signingSecret?)` mirror `Forge::init` (connect + migrate + ping). Python's `queue_dequeue` returns a `(job_id, payload, attempt)` tuple and `queue_nack(job_id, retry_in_seconds)` takes an explicit backoff; Node's `queueDequeue` returns a job object with `.id` / `.payload`. Neither binding has a blob router method — chatapp's node-be and python-be serve `/_forge/blob` by hand against `blobVerifyPresign` / `blobPutBytes` / `blobGetBytes`. Rust is the canonical surface; the bindings are thin wrappers over the same primitives with the loops moved out into the host.

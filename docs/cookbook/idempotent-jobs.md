# Idempotent job handlers

Forge's queue is **at-least-once**: a job can be delivered more than once after a lease expiry, a worker crash, a heartbeat that lost a race, or a blip between handler success and `ack`. This is not an edge case to defend against later; it is the contract. Forge does not offer exactly-once and does not approximate it. So every handler you write must be safe to run twice. The cleanest way is to key your side effects on the job's stable id (`Job.id`) or a business key, treat redelivery as expected, and make the second run a no-op. This page shows the typed enqueue/dequeue flow, the managed worker, and where the fence token protects you.

## The shape of an idempotent handler

The trick isn't a Forge API; it's the data model. If the side effect is naturally idempotent (a `SET`, an upsert, flipping a `delivered_at` that's already set), you're done. If it isn't (incrementing a counter, charging a card), record that you did it under `job.id` in the same transaction as the effect, and skip on replay.

```rust
use std::time::Duration;
use forge::{Forge, Job, ForgeError};
use forge::typed::{QueueName, QueuePayload, QueueTyped, TypedJob};
use forge::queue::{DequeueOpts, EnqueueOpts};
use serde::{Serialize, Deserialize};

// One payload type per job. The queue name and codec are bound to it once,
// so call sites never pass a queue string or hand-roll JSON.
#[derive(Debug, Serialize, Deserialize)]
struct ChargeInvoice {
    invoice_id: uuid::Uuid,
    amount_cents: i64,
}

impl QueuePayload for ChargeInvoice {
    const QUEUE: QueueName<Self> = QueueName::new("charges");
    const MAX_ATTEMPTS: u32 = 5; // deliveries before dead-lettering; default is 5
}

// --- enqueue side ---------------------------------------------------------

async fn schedule_charge(forge: &Forge, job: ChargeInvoice) -> forge::Result<()> {
    // enqueue_typed serializes the payload and applies the type's default opts
    // (MAX_ATTEMPTS, exponential backoff). dedup_id collapses repeated *enqueues*
    // of the same logical charge within the dedup window (default 5 min) so a
    // double-submit doesn't create two jobs.
    let opts = ChargeInvoice::enqueue_opts()
        .with_dedup_id(format!("charge:{}", job.invoice_id));
    forge.queue().enqueue_typed_with(&job, opts).await?;
    Ok(())
}

// --- consume side ---------------------------------------------------------

async fn handle_charge(pool: &sqlx::PgPool, job: &TypedJob<ChargeInvoice>) -> anyhow::Result<()> {
    let id = job.job().id;                 // the natural idempotency key
    let p = &job.payload;

    let mut tx = pool.begin().await?;

    // Claim this job.id atomically. If the row already exists, a prior delivery
    // ran to completion — this is a duplicate, so do nothing.
    let inserted = sqlx::query_scalar::<_, bool>(
        "INSERT INTO processed_jobs (job_id) VALUES ($1)
         ON CONFLICT (job_id) DO NOTHING RETURNING true",
    )
    .bind(id.to_string())
    .fetch_optional(&mut *tx)
    .await?
    .is_some();

    if !inserted {
        tx.rollback().await?;
        return Ok(()); // already processed; ack and move on
    }

    // The non-idempotent effect and the dedup marker commit together.
    apply_charge(&mut tx, p.invoice_id, p.amount_cents).await?;
    tx.commit().await?;
    Ok(())
}

// --- the managed worker ---------------------------------------------------

async fn run_charges_worker(forge: Forge, pool: sqlx::PgPool, shutdown: impl std::future::Future<Output = ()> + Send) {
    forge
        .worker(ChargeInvoice::QUEUE.as_str())
        .concurrency(8)
        .visibility_timeout(Duration::from_secs(30)) // auto-heartbeats at ~1/3 of this
        .run_until(shutdown, move |job: Job| {
            let pool = pool.clone();
            async move {
                // Decode the raw Job into the typed payload at the boundary.
                let payload: ChargeInvoice = job.payload_json()
                    .map_err(|e| anyhow::anyhow!("bad charge payload: {e}"))?;
                let typed = forge::typed::TypedJob { payload, job }; // illustrative; see note below
                handle_charge(&pool, &typed).await
            }
        })
        .await;
}
```

The worker runs a managed loop: it dequeues up to `concurrency` jobs, auto-heartbeats at roughly `visibility_timeout / 3` while the handler runs, `ack`s on `Ok`, `nack`s on `Err`, and `nack`s on panic (caught at the task boundary so one bad job never crashes the loop). On `run`/`run_until` shutdown it stops dequeuing and drains in-flight handlers within the grace period; anything still running when grace expires is aborted, its lease expires, and it redelivers. You never call `ack`/`nack`/`heartbeat` yourself inside `worker`.

## Two ways to decode the payload

The worker's handler receives a raw `Job`, so inside `worker` you decode with `job.payload_json::<T>()` (as the chatapp example does). The typed `TypedJob<P>` shines when you drive the queue **manually** with `dequeue_typed`, which decodes for you and hands back the lease so you can `ack`/`nack`/`heartbeat` it:

```rust
// Manual consume loop (no managed worker). You own the lease lifecycle.
loop {
    let opts = DequeueOpts::new().with_visibility_timeout(Duration::from_secs(30));
    match forge.queue().dequeue_typed::<ChargeInvoice>(opts).await? {
        Some(job) => {
            match handle_charge(&pool, &job).await {
                Ok(()) => forge.queue().ack(job.job()).await?,
                Err(_) => forge.queue().nack(job.job(), Default::default()).await?,
            }
        }
        None => continue, // long-poll returned nothing within `wait`
    }
}
```

Note: `TypedJob`'s `job` field is private — you build a `TypedJob` only via `dequeue_typed`, and read the lease back out with `.job()` or `.into_parts()`. In the managed-worker snippet above, the `TypedJob { payload, job }` construction is shown for illustration; real worker handlers just hold the `Job` and call `job.payload_json()` directly (see `examples/chatapp/rust-be/src/worker.rs:53`).

## ack / nack / heartbeat and the fence token

- **`ack`** moves the job `leased -> done`. It is idempotent: acking a job whose lease already expired and was reclaimed by another worker returns `Ok(())`, not an error. Crucially, **`ack` does not mean "no one else ran this."** A duplicate may already be running or finished elsewhere. That's exactly why the handler must be idempotent — `ack` is the at-least-once seam, not a mutual-exclusion lock.
- **`nack`** marks the current delivery failed. `NackOpts::default()` retries immediately; `NackOpts::retry_in(d)` delays the redelivery. The redelivery increments `attempt`; when the incremented count reaches `max_attempts`, the job goes to `"<queue>.dlq"` instead of back to the queue. Nothing is silently dropped.
- **`heartbeat`** extends the lease by another `visibility_timeout`. Each lease carries a per-delivery **fence token**. If your lease was already lost (expired and reclaimed by another worker), `heartbeat` returns `ForgeError::Precondition` — stop work on this job immediately, because another worker now owns it. The managed worker handles this for you: on a `Precondition` heartbeat it aborts the handler and abandons the job to its new owner. `ack`/`nack` from a worker whose fence is stale become no-ops, so a slow handler that wakes up after losing its lease can't corrupt the row the new owner is processing.

## Gotchas

- **`attempt` increments on redelivery, never on claim.** First delivery is `attempt == 1` (at rest the row counts 0 deliveries). A job is dead-lettered once a delivery fails and the incremented count reaches `max_attempts` (default 5). If you branch on `attempt` for "last try" logic, remember the increment happens on the *redelivery*, not the current run.
- **`dedup_id` dedupes enqueues, not deliveries.** It collapses repeated sends of the same `(queue, dedup_id)` within the window and returns the existing `JobId` (a success, not an error). It does nothing about a single job being delivered twice. Idempotent consumers are still mandatory. Dedup is scoped per queue — the same id in two queues is two jobs. Max length is 128 chars.
- **No ordering.** Jobs are not delivered in enqueue order. Don't encode "this runs after that" assumptions.
- **Keep the visibility timeout above your handler's worst case**, or pick a `visibility_timeout` the auto-heartbeat can cover. A handler that outruns its lease without heartbeating will see its job redelivered while it's still running — the idempotency key is what saves you there.
- **Payloads cap at 256 KiB**; over that is a `ForgeError::Limit`. Pass an id and re-read the row, don't ship the whole record.

## Node / Python bindings

The bindings expose the same at-least-once queue but **not** the typed `QueuePayload` layer or the managed `worker()` builder — those are Rust-only. You write the consume loop yourself and key idempotency on the job id exactly the same way.

- **Node** (`bindings/forge-node/index.d.ts`): `queueEnqueue(queue, payload, maxAttempts?, dedupId?)`, `queueDequeue(queue, visibilitySeconds, waitSeconds)` returns a flat `JsJob` (`{ id, payload, attempt, ... }`) or `null`. Settle by id: `queueAck(id)`, `queueNack(id, retrySeconds?)`, `queueHeartbeat(id)`. Payloads are strings — `JSON.stringify`/`parse` yourself.
- **Python** (`forge_py`): `queue_dequeue(queue, visibility_seconds, wait_seconds)` returns a `(job_id, payload, attempt)` tuple or `None`; `queue_enqueue`, `queue_ack(job_id)`, `queue_nack(job_id, retry_seconds?)`, `queue_heartbeat(job_id)`. See `examples/chatapp/python-be/app/workers.py` for the dequeue-decode-ack/nack loop pattern, including acking on the "already gone" path so a duplicate reaps to a no-op.

In both bindings there's no auto-heartbeat, so either keep handlers well under the visibility timeout or call the heartbeat function yourself on a timer.

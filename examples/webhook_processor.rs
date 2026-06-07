//! Dogfooding example: a webhook-ingesting background processor.
//!
//! A real, tiny end-to-end app that needs BOTH v0.1 primitives:
//!
//! - `kv` — idempotency gate (SET NX), processed counters (INCR)
//! - `queue` — async processing with a managed worker, ack/nack, dead-letter queue
//!
//! Plus migrations + automatic tracing/metrics, all from `Forge::init`.
//!
//! Run it:
//!   docker compose up -d db
//!   FORGE_POSTGRES_URL=postgres://postgres:forge@localhost:5432/forge_dev \
//!     cargo run --example webhook_processor

use forge::{Bytes, DequeueOpts, EnqueueOpts, Forge, ForgeConfig, QueueExt, SetMode, SetOpts};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[derive(serde::Serialize, serde::Deserialize)]
struct Event {
    id: String,
    kind: String,
}

#[tokio::main]
async fn main() -> forge::Result<()> {
    // Set RUST_LOG=forge=debug to watch every kv/queue op.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    tracing_subscriber::fmt().with_env_filter(filter).init();

    let url = std::env::var("FORGE_POSTGRES_URL")
        .unwrap_or_else(|_| "postgres://postgres:forge@localhost:5432/forge_dev".to_string());
    let forge = Forge::init(ForgeConfig::new(url)).await?;

    // Unique names so repeated runs are independent.
    let run = unique_token();
    let queue = format!("webhooks_{run}");
    let seen_prefix = format!("seen:{run}:");
    let total_key = format!("processed:{run}:total");

    // 3 distinct good events, 1 duplicate of evt-1, 1 "explode" that always fails.
    let incoming = [
        ("evt-1", "signup"),
        ("evt-2", "payment"),
        ("evt-1", "signup"), // duplicate delivery
        ("evt-3", "signup"),
        ("evt-bad", "explode"),
    ];

    let mut enqueued = 0usize;
    let mut deduped = 0usize;
    for (id, kind) in incoming {
        // Idempotency gate: SET NX returns false on a repeat delivery, dropping it before the queue.
        let fresh = forge
            .kv()
            .set(
                &format!("{seen_prefix}{id}"),
                Bytes::from_static(b"1"),
                SetOpts::new()
                    .with_mode(SetMode::IfNotExists)
                    .with_ttl(Duration::from_secs(3600)),
            )
            .await?;
        if !fresh {
            deduped += 1;
            continue;
        }

        let event = Event {
            id: id.to_string(),
            kind: kind.to_string(),
        };
        // max_attempts = 1 makes the exploding event dead-letter on its first failure.
        let max_attempts = if kind == "explode" { 1 } else { 5 };
        forge
            .queue()
            .enqueue_json(
                &queue,
                &event,
                EnqueueOpts::new().with_max_attempts(max_attempts),
            )
            .await?;
        enqueued += 1;
    }
    println!(
        "ingested {} webhooks: {enqueued} enqueued, {deduped} deduped",
        incoming.len()
    );

    let processed = Arc::new(AtomicUsize::new(0));
    let failed = Arc::new(AtomicUsize::new(0));
    const EXPECTED_GOOD: usize = 3;
    const EXPECTED_BAD: usize = 1;

    // Stop once every event has reached a terminal state (acked or dead-lettered).
    let shutdown = {
        let processed = Arc::clone(&processed);
        let failed = Arc::clone(&failed);
        async move {
            for _ in 0..400 {
                if processed.load(Ordering::SeqCst) >= EXPECTED_GOOD
                    && failed.load(Ordering::SeqCst) >= EXPECTED_BAD
                {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        }
    };

    {
        let f = forge.clone();
        let total_key = total_key.clone();
        let run = run.clone();
        let processed = Arc::clone(&processed);
        let failed = Arc::clone(&failed);
        forge
            .worker(&queue)
            .concurrency(4)
            .poll_wait(Duration::from_millis(200))
            .run_until(shutdown, move |job| {
                let f = f.clone();
                let total_key = total_key.clone();
                let run = run.clone();
                let processed = Arc::clone(&processed);
                let failed = Arc::clone(&failed);
                async move {
                    let event: Event = job.payload_json().map_err(|e| e.to_string())?;
                    if event.kind == "explode" {
                        failed.fetch_add(1, Ordering::SeqCst);
                        return Err(format!("event {} exploded", event.id));
                    }
                    f.kv()
                        .incr(&total_key, 1)
                        .await
                        .map_err(|e| e.to_string())?;
                    f.kv()
                        .incr(&format!("processed:{run}:{}", event.kind), 1)
                        .await
                        .map_err(|e| e.to_string())?;
                    processed.fetch_add(1, Ordering::SeqCst);
                    Ok::<(), String>(())
                }
            })
            .await;
    }

    // `incr(key, 0)` reads a counter (INCRBY 0).
    let total = forge.kv().incr(&total_key, 0).await?;
    let signups = forge
        .kv()
        .incr(&format!("processed:{run}:signup"), 0)
        .await?;

    let mut dead = 0usize;
    while let Some(job) = forge
        .queue()
        .dequeue(
            &format!("{queue}.dlq"),
            DequeueOpts::new().with_wait(Duration::ZERO),
        )
        .await?
    {
        dead += 1;
        forge.queue().ack(&job).await?;
    }

    println!(
        "processed total={total} (signups={signups}), dead-lettered={dead}, deduped={deduped}"
    );

    assert_eq!(total, 3, "3 good events each processed exactly once");
    assert_eq!(signups, 2, "evt-1 (once, despite the duplicate) + evt-3");
    assert_eq!(dead, 1, "the exploding event landed in the DLQ, not lost");
    assert_eq!(
        deduped, 1,
        "the duplicate delivery was stopped at the kv NX gate"
    );
    println!("OK — kv + queue worked end to end");
    Ok(())
}

/// A short unique-ish token so reruns don't share keys/queues; avoids a uuid dependency.
fn unique_token() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("{nanos:x}")
}

//! queue contract tests. Run with: `cargo test --features pg-tests` (needs TEST_DATABASE_URL).
#![cfg(feature = "pg-tests")]
#![allow(clippy::unwrap_used, clippy::panic)]

use bytes::Bytes;
use forge::testing::TestDatabase;
use forge::{DequeueOpts, EnqueueOpts, ForgeError, JobId, NackOpts};
use std::collections::HashSet;
use std::time::Duration;

fn payload(s: &str) -> Bytes {
    Bytes::from(s.as_bytes().to_vec())
}

fn vis(secs: u64) -> DequeueOpts {
    DequeueOpts::new()
        .with_wait(Duration::ZERO)
        .with_visibility_timeout(Duration::from_secs(secs))
}

#[tokio::test]
async fn enqueue_dequeue_ack_roundtrip() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();

    let id = q
        .enqueue("emails", payload("hi"), EnqueueOpts::new())
        .await
        .unwrap();
    let job = q.dequeue("emails", vis(30)).await.unwrap().expect("a job");
    assert_eq!(job.id, id);
    assert_eq!(job.payload, payload("hi"));
    assert_eq!(job.attempt, 1, "first delivery is attempt 1");

    q.ack(&job).await.unwrap();
    assert!(
        q.dequeue("emails", vis(30)).await.unwrap().is_none(),
        "acked job is gone"
    );
}

#[tokio::test]
async fn leased_job_is_invisible_to_other_dequeues() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();

    q.enqueue("q", payload("x"), EnqueueOpts::new())
        .await
        .unwrap();
    let _job = q.dequeue("q", vis(60)).await.unwrap().expect("claimed");
    assert!(
        q.dequeue("q", vis(60)).await.unwrap().is_none(),
        "leased => invisible"
    );
}

#[tokio::test]
async fn lease_expiry_redelivers_and_increments_attempt() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();

    q.enqueue("q", payload("x"), EnqueueOpts::new())
        .await
        .unwrap();
    let first = q
        .dequeue("q", vis(1))
        .await
        .unwrap()
        .expect("first delivery");
    assert_eq!(first.attempt, 1);

    // Let the lease expire (simulating a crashed worker), then redeliver.
    tokio::time::sleep(Duration::from_millis(1200)).await;
    let second = q.dequeue("q", vis(30)).await.unwrap().expect("redelivery");
    assert_eq!(second.id, first.id);
    assert_eq!(second.attempt, 2, "redelivery increments the attempt");

    // Stale handle from the crashed worker can no longer mutate the job.
    assert!(matches!(
        q.heartbeat(&first).await,
        Err(ForgeError::Precondition(_))
    ));
    // Acking the stale handle is an idempotent no-op.
    q.ack(&first).await.unwrap();
    q.ack(&second).await.unwrap();
    assert!(q.dequeue("q", vis(30)).await.unwrap().is_none());
}

#[tokio::test]
async fn nack_returns_job_for_redelivery() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();

    q.enqueue("q", payload("x"), EnqueueOpts::new())
        .await
        .unwrap();
    let job = q.dequeue("q", vis(30)).await.unwrap().unwrap();
    // Immediate retry so the test doesn't wait out a backoff.
    q.nack(&job, NackOpts::retry_in(Duration::ZERO))
        .await
        .unwrap();

    let again = q.dequeue("q", vis(30)).await.unwrap().expect("redelivered");
    assert_eq!(again.id, job.id);
    assert_eq!(again.attempt, 2);
}

#[tokio::test]
async fn exhausted_job_moves_to_dead_letter_queue() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();

    // max_attempts = 1: one delivery, then dead-letter on failure.
    let id = q
        .enqueue("q", payload("x"), EnqueueOpts::new().with_max_attempts(1))
        .await
        .unwrap();
    let job = q.dequeue("q", vis(30)).await.unwrap().unwrap();
    q.nack(&job, NackOpts::retry_in(Duration::ZERO))
        .await
        .unwrap();

    assert!(
        q.dequeue("q", vis(30)).await.unwrap().is_none(),
        "exhausted job left the source queue"
    );
    let dead = q
        .dequeue("q.dlq", vis(30))
        .await
        .unwrap()
        .expect("job in DLQ");
    assert_eq!(dead.id, id);
    assert_eq!(dead.attempt, 1, "DLQ job is a fresh available job");
}

#[tokio::test]
async fn dedup_collapses_enqueues_within_window() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();

    let opts = || EnqueueOpts::new().with_dedup_id("order-42");
    let id1 = q.enqueue("q", payload("a"), opts()).await.unwrap();
    let id2 = q.enqueue("q", payload("b"), opts()).await.unwrap();
    assert_eq!(id1, id2, "same dedup_id within window => same job id");

    let job = q.dequeue("q", vis(30)).await.unwrap().expect("one job");
    assert_eq!(job.id, id1);
    q.ack(&job).await.unwrap();
    assert!(
        q.dequeue("q", vis(30)).await.unwrap().is_none(),
        "only one job was enqueued"
    );

    // Same dedup_id in a different queue is independent.
    let other = q.enqueue("other", payload("c"), opts()).await.unwrap();
    assert_ne!(other, id1, "dedup is scoped per queue");
}

#[tokio::test]
async fn unknown_job_id_is_not_found() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();

    q.enqueue("q", payload("x"), EnqueueOpts::new())
        .await
        .unwrap();
    let job = q.dequeue("q", vis(30)).await.unwrap().unwrap();
    q.ack(&job).await.unwrap();
    // Row still exists (status=done) but the lease is gone => Precondition, not NotFound.
    assert!(matches!(
        q.heartbeat(&job).await,
        Err(ForgeError::Precondition(_))
    ));
}

#[test]
fn job_ids_are_unique() {
    assert_ne!(JobId::new(), JobId::new());
}

#[tokio::test]
async fn validates_queue_names_and_limits() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let q = forge.queue();

    assert!(matches!(
        q.enqueue("bad name", payload("x"), EnqueueOpts::new())
            .await,
        Err(ForgeError::Invalid(_))
    ));
    assert!(
        matches!(
            q.enqueue("q.dlq", payload("x"), EnqueueOpts::new()).await,
            Err(ForgeError::Invalid(_)),
        ),
        "enqueue to a .dlq name is reserved"
    );

    let too_big = Bytes::from(vec![0u8; 256 * 1024 + 1]);
    assert!(matches!(
        q.enqueue("q", too_big, EnqueueOpts::new()).await,
        Err(ForgeError::Limit(_))
    ));
}

/// N concurrent consumers each get a distinct job, exercising `FOR UPDATE SKIP LOCKED`.
#[tokio::test]
async fn concurrent_consumers_never_double_deliver() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();

    const N: usize = 60;
    for i in 0..N {
        forge
            .queue()
            .enqueue("work", payload(&format!("job-{i}")), EnqueueOpts::new())
            .await
            .unwrap();
    }

    let mut tasks = tokio::task::JoinSet::new();
    for _ in 0..8 {
        let f = forge.clone();
        tasks.spawn(async move {
            let mut mine = Vec::new();
            // Long lease, never ack: a claimed job stays invisible, so None means drained.
            while let Some(job) = f.queue().dequeue("work", vis(120)).await.unwrap() {
                mine.push(job.id);
            }
            mine
        });
    }

    let mut all = Vec::new();
    while let Some(res) = tasks.join_next().await {
        all.extend(res.unwrap());
    }

    let unique: HashSet<_> = all.iter().copied().collect();
    assert_eq!(
        all.len(),
        unique.len(),
        "a job was delivered to more than one consumer"
    );
    assert_eq!(unique.len(), N, "every job was delivered exactly once");
}

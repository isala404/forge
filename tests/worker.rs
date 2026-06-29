#![cfg(feature = "pg-tests")]
#![allow(clippy::unwrap_used, clippy::panic)]

use bytes::Bytes;
use forgelib::EnqueueOpts;
use forgelib::testing::TestDatabase;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::Duration;

#[tokio::test]
async fn worker_processes_jobs_and_acks_them() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();

    for i in 0..5 {
        forge
            .queue()
            .enqueue("jobs", Bytes::from(format!("n{i}")), EnqueueOpts::new())
            .await
            .unwrap();
    }

    let processed = Arc::new(AtomicUsize::new(0));
    let p = Arc::clone(&processed);
    let worker = forge
        .worker("jobs")
        .concurrency(3)
        .poll_wait(Duration::from_millis(100));

    // Shut down once all five have been handled (or after a safety timeout).
    let p2 = Arc::clone(&processed);
    let shutdown = async move {
        for _ in 0..100 {
            if p2.load(Ordering::SeqCst) >= 5 {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    };

    worker
        .run_until(shutdown, move |_job| {
            let p = Arc::clone(&p);
            async move {
                p.fetch_add(1, Ordering::SeqCst);
                Ok::<(), String>(())
            }
        })
        .await;

    assert_eq!(processed.load(Ordering::SeqCst), 5, "all jobs handled");
    assert!(
        forge
            .queue()
            .dequeue("jobs", forgelib::DequeueOpts::new().with_wait(Duration::ZERO))
            .await
            .unwrap()
            .is_none()
    );
}

/// A hung handler must not block shutdown forever: drain is bounded by grace, then aborted.
#[tokio::test]
async fn shutdown_drain_is_bounded_by_grace() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    forge
        .queue()
        .enqueue("slow", Bytes::from_static(b"x"), EnqueueOpts::new())
        .await
        .unwrap();

    let worker = forge
        .worker("slow")
        .concurrency(1)
        .poll_wait(Duration::from_millis(100))
        .grace(Duration::from_millis(500));

    // Fire shutdown after the job is surely in-flight; the handler then hangs.
    let shutdown = tokio::time::sleep(Duration::from_millis(300));
    let run = worker.run_until(shutdown, |_job| async move {
        tokio::time::sleep(Duration::from_secs(60)).await;
        Ok::<(), String>(())
    });

    // Without the bound this would block ~60s; with it, ~0.3s wait + 0.5s grace.
    let result = tokio::time::timeout(Duration::from_secs(5), run).await;
    assert!(result.is_ok(), "worker drain hung past the grace period");
}

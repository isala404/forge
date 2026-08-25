#![allow(clippy::panic, clippy::unwrap_used)]

use std::time::Duration;

use forgelib::{Bytes, EnqueueOpts, Forge, ForgeError, ProbeOptions, RuntimeMode, SetOpts};
use futures_util::StreamExt;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

const MEMORY: &str = "[forge]\nmode = \"memory\"\nenvironment = \"test\"\n";

fn assert_code<T>(result: Result<T, ForgeError>, expected: &str) {
    match result {
        Err(error) => assert_eq!(error.code(), expected),
        Ok(_) => panic!("expected {expected} error"),
    }
}

#[tokio::test]
async fn memory_profile_is_database_free_and_complete() {
    let forge = Forge::init_from_str(MEMORY).await.unwrap();
    assert_eq!(forge.mode(), RuntimeMode::Memory);
    assert_code(forge.pool(), "NOT_CONFIGURED");
    assert_code(forge.postgres_url(), "NOT_CONFIGURED");

    forge
        .kv()
        .set("key", Bytes::from_static(b"value"), SetOpts::new())
        .await
        .unwrap();
    assert_eq!(
        forge.kv().get("key").await.unwrap(),
        Some(Bytes::from_static(b"value"))
    );
}

#[tokio::test]
async fn close_is_idempotent_rejects_work_and_ends_subscriptions() {
    let forge = Forge::init_from_str(MEMORY).await.unwrap();
    let mut subscription = forge.pubsub().subscribe("events").await.unwrap();

    forge.close(Duration::from_secs(1)).await.unwrap();
    forge.close(Duration::from_secs(1)).await.unwrap();

    assert_code(forge.kv().get("after-close").await, "PRECONDITION");
    assert!(subscription.next().await.is_none());
}

#[tokio::test]
async fn close_stops_dequeueing_and_drains_managed_workers() {
    let forge = Forge::init_from_str(MEMORY).await.unwrap();
    forge
        .queue()
        .enqueue("jobs", Bytes::from_static(b"payload"), EnqueueOpts::new())
        .await
        .unwrap();

    let started = Arc::new(tokio::sync::Notify::new());
    let handled = Arc::new(AtomicBool::new(false));
    let worker = {
        let started = Arc::clone(&started);
        let handled = Arc::clone(&handled);
        let forge = forge.clone();
        tokio::spawn(async move {
            forge
                .worker("jobs")
                .poll_wait(Duration::from_millis(10))
                .run(move |_job| {
                    let started = Arc::clone(&started);
                    let handled = Arc::clone(&handled);
                    async move {
                        started.notify_one();
                        tokio::time::sleep(Duration::from_millis(50)).await;
                        handled.store(true, Ordering::Release);
                        Ok::<(), String>(())
                    }
                })
                .await;
        })
    };

    tokio::time::timeout(Duration::from_secs(1), started.notified())
        .await
        .unwrap();
    forge.close(Duration::from_secs(1)).await.unwrap();
    tokio::time::timeout(Duration::from_secs(1), worker)
        .await
        .unwrap()
        .unwrap();
    assert!(handled.load(Ordering::Acquire));
}

#[tokio::test]
async fn production_memory_requires_an_explicit_gate() {
    let result =
        Forge::init_from_str("[forge]\nmode = \"memory\"\nenvironment = \"production\"\n").await;
    assert_code(result, "CONFIG");

    let unsafe_forge = Forge::init_from_str(
        "[forge]\nmode = \"memory\"\nenvironment = \"production\"\nallow_memory_in_production = true\n",
    )
    .await
    .unwrap();
    let diagnostics = unsafe_forge
        .diagnostics(Duration::from_secs(1))
        .await
        .unwrap();
    assert!(!diagnostics.ready);
    assert!(
        diagnostics
            .checks
            .iter()
            .any(|check| { check.name == "unsafe_production_settings" && check.status == "fail" })
    );
}

#[tokio::test]
async fn health_and_metrics_are_per_instance_and_redacted() {
    let first = Forge::init_from_str(MEMORY).await.unwrap();
    let second = Forge::init_from_str(MEMORY).await.unwrap();
    first.kv().get("private-user-key").await.unwrap();

    let first_text = first.render_prometheus();
    let second_text = second.render_prometheus();
    assert!(first_text.contains("forge_operations_total"));
    assert!(!first_text.contains("private-user-key"));
    assert!(!second_text.contains("forge_operations_total"));

    let health = first.probe(ProbeOptions::new()).await.unwrap();
    assert!(health.live);
    assert!(health.ready);
    assert_eq!(health.backends.len(), 8);
    assert!(
        health
            .backends
            .iter()
            .all(|backend| backend.status == "healthy")
    );
    let diagnostics = first.diagnostics(Duration::from_secs(1)).await.unwrap();
    assert!(diagnostics.ready);
    assert_eq!(diagnostics.checks.len(), 7);

    first.close(Duration::from_secs(1)).await.unwrap();
    assert!(!first.is_live());
}

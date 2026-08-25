#![allow(clippy::unwrap_used)]

use forgelib::{Bytes, DequeueOpts, EnqueueOpts, Forge, Limit, SessionOpts, SetOpts};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MEMORY: &str = "[forge]\nmode = \"memory\"\nenvironment = \"test\"\n";

#[tokio::test]
async fn manual_clock_drives_expiry_backoff_scheduling_and_refill() {
    let forge =
        Forge::init_memory_for_testing(MEMORY, UNIX_EPOCH + Duration::from_secs(1_700_000_000), 7)
            .await
            .unwrap();

    forge
        .kv()
        .set(
            "ttl",
            Bytes::from_static(b"value"),
            SetOpts::new().with_ttl(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    let session = forge
        .auth()
        .create_session(
            "user",
            SessionOpts::new()
                .with_idle_timeout(Duration::from_secs(10))
                .with_absolute_timeout(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    let job = forge
        .queue()
        .enqueue(
            "jobs",
            Bytes::new(),
            EnqueueOpts::new().with_delay(Duration::from_secs(10)),
        )
        .await
        .unwrap();
    forge
        .schedule()
        .at(
            SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_010),
            "scheduled",
            Bytes::new(),
            Default::default(),
        )
        .await
        .unwrap();
    let limit = Limit::per_duration(1, Duration::from_secs(10));
    assert!(
        forge
            .ratelimit()
            .check("api", "user", limit)
            .await
            .unwrap()
            .allowed
    );
    assert!(
        !forge
            .ratelimit()
            .check("api", "user", limit)
            .await
            .unwrap()
            .allowed
    );

    forge.advance_test_clock(Duration::from_secs(10)).unwrap();

    assert!(forge.kv().get("ttl").await.unwrap().is_none());
    assert!(
        forge
            .auth()
            .validate_session(session.as_str())
            .await
            .unwrap()
            .is_none()
    );
    assert_eq!(
        forge
            .queue()
            .dequeue("jobs", DequeueOpts::new().with_wait(Duration::ZERO))
            .await
            .unwrap()
            .map(|value| value.id),
        Some(job)
    );
    assert_eq!(forge.run_scheduler_once().await.unwrap(), 1);
    assert!(
        forge
            .ratelimit()
            .check("api", "user", limit)
            .await
            .unwrap()
            .allowed
    );
}

#[tokio::test]
async fn seeded_token_entropy_is_repeatable_per_factory() {
    let first = Forge::init_memory_for_testing(MEMORY, UNIX_EPOCH, 42)
        .await
        .unwrap();
    let second = Forge::init_memory_for_testing(MEMORY, UNIX_EPOCH, 42)
        .await
        .unwrap();
    let options = SessionOpts::new();
    let a = first.auth().create_session("user", options).await.unwrap();
    let b = second.auth().create_session("user", options).await.unwrap();
    assert_eq!(a.as_str(), b.as_str());
}

#[tokio::test]
async fn ordinary_clients_reject_test_clock_mutation() {
    let forge = forgelib::Forge::init_from_str(MEMORY).await.unwrap();
    let error = forge
        .advance_test_clock(Duration::from_secs(1))
        .unwrap_err();
    assert_eq!(error.code(), "PRECONDITION");
}

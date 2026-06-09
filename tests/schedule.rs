//! schedule contract tests. Run with: `cargo test --features pg-tests` (needs TEST_DATABASE_URL).
#![cfg(feature = "pg-tests")]
#![allow(clippy::unwrap_used, clippy::panic)]

use forge::testing::TestDatabase;
use forge::{Bytes, DequeueOpts, ForgeError, ScheduleKind};
use std::time::{Duration, SystemTime};

#[tokio::test]
async fn at_fires_a_due_one_shot_into_the_queue() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();

    let id = forge
        .schedule()
        .at(SystemTime::now(), "reports", Bytes::from_static(b"r1"))
        .await
        .unwrap();

    assert_eq!(forge.run_scheduler_once().await.unwrap(), 1);

    // The enqueued job carries the JobId that `at` returned.
    let job = forge
        .queue()
        .dequeue("reports", DequeueOpts::new().with_wait(Duration::ZERO))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.id, id);
    assert_eq!(job.payload, Bytes::from_static(b"r1"));
    forge.queue().ack(&job).await.unwrap();

    // The one-shot is consumed.
    assert!(forge.schedule().list().await.unwrap().is_empty());
}

#[tokio::test]
async fn future_at_does_not_fire_yet() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();

    forge
        .schedule()
        .at(
            SystemTime::now() + Duration::from_secs(3600),
            "later",
            Bytes::from_static(b"x"),
        )
        .await
        .unwrap();

    assert_eq!(forge.run_scheduler_once().await.unwrap(), 0);
    let list = forge.schedule().list().await.unwrap();
    assert_eq!(list.len(), 1);
    assert!(matches!(
        list.first().map(|s| &s.kind),
        Some(ScheduleKind::At)
    ));
}

#[tokio::test]
async fn cron_upserts_lists_and_cancels() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let s = forge.schedule();

    s.cron("nightly", "0 0 * * *", "reports", Bytes::from_static(b"c"))
        .await
        .unwrap();
    let list = s.list().await.unwrap();
    assert_eq!(list.len(), 1);
    assert!(matches!(
        list.first().map(|i| &i.kind),
        Some(ScheduleKind::Cron(_))
    ));

    // Re-registering the same name upserts (still one).
    s.cron(
        "nightly",
        "30 0 * * *",
        "reports2",
        Bytes::from_static(b"c"),
    )
    .await
    .unwrap();
    assert_eq!(s.list().await.unwrap().len(), 1);

    assert!(s.cancel("nightly").await.unwrap());
    assert!(!s.cancel("nightly").await.unwrap());
    assert!(s.list().await.unwrap().is_empty());
}

#[tokio::test]
async fn concurrent_ticks_fire_each_schedule_once() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();

    for i in 0..5 {
        forge
            .schedule()
            .at(SystemTime::now(), "burst", Bytes::from(format!("j{i}")))
            .await
            .unwrap();
    }

    let f1 = forge.clone();
    let f2 = forge.clone();
    let (a, b) = tokio::join!(f1.run_scheduler_once(), f2.run_scheduler_once());
    assert_eq!(
        a.unwrap() + b.unwrap(),
        5,
        "each due schedule fires exactly once across concurrent ticks"
    );
}

#[tokio::test]
async fn invalid_cron_and_names_error() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let s = forge.schedule();
    let p = Bytes::from_static(b"x");

    assert!(matches!(
        s.cron("bad", "not a cron", "q", p.clone()).await,
        Err(ForgeError::Invalid(_))
    ));
    assert!(matches!(
        s.cron("", "* * * * *", "q", p.clone()).await,
        Err(ForgeError::Invalid(_))
    ));
    assert!(matches!(
        s.cron("ok", "* * * * *", "bad queue", p).await,
        Err(ForgeError::Invalid(_))
    ));
}

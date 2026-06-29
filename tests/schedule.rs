#![cfg(feature = "pg-tests")]
#![allow(clippy::unwrap_used, clippy::panic)]

use forgelib::testing::TestDatabase;
use forgelib::{Bytes, DequeueOpts, ForgeError, ScheduleKind, ScheduleOpts};
use std::time::{Duration, SystemTime};

#[tokio::test]
async fn at_fires_a_due_one_shot_into_the_queue() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();

    let id = forge
        .schedule()
        .at(
            SystemTime::now() - Duration::from_secs(2),
            "reports",
            Bytes::from_static(b"r1"),
            ScheduleOpts::new(),
        )
        .await
        .unwrap();

    assert_eq!(forge.run_scheduler_once().await.unwrap(), 1);

    let job = forge
        .queue()
        .dequeue("reports", DequeueOpts::new().with_wait(Duration::ZERO))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.id, id);
    assert_eq!(job.payload, Bytes::from_static(b"r1"));
    forge.queue().ack(&job).await.unwrap();

    assert!(forge.schedule().list(None, 100).await.unwrap().0.is_empty());
}

#[tokio::test]
async fn fast_cron_far_behind_still_fires_its_most_recent_tick() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();

    forge
        .schedule()
        .cron(
            "heartbeat",
            "* * * * *",
            "beats",
            Bytes::from_static(b"b"),
            ScheduleOpts::new(),
        )
        .await
        .unwrap();

    // Simulate a long outage: the stored tick is ~90 minutes stale (well past the 1h
    // grace). The most-recent missed tick is still within grace, so recovery must fire
    // exactly one job, not skip the schedule wholesale. (pg-relative time, so the
    // Docker VM clock skew can't flake this.)
    db.execute_raw(
        "UPDATE forge_schedules SET next_run = now() - interval '90 minutes' \
         WHERE name = 'heartbeat'",
    )
    .await
    .unwrap();

    assert_eq!(forge.run_scheduler_once().await.unwrap(), 1);

    let job = forge
        .queue()
        .dequeue("beats", DequeueOpts::new().with_wait(Duration::ZERO))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.payload, Bytes::from_static(b"b"));
    forge.queue().ack(&job).await.unwrap();
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
            ScheduleOpts::new(),
        )
        .await
        .unwrap();

    assert_eq!(forge.run_scheduler_once().await.unwrap(), 0);
    let (list, _) = forge.schedule().list(None, 100).await.unwrap();
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

    s.cron(
        "nightly",
        "0 0 * * *",
        "reports",
        Bytes::from_static(b"c"),
        ScheduleOpts::new(),
    )
    .await
    .unwrap();
    let (list, _) = s.list(None, 100).await.unwrap();
    assert_eq!(list.len(), 1);
    assert!(matches!(
        list.first().map(|i| &i.kind),
        Some(ScheduleKind::Cron(_))
    ));

    s.cron(
        "nightly",
        "30 0 * * *",
        "reports2",
        Bytes::from_static(b"c"),
        ScheduleOpts::new(),
    )
    .await
    .unwrap();
    assert_eq!(s.list(None, 100).await.unwrap().0.len(), 1);

    assert!(s.cancel("nightly").await.unwrap());
    assert!(!s.cancel("nightly").await.unwrap());
    assert!(s.list(None, 100).await.unwrap().0.is_empty());
}

#[tokio::test]
async fn concurrent_ticks_fire_each_schedule_once() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();

    for i in 0..5 {
        forge
            .schedule()
            .at(
                SystemTime::now() - Duration::from_secs(2),
                "burst",
                Bytes::from(format!("j{i}")),
                ScheduleOpts::new(),
            )
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
async fn scheduler_ticks_are_scoped_to_namespace() {
    let db = TestDatabase::new().await.unwrap();
    let app_a = db.forge_with("[forge]\nnamespace = \"app_a\"\n").await.unwrap();
    let app_b = db.forge_with("[forge]\nnamespace = \"app_b\"\n").await.unwrap();

    let id = app_a
        .schedule()
        .at(
            SystemTime::now() - Duration::from_secs(2),
            "jobs",
            Bytes::from_static(b"a"),
            ScheduleOpts::new(),
        )
        .await
        .unwrap();

    assert_eq!(
        app_b.run_scheduler_once().await.unwrap(),
        0,
        "app_b must not fire app_a schedules"
    );
    assert_eq!(app_a.run_scheduler_once().await.unwrap(), 1);
    let job = app_a
        .queue()
        .dequeue("jobs", DequeueOpts::new().with_wait(Duration::ZERO))
        .await
        .unwrap()
        .expect("app_a's scheduler fires app_a's job");
    assert_eq!(job.id, id);
}

#[tokio::test]
async fn at_past_is_allowed_but_far_future_is_limited() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();

    // A `when` in the past is accepted and fires on the next tick (within grace).
    let id = forge
        .schedule()
        .at(
            SystemTime::now() - Duration::from_secs(60),
            "past",
            Bytes::from_static(b"p"),
            ScheduleOpts::new(),
        )
        .await
        .unwrap();
    assert_eq!(forge.run_scheduler_once().await.unwrap(), 1);
    let job = forge
        .queue()
        .dequeue("past", DequeueOpts::new().with_wait(Duration::ZERO))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.id, id);
    forge.queue().ack(&job).await.unwrap();

    // A `when` past the ~100-year ceiling is `Limit`.
    let far = SystemTime::now() + Duration::from_secs(200 * 365 * 24 * 60 * 60);
    assert!(matches!(
        forge
            .schedule()
            .at(far, "later", Bytes::from_static(b"x"), ScheduleOpts::new())
            .await,
        Err(ForgeError::Limit(_))
    ));
}

#[tokio::test]
async fn invalid_cron_and_names_error() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let s = forge.schedule();
    let p = Bytes::from_static(b"x");

    assert!(matches!(
        s.cron("bad", "not a cron", "q", p.clone(), ScheduleOpts::new())
            .await,
        Err(ForgeError::Invalid(_))
    ));
    assert!(matches!(
        s.cron("", "* * * * *", "q", p.clone(), ScheduleOpts::new())
            .await,
        Err(ForgeError::Invalid(_))
    ));
    assert!(matches!(
        s.cron("ok", "* * * * *", "bad queue", p, ScheduleOpts::new())
            .await,
        Err(ForgeError::Invalid(_))
    ));
}

#[tokio::test]
async fn schedule_opts_carry_to_the_enqueued_job() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();

    // A one-attempt scheduled job: the opts must reach the tick-time enqueue, not the
    // hardcoded default of 5. Schedule a hair in the past so it is unambiguously due
    // regardless of sub-second host/DB clock skew (this test checks opt propagation,
    // not the zero-buffer now() firing path; `at_fires_a_due_one_shot` covers that).
    let past = SystemTime::now() - Duration::from_secs(2);
    forge
        .schedule()
        .at(
            past,
            "oneshot",
            Bytes::from_static(b"x"),
            ScheduleOpts::new().with_max_attempts(1),
        )
        .await
        .unwrap();
    assert_eq!(forge.run_scheduler_once().await.unwrap(), 1);

    let job = forge
        .queue()
        .dequeue("oneshot", DequeueOpts::new().with_wait(Duration::ZERO))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        job.max_attempts, 1,
        "scheduled job carries the chosen max_attempts"
    );
    forge.queue().ack(&job).await.unwrap();

    // An unset opt still inherits the queue default (5).
    forge
        .schedule()
        .at(
            SystemTime::now() - Duration::from_secs(2),
            "defaulted",
            Bytes::from_static(b"y"),
            ScheduleOpts::new(),
        )
        .await
        .unwrap();
    assert_eq!(forge.run_scheduler_once().await.unwrap(), 1);
    let job = forge
        .queue()
        .dequeue("defaulted", DequeueOpts::new().with_wait(Duration::ZERO))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(job.max_attempts, 5, "unset opts inherit the queue default");
    forge.queue().ack(&job).await.unwrap();
}

#[tokio::test]
async fn list_paginates_by_cursor() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let s = forge.schedule();

    for name in ["a", "b", "c"] {
        s.cron(
            name,
            "0 0 * * *",
            "q",
            Bytes::from_static(b"p"),
            ScheduleOpts::new(),
        )
        .await
        .unwrap();
    }

    let (page1, cur1) = s.list(None, 2).await.unwrap();
    let names1: Vec<&str> = page1.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(names1, vec!["a", "b"]);
    let cur1 = cur1.expect("a full page yields a next cursor");

    let (page2, cur2) = s.list(Some(cur1), 2).await.unwrap();
    let names2: Vec<&str> = page2.iter().map(|s| s.name.as_str()).collect();
    assert_eq!(
        names2,
        vec!["c"],
        "second page holds the remaining schedule"
    );
    assert!(cur2.is_none(), "a short page ends iteration");
}

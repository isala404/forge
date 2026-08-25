#![cfg(feature = "pg-tests")]
#![allow(clippy::unwrap_used, clippy::panic)]

use forgelib::testing::TestDatabase;
use forgelib::{Algo, ForgeError, Limit};
use std::time::Duration;

fn assert_code<T>(result: Result<T, ForgeError>, expected: &str) {
    match result {
        Err(error) => assert_eq!(error.code(), expected),
        Ok(_) => panic!("expected {expected} error"),
    }
}

#[tokio::test]
async fn token_bucket_admits_up_to_max_then_denies() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let rl = forge.ratelimit();
    let limit = Limit::per_duration(3, Duration::from_secs(3600));

    for i in 0..3 {
        let d = rl.check("api", "user-1", limit).await.unwrap();
        assert!(d.allowed, "call {i} should be allowed");
        assert_eq!(d.limit, 3);
    }
    let denied = rl.check("api", "user-1", limit).await.unwrap();
    assert!(!denied.allowed, "denied is Ok, not Err");
    assert_eq!(denied.remaining, 0);
    assert!(denied.retry_after.is_some());

    // A different subject has an independent bucket.
    assert!(rl.check("api", "user-2", limit).await.unwrap().allowed);
}

#[tokio::test]
async fn weighted_reservations_commit_and_release_atomically() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let rl = forge.ratelimit();
    let limit = Limit::per_duration(10, Duration::from_secs(3600));
    assert_eq!(
        rl.check_cost("tokens", "tenant", limit, 4)
            .await
            .unwrap()
            .remaining,
        6
    );
    let reservation = rl
        .reserve("tokens", "tenant", limit, 5, Duration::from_secs(30))
        .await
        .unwrap()
        .unwrap();
    let committed = rl.commit(reservation.id, 2).await.unwrap();
    assert_eq!(committed.committed_units, Some(2));
    assert_eq!(rl.commit(reservation.id, 2).await.unwrap(), committed);
    assert_code(rl.release(reservation.id).await, "PRECONDITION");
    assert_eq!(
        rl.check_cost("tokens", "tenant", limit, 4)
            .await
            .unwrap()
            .remaining,
        0
    );
}

#[tokio::test]
async fn invalid_limits_and_keys_error() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let rl = forge.ratelimit();
    let ok = Limit::per_duration(5, Duration::from_secs(1));

    assert_code(
        rl.check("api", "u", Limit::per_duration(0, Duration::from_secs(1)))
            .await,
        "INVALID",
    );
    assert_code(
        rl.check("api", "u", Limit::per_duration(5, Duration::ZERO))
            .await,
        "INVALID",
    );
    assert_code(rl.check("", "u", ok).await, "INVALID");
    assert_code(rl.check("api", "", ok).await, "INVALID");
}

#[tokio::test]
async fn sliding_window_caps_within_a_window() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    let rl = forge.ratelimit();
    let limit = Limit::per_duration(2, Duration::from_secs(3600)).with_algo(Algo::SlidingWindow);

    assert!(rl.check("sw", "u", limit).await.unwrap().allowed);
    assert!(rl.check("sw", "u", limit).await.unwrap().allowed);
    assert!(!rl.check("sw", "u", limit).await.unwrap().allowed);
}

#[tokio::test]
async fn concurrent_checks_never_oversubscribe() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();
    // Long period so refill during the test is negligible: exactly `max` admits.
    let limit = Limit::per_duration(10, Duration::from_secs(3600));

    let mut handles = Vec::new();
    for _ in 0..50 {
        let f = forge.clone();
        handles.push(tokio::spawn(async move {
            f.ratelimit()
                .check("burst", "subject", limit)
                .await
                .unwrap()
                .allowed
        }));
    }
    let mut allowed = 0;
    for h in handles {
        if h.await.unwrap() {
            allowed += 1;
        }
    }
    assert_eq!(
        allowed, 10,
        "exactly max admitted under concurrency, no double-spend"
    );
}

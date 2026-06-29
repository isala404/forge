#![cfg(feature = "pg-tests")]
#![allow(clippy::unwrap_used, clippy::panic)]

use bytes::Bytes;
use forgelib::testing::TestDatabase;
use futures_util::StreamExt;
use std::time::Duration;

async fn recv(sub: &mut forgelib::Subscription) -> Option<Bytes> {
    tokio::time::timeout(Duration::from_secs(5), sub.next())
        .await
        .expect("timed out waiting for a pubsub message")
        .map(|r| r.expect("subscription yielded an error"))
}

#[tokio::test]
async fn publish_reaches_a_live_subscriber() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();

    let mut sub = forge.pubsub().subscribe("chat:1").await.unwrap();
    forge
        .pubsub()
        .publish("chat:1", Bytes::from_static(b"hello"))
        .await
        .unwrap();

    assert_eq!(recv(&mut sub).await, Some(Bytes::from_static(b"hello")));
}

#[tokio::test]
async fn topics_are_isolated() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();

    let mut chat1 = forge.pubsub().subscribe("chat:1").await.unwrap();
    let mut chat2 = forge.pubsub().subscribe("chat:2").await.unwrap();

    forge
        .pubsub()
        .publish("chat:2", Bytes::from_static(b"for-two"))
        .await
        .unwrap();

    assert_eq!(recv(&mut chat2).await, Some(Bytes::from_static(b"for-two")));
    let nothing = tokio::time::timeout(Duration::from_millis(500), chat1.next()).await;
    assert!(nothing.is_err(), "chat:1 must not receive chat:2's message");
}

#[tokio::test]
async fn message_published_before_subscribe_is_not_delivered() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();

    // No subscriber yet. Fire-and-forget publish must still succeed.
    forge
        .pubsub()
        .publish("room", Bytes::from_static(b"missed"))
        .await
        .unwrap();

    let mut sub = forge.pubsub().subscribe("room").await.unwrap();
    let nothing = tokio::time::timeout(Duration::from_millis(500), sub.next()).await;
    assert!(
        nothing.is_err(),
        "a connected-only subscriber must not see messages published before it subscribed"
    );
}

#[tokio::test]
async fn fans_out_to_every_subscriber_on_one_topic() {
    // The multiplexed broker shares one LISTEN connection; a publish must still reach
    // every independent subscriber of the same topic.
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();

    let mut a = forge.pubsub().subscribe("chat:7").await.unwrap();
    let mut b = forge.pubsub().subscribe("chat:7").await.unwrap();
    forge
        .pubsub()
        .publish("chat:7", Bytes::from_static(b"broadcast"))
        .await
        .unwrap();

    assert_eq!(recv(&mut a).await, Some(Bytes::from_static(b"broadcast")));
    assert_eq!(recv(&mut b).await, Some(Bytes::from_static(b"broadcast")));
}

#[tokio::test]
async fn oversized_payload_is_limit_and_bad_utf8_is_invalid() {
    let db = TestDatabase::new().await.unwrap();
    let forge = db.forge().await.unwrap();

    let big = Bytes::from(vec![b'a'; forgelib::pubsub::MAX_PAYLOAD_BYTES + 1]);
    assert!(matches!(
        forge.pubsub().publish("t", big).await,
        Err(forgelib::ForgeError::Limit(_))
    ));

    let bad = Bytes::from_static(&[0xff, 0xfe]);
    assert!(matches!(
        forge.pubsub().publish("t", bad).await,
        Err(forgelib::ForgeError::Invalid(_))
    ));
}
